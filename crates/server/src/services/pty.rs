//! PTY 服务：open/attach/回放/resize/close/枚举/cwd 跟随。
use crate::state::{PtySession, RING_MAX, ServerState};
use crate::setup::user_map;

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;

use anyhow::{Context, Result};
use easytidy_protocol::{
    Frame, Message, MsgKind, PtyClose, PtyCwd, PtyCwdResp,
    PtyListResp, PtyOpen, PtyOpenResp, PtyResize, PtyTerminalInfo, RpcError,
};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use serde_json::json;
use tokio::sync::{mpsc, RwLock};
use tracing::{error, info, warn};

/// 把连接订阅到已有 PTY 会话：按请求尺寸同步 PTY + 清屏回放环形缓冲 + 登记订阅。
/// 返回 None = 会话不存在（调用方回退新建路径）。
///
/// ⚠️ 回放前缀"清屏 + 光标复位"：ring 是从字节流中间截取的片段（128KB
/// 裁剪/半截 ESC 序列），直接回放会让 xterm 从错误状态开始渲染 → 光标漂移、
/// 提示符残缺错位（实测乱码）。合并**单帧**发送：分块回放导致 xterm 逐块
/// 渲染 → 切回 tab 时"先少量文字再迅速补齐"的闪烁（实测）。DECSET 2026
/// （同步输出）包裹整帧：xterm 6.0 原生将整帧原子渲染，清除回放过程的
/// 逐块重绘闪烁（社区标准做法，调研 2026-08-07）。
pub(crate) async fn attach_to_session(
    state: &Arc<ServerState>,
    stream_id: u32,
    cols: u16,
    rows: u16,
    event_tx: &mpsc::UnboundedSender<Frame>,
    conn_token: u64,
    msg_id: u64,
) -> Result<Option<Frame>> {
    let sessions = state.sessions.read().await;
    let Some(session) = sessions.get(&stream_id) else {
        eprintln!("[SRV-DBG] attach_to_session: sid={} conn_token={} → SESSION NOT FOUND", stream_id, conn_token);
        return Ok(None);
    };
    eprintln!(
        "[SRV-DBG] attach_to_session: sid={} conn_token={} subs_before={}",
        stream_id,
        conn_token,
        session.subs.lock().unwrap().len()
    );

    // 用请求尺寸立即同步 PTY：attach 客户端尺寸可能与旧会话不同，
    // 不 resize 则 shell 按旧行列换行 → 提示符截断错位（实测）
    {
        let master = session.master.lock().unwrap();
        let _ = master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
    }

    // 回放环形缓冲（恢复当前屏幕）→ 订阅
    {
        const SYNC_START: &[u8] = b"\x1b[?2026h";
        const SYNC_END: &[u8] = b"\x1b[?2026l";
        let prefix = b"\x1b[2J\x1b[H";
        let ring = session.ring.lock().unwrap();
        let mut pending: Vec<u8> =
            Vec::with_capacity(SYNC_START.len() + prefix.len() + ring.len() + SYNC_END.len());
        pending.extend_from_slice(SYNC_START);
        pending.extend_from_slice(prefix);
        pending.extend(ring.iter().copied());
        pending.extend_from_slice(SYNC_END);
        // ring 不清空：多客户端 attach 各自从"清屏 + 全量 ring"渲染，
        // 渲染幂等；清空会破坏后续 attach 的回放
        drop(ring);
        let _ = event_tx.send(Frame::Raw {
            stream_id,
            data: pending,
        });
    }
    session.subs.lock().unwrap().push((conn_token, (*event_tx).clone()));
    info!("PTY attach: stream_id={}", stream_id);
    Ok(Some(Frame::Json(Message {
        id: msg_id,
        kind: MsgKind::Resp,
        op: "pty.open".to_string(),
        payload: serde_json::to_value(PtyOpenResp { stream_id })?,
        err: None,
    })))
}

/// Handle PTY open
pub(crate) async fn handle_pty_open(
    msg: Message,
    state: &Arc<ServerState>,
    event_tx: mpsc::UnboundedSender<Frame>,
    conn_token: u64,
) -> Result<Frame> {

    info!("div: handle_pty_open enter {}", conn_token);

    let req: PtyOpen = serde_json::from_value(msg.payload)
        .context("Failed to parse PtyOpen")?;

    // 接线常驻终端：server 按身份各持一个 attach 终端（persistent 会话，
    // 不随连接断开清理；key 恒 "user"（新模型 server 即容器默认用户，无
    // root 会话——root 通道走宿主 easytidy-root-channel）。
    // 已有 → 订阅 + 回放环形缓冲 → 复用同一 stream_id；无 → 走新建路径并登记。
    // ⚠️ guard 先取值再 await：std RwLock guard 在 if-let scrutinee 中存活
    // 整个语句，跨 await 导致 future 非 Send
    let attach_key = "user";
    let default_sid = state
        .default_terminal
        .read()
        .unwrap()
        .get(attach_key)
        .copied();
    // 多终端：按 stream_id 附接已有会话（GUI 重开窗口恢复面板）
    if let Some(sid) = req.attach_stream {
        if let Some(resp) =
            attach_to_session(state, sid, req.cols, req.rows, &event_tx, conn_token, msg.id).await?
        {
            return Ok(resp);
        }
        info!("attach_stream {} 不存在，回退新建路径", sid);
    }
    // 单终端 attach 语义：复用身份默认常驻会话
    if req.attach {
        if let Some(default_sid) = default_sid {
            if let Some(resp) = attach_to_session(
                state,
                default_sid,
                req.cols,
                req.rows,
                &event_tx,
                conn_token,
                msg.id,
            )
            .await?
            {
                return Ok(resp);
            }
        }
    }

    let pty_system = native_pty_system();
    let pty_size = PtySize {
        rows: req.rows,
        cols: req.cols,
        pixel_width: 0,
        pixel_height: 0,
    };

    let pty_pair = pty_system
        .openpty(pty_size)
        .context("Failed to open PTY")?;

    // ⚠️ 模型（2026-08-27 定案）：终端不指定用户——server 进程本身即容器
    // 默认用户（容器 User 字段 = 配置 uid:gid，宿主侧 create_with_config），
    // 直接 exec 命令、继承 server 身份与环境；登录 env（HOME/USER 等）由
    // 下方显式覆盖（客户端 env 继承自宿主进程）。root 终端不走此通道
    // （协议 v2 删 as_root，root 走宿主 easytidy-root-channel）。
    let (cmd, argv) = if req.cmd.is_empty() {
        // 默认终端 = 登录 shell（-l 读 /etc/profile，HOME 由下方 env 注入）
        let shell = if Path::new("/bin/bash").exists() { "/bin/bash" } else { "/bin/sh" };
        (shell.to_string(), vec![shell.to_string(), "-l".to_string()])
    } else {
        (
            req.cmd.clone(),
            if req.argv.is_empty() {
                vec![req.cmd.clone()]
            } else {
                req.argv.clone()
            },
        )
    };

    let mut cmd_builder = CommandBuilder::new(&cmd);
    for arg in &argv[1..] {
        cmd_builder.arg(arg);
    }

    // Set environment variables。XDG_DATA_DIRS 必须含系统默认目录（旧 flavor 注入
    // 纯覆盖值导致 gdk-pixbuf 找不到系统 loaders.cache，PNG 图标解码失败、GTK
    // 断言崩溃——Chrome 保存图片实测），此处对固化 env 做防御性修正。
    for (k, v) in &req.env {
        if k == "XDG_DATA_DIRS" {
            cmd_builder.env(k, easytidy_core::incontainer::fixup_xdg_data_dirs_value(v));
        } else {
            cmd_builder.env(k, v);
        }
    }
    // 登录语义 env 后置覆盖：CLI/GUI 客户端 env 继承自宿主进程（HOME=宿主
    // home、USER=宿主用户名）——在客户端 env 之后显式注入容器默认用户的
    // 登录环境（新模型下恒注入：server 即容器默认用户）
    if let Some(user) = user_map() {
        cmd_builder.env("HOME", &user.home);
        cmd_builder.env("USER", &user.name);
        cmd_builder.env("LOGNAME", &user.name);
        cmd_builder.env("SHELL", if Path::new("/bin/bash").exists() { "/bin/bash" } else { "/bin/sh" });
    }
    // TERM 注入：交互 shell 必需（clear 等依赖），客户端 env 未必携带
    // （实测 "TERM environment variable not set"）
    if !req.env.contains_key("TERM") {
        cmd_builder.env("TERM", "xterm-256color");
    }

    // Set working directory：空或 "/" = 落用户 home（登录 shell 不会自行
    // chdir home——GUI 恒传 "/"，直接以 "/" 为 cwd 会让 shell 停在根目录）。
    // home 可能暂缺（prepare_container 补齐是 best-effort、创建后窗口期开
    // 终端）→ 非目录时回退 "/"，不阻断开终端
    let cwd = if req.cwd.is_empty() || req.cwd == "/" {
        user_map().map(|u| u.home.clone()).unwrap_or_default()
    } else {
        req.cwd.clone()
    };
    let cwd = if std::path::Path::new(&cwd).is_dir() {
        cwd
    } else {
        "/".to_string()
    };
    cmd_builder.cwd(&cwd);

    let slave = pty_pair.slave;
    let master = pty_pair.master;

    // Take writer BEFORE we wrap master in Arc<Mutex<>>
    let writer = master.take_writer()
        .context("Failed to take PTY writer")?;

    // Clone reader for the reader thread
    let reader = master.try_clone_reader()
        .context("Failed to clone PTY reader")?;

    // Wrap master in Arc<Mutex<>> for resize operations
    let master = Arc::new(std::sync::Mutex::new(master));

    // Spawn the command - child is moved to reader thread
    let child = slave
        .spawn_command(cmd_builder)
        .context("Failed to spawn PTY child")?;
    let spawn_pid = child.process_id().unwrap_or(0);

    // Allocate stream ID
    let stream_id = state.next_stream_id.fetch_add(1, Ordering::SeqCst);

    // 常驻会话：attach（登记为身份默认终端，供复用）或 persistent（多终端
    // 实例，独立会话不登记——GUI 经 pty.list/attach_stream 恢复）
    let persistent = req.attach || req.persistent;
    if req.attach {
        let mut def = state.default_terminal.write().unwrap();
        def.insert(attach_key.to_string(), stream_id);
        info!("常驻终端已登记：{attach_key} → stream_id={stream_id}");
    } else if req.persistent {
        info!("多终端会话已创建：stream_id={stream_id}");
    }

    // Create session
    let session = Arc::new(PtySession {
        writer: Arc::new(std::sync::Mutex::new(writer)),
        master: master.clone(),
        ring: Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new())),
        subs: Arc::new(std::sync::Mutex::new(vec![(conn_token, event_tx.clone())])),
        persistent: std::sync::atomic::AtomicBool::new(persistent),
        spawn_pid,
        last_cwd: std::sync::Mutex::new(None),
        cmd: req.cmd.clone(),
    });

    // Store session
    {
        let mut sessions = state.sessions.write().await;
        sessions.insert(stream_id, session);
    }

    // Drop the slave after spawn (else master never sees EOF)
    drop(slave);

    info!(
        "[SRV-DBG] new PTY: sid={} persistent={} cmd={:?}",
        stream_id, persistent, req.cmd
    );
    info!("PTY opened: stream_id={}, cmd={}", stream_id, req.cmd);

    // Spawn PTY reader thread (owns the child for reaping)
    let msg_id = state.next_msg_id.fetch_add(1, Ordering::SeqCst) as u64;
    let stream_id_copy = stream_id;
    let session_for_reader = {
        let sessions = state.sessions.read().await;
        sessions.get(&stream_id).cloned().unwrap()
    };
    let default_terminal = state.default_terminal.clone();
    let sessions_map = state.sessions.clone();
    thread::spawn(move || {
        pty_reader_thread(
            stream_id_copy,
            reader,
            child,
            msg_id,
            session_for_reader,
            sessions_map,
            default_terminal,
        );
    });

    Ok(Frame::Json(Message {
        id: msg.id,
        kind: MsgKind::Resp,
        op: "pty.open".to_string(),
        payload: serde_json::to_value(PtyOpenResp { stream_id })?,
        err: None,
    }))
}

/// PTY reader thread (runs in a separate thread because PTY I/O is synchronous)
///
/// 输出路径：写入环形缓冲（新 attach 客户端回放）+ 广播所有订阅连接
/// （send 失败 = 连接断开 → 退订该连接；常驻会话保留继续缓冲）。
pub(crate) fn pty_reader_thread(
    stream_id: u32,
    reader: Box<dyn std::io::Read + Send>,
    mut child: Box<dyn portable_pty::Child + Send>,
    msg_id: u64,
    session: Arc<PtySession>,
    sessions: Arc<RwLock<HashMap<u32, Arc<PtySession>>>>,
    default_terminal: Arc<std::sync::RwLock<HashMap<String, u32>>>,
) {
    eprintln!(
        "[SRV-DBG] reader thread START: sid={} subs={}",
        stream_id,
        session.subs.lock().unwrap().len()
    );
    info!("PTY reader thread started: stream_id={}", stream_id);

    let mut reader = std::io::BufReader::new(reader);
    let mut buf = [0u8; 8192];
    let mut total_bytes: u64 = 0;
    let mut total_frames: u64 = 0;

    loop {
        match reader.read(&mut buf) {
            Ok(0) => {
                eprintln!(
                    "[SRV-DBG] reader EOF: sid={} total_bytes={} total_frames={}",
                    stream_id, total_bytes, total_frames
                );
                info!("PTY EOF: stream_id={}", stream_id);
                break;
            }
            Ok(n) => {
                total_bytes += n as u64;
                total_frames += 1;
                if total_frames % 32 == 1 {
                    eprintln!(
                        "[SRV-DBG] reader read: sid={} bytes={} cumulative_bytes={} frames={}",
                        n, n, total_bytes, total_frames
                    );
                }
                let data = buf[..n].to_vec();

                // 环形缓冲（回放；上限裁剪）
                {
                    let mut ring = session.ring.lock().unwrap();
                    ring.extend(&data);
                    while ring.len() > RING_MAX {
                        ring.pop_front();
                    }
                }

                // 广播订阅者（send 失败 = 连接断开 → 退订）
                let frame = Frame::Raw {
                    stream_id,
                    data,
                };
                let mut subs = session.subs.lock().unwrap();
                let subs_count = subs.len();
                let mut sent_ok = 0usize;
                let mut sent_fail = 0usize;
                let mut fail_tokens: Vec<u64> = Vec::new();
                for (tok, tx) in subs.iter() {
                    if tx.send(frame.clone()).is_ok() {
                        sent_ok += 1;
                    } else {
                        sent_fail += 1;
                        fail_tokens.push(*tok);
                    }
                }
                if sent_fail > 0 || total_frames % 32 == 1 {
                    eprintln!(
                        "[SRV-DBG] broadcast: sid={} subs={} sent_ok={} sent_fail={} fail_tokens={:?}",
                        stream_id, subs_count, sent_ok, sent_fail, fail_tokens
                    );
                }
                // 移除失败订阅（语义同 retain）
                subs.retain(|(tok, _)| !fail_tokens.contains(tok));
            }
            Err(e) => {
                error!("PTY read error: stream_id={}, {}", stream_id, e);
                break;
            }
        }
    }

    // Try to reap child to get exit code
    // Note: portable_pty::ExitStatus doesn't expose code() method directly
    // For v0, we use success=0, error=-1
    // su -c 场景存在"子命令退出 → master EOF → su 尚未退出"的竞态：EOF 后先
    // 短暂等待子进程自然退出（否则 kill 会吞掉输出/退出码，实测快速命令丢输出）
    let exit_code = match child.try_wait() {
        Ok(Some(status)) => {
            if status.success() { 0 } else { -1 }
        }
        Ok(None) => {
            // EOF 后给子进程一个自然退出的宽限窗口
            let mut grace = std::time::Duration::from_millis(300);
            let exit = loop {
                match child.try_wait() {
                    Ok(Some(status)) => break Some(status),
                    Ok(None) => {
                        if grace.is_zero() {
                            break None;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(50));
                        grace = grace.saturating_sub(std::time::Duration::from_millis(50));
                    }
                    Err(e) => {
                        error!("Failed to wait for PTY child: {}", e);
                        break None;
                    }
                }
            };
            match exit {
                Some(status) => {
                    if status.success() { 0 } else { -1 }
                }
                None => {
                    info!("PTY child still running at EOF, killing");
                    let _ = child.kill();
                    match child.wait() {
                        Ok(status) => {
                            if status.success() { 0 } else { -1 }
                        }
                        Err(_) => -1,
                    }
                }
            }
        }
        Err(e) => {
            error!("Failed to wait for PTY child: {}", e);
            -1
        }
    };

    // Send pty.exited event to all subscribers（连接侧 GUI/CLI 据此显示退出）
    let evt = Frame::Json(Message {
        id: msg_id,
        kind: MsgKind::Evt,
        op: "pty.exited".to_string(),
        payload: serde_json::json!({
            "stream_id": stream_id,
            "code": exit_code,
        }),
        err: None,
    });
    {
        let mut subs = session.subs.lock().unwrap();
        for (_, tx) in subs.iter() {
            let _ = tx.send(evt.clone());
        }
        subs.clear();
    }

    // 死亡同步清理（server 全权负责生命周期）：
    // 1) sessions 移除 → pty.list 不再列出、attach 找不到 → 客户端
    //    attach 已死终端时 handle_pty_open 回退新建，前端拿到新 stream_id
    //    （blocking_write：本线程为同步上下文，reader I/O 不跨 async）
    {
        let mut sessions = sessions.blocking_write();
        sessions.remove(&stream_id);
    }
    // 2) 常驻终端登记清除（用户 exit/进程结束）→ 下次 attach 新建
    {
        let mut def = default_terminal.write().unwrap();
        if let Some((key, _)) = def.iter().find(|(_, v)| **v == stream_id) {
            let key = key.clone();
            def.remove(&key);
            info!("常驻终端已退出并清除登记：{key} → stream_id={stream_id}");
        }
    }

    info!("PTY reader thread ended: stream_id={}, exit_code={}", stream_id, exit_code);
}

/// 读取进程 cwd：直接 readlink（新模型下 server 与子进程同 uid，
/// 无 ptrace 权限问题——旧 root 模型的 `su node` 兜底已无意义，删除）。
pub(crate) fn read_cwd(pid: u32) -> Option<String> {
    std::fs::read_link(format!("/proc/{pid}/cwd"))
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

/// Handle pty.cwd：查询会话主进程的实时工作目录。
///
/// spawn 的可能是 su（su - node 场景），其子进程（bash）才是 shell——
/// 列出所有活跃 PTY 会话（GUI get_terminals：多终端面板恢复）。
///
/// 返回全部会话（含 CLI 临时会话）——GUI 按需 attach；附 cmd/as_root/
/// persistent 供标签展示。
pub(crate) async fn handle_pty_list(msg: Message, state: &Arc<ServerState>) -> Result<Frame> {
    let sessions = state.sessions.read().await;
    let terminals: Vec<PtyTerminalInfo> = sessions
        .iter()
        .map(|(sid, s)| PtyTerminalInfo {
            stream_id: *sid,
            cmd: s.cmd.clone(),
            persistent: s.persistent.load(std::sync::atomic::Ordering::SeqCst),
            cwd: s.last_cwd.lock().unwrap().clone(),
        })
        .collect();
    Ok(Frame::Json(Message {
        id: msg.id,
        kind: MsgKind::Resp,
        op: "pty.list".to_string(),
        payload: serde_json::to_value(PtyListResp { terminals })?,
        err: None,
    }))
}

/// 先经 /proc/<pid>/task/<pid>/children 取第一个子进程，再读其
/// /proc/<child-pid>/cwd（symlink，实时反映 cd 结果）；无子进程（root
/// 终端直接 spawn bash）则直接读 spawn pid 的 cwd。
pub(crate) async fn handle_pty_cwd(msg: Message, state: &Arc<ServerState>) -> Result<Frame> {
    let req: PtyCwd = serde_json::from_value(msg.payload)
        .context("Failed to parse PtyCwd")?;

    let sessions = state.sessions.read().await;
    let Some(session) = sessions.get(&req.stream_id) else {
        return Ok(Frame::Json(Message {
            id: msg.id,
            kind: MsgKind::Resp,
            op: "pty.cwd".to_string(),
            payload: json!(null),
            err: Some(RpcError {
                code: "not_found".to_string(),
                message: format!("PTY stream {} not found", req.stream_id),
            }),
        }));
    };
    // 复用 session_cwd（su 场景取子进程 bash；ptrace 拒绝时经 su node）
    let cwd = session_cwd(session).await.unwrap_or_default();

    Ok(Frame::Json(Message {
        id: msg.id,
        kind: MsgKind::Resp,
        op: "pty.cwd".to_string(),
        payload: serde_json::to_value(PtyCwdResp { cwd })?,
        err: None,
    }))
}

/// Handle PTY resize
pub(crate) async fn handle_pty_resize(
    msg: Message,
    state: &Arc<ServerState>,
) -> Result<Frame> {
    let req: PtyResize = serde_json::from_value(msg.payload)
        .context("Failed to parse PtyResize")?;

    let sessions = state.sessions.read().await;
    if let Some(session) = sessions.get(&req.stream_id) {
        let master = session.master.lock().unwrap();
        master.resize(PtySize {
            rows: req.rows,
            cols: req.cols,
            pixel_width: 0,
            pixel_height: 0,
        }).context("Failed to resize PTY")?;

        Ok(Frame::Json(Message {
            id: msg.id,
            kind: MsgKind::Resp,
            op: "pty.resize".to_string(),
            payload: json!(null),
            err: None,
        }))
    } else {
        Ok(Frame::Json(Message {
            id: msg.id,
            kind: MsgKind::Resp,
            op: "pty.resize".to_string(),
            payload: json!(null),
            err: Some(RpcError {
                code: "not_found".to_string(),
                message: format!("PTY stream {} not found", req.stream_id),
            }),
        }))
    }
}

/// Handle PTY close
pub(crate) async fn handle_pty_close(
    msg: Message,
    state: &Arc<ServerState>,
) -> Result<Frame> {
    let req: PtyClose = serde_json::from_value(msg.payload)
        .context("Failed to parse PtyClose")?;

    // 显式关闭（pty.close）：常驻终端也一并终结并清除登记
    {
        let mut def = state.default_terminal.write().unwrap();
        if let Some((key, _)) = def.iter().find(|(_, v)| **v == req.stream_id) {
            let key = key.clone();
            def.remove(&key);
            info!("常驻终端已显式关闭并清除登记：{key} → stream_id={}", req.stream_id);
        }
    }

    let mut sessions = state.sessions.write().await;
    if let Some(session) = sessions.remove(&req.stream_id) {
        // ⚠️ 仅 remove 不够：reader 线程持有 Arc<PtySession>，session/writer 不会
        // drop → bash stdin 收不到 EOF → 进程残留。必须主动关写侧（换 sink），
        // bash 退出后 reader 收 EOF 再回收子进程。
        session.shutdown_writer();

        Ok(Frame::Json(Message {
            id: msg.id,
            kind: MsgKind::Resp,
            op: "pty.close".to_string(),
            payload: json!(null),
            err: None,
        }))
    } else {
        Ok(Frame::Json(Message {
            id: msg.id,
            kind: MsgKind::Resp,
            op: "pty.close".to_string(),
            payload: json!(null),
            err: Some(RpcError {
                code: "not_found".to_string(),
                message: format!("PTY stream {} not found", req.stream_id),
            }),
        }))
    }
}

/// Handle raw data from client (write to PTY)
pub(crate) async fn handle_raw_data(
    stream_id: u32,
    data: &[u8],
    state: &Arc<ServerState>,
) -> Result<()> {
    let sessions = state.sessions.read().await;
    if let Some(session) = sessions.get(&stream_id) {
        let writer = session.writer.clone();
        // TTY 事件检测（用户在终端敲回车执行命令）：输入含换行即"TTY 事件"
        let has_enter = data.contains(&b'\n') || data.contains(&b'\r');
        let data = data.to_vec();

        // Write in spawn_blocking to avoid blocking async runtime
        tokio::task::spawn_blocking(move || {
            let mut writer_guard = writer.lock().unwrap();
            if let Err(e) = writer_guard.write_all(&data) {
                error!("Failed to write to PTY writer: {}", e);
            }
            let _ = writer_guard.flush();
        }).await
        .context("spawn_blocking join error")?;

        // TTY 事件驱动 cwd 检测（"高人方案"机制三：TTY 事件触发 + /proc 读取）。
        // server 持有 PTY master，用户敲回车（执行命令）的输入经此转发——
        // 此时读一次 bash cwd，变化则广播 pty.cwdChanged（主动推送，
        // 毫秒级响应，替代 GUI 轮询）。
        if has_enter {
            if let Some(cwd) = session_cwd(session).await {
                let mut guard = session.last_cwd.lock().unwrap();
                if *guard != Some(cwd.clone()) {
                    *guard = Some(cwd.clone());
                    info!("cwd 变化：stream_id={stream_id} → {cwd}");
                    // 广播给订阅连接（GUI 的 pty reader 消费）
                    let evt = Frame::Json(Message {
                        id: state.next_msg_id.fetch_add(1, Ordering::SeqCst) as u64,
                        kind: MsgKind::Evt,
                        op: "pty.cwdChanged".to_string(),
                        payload: serde_json::json!({
                            "stream_id": stream_id,
                            "cwd": cwd,
                        }),
                        err: None,
                    });
                    let mut subs = session.subs.lock().unwrap();
                    subs.retain(|(_, tx)| tx.send(evt.clone()).is_ok());
                }
            }
        }
    } else {
        warn!("PTY session {} not found for write", stream_id);
    }
    Ok(())
}

/// 读取会话主进程的实时 cwd（su 场景取子进程 bash；ptrace 拒绝时经 su node）
pub(crate) async fn session_cwd(session: &Arc<PtySession>) -> Option<String> {
    let spawn_pid = session.spawn_pid;
    let children_path = format!("/proc/{spawn_pid}/task/{spawn_pid}/children");
    let child_pid = std::fs::read_to_string(children_path)
        .ok()
        .and_then(|c| c.split_whitespace().next().map(|s| s.parse::<u32>().unwrap_or(0)));
    match child_pid {
        Some(pid) => read_cwd(pid),
        None => read_cwd(spawn_pid),
    }
}
