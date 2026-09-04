//! easytidy-dock daemon 模式（容器内常驻 root 服务）。
//!
//! ## 职责
//! 容器内长驻进程：
//! - bind unix socket `/run/easytidy/dock.sock`
//! - 接受 client 连接（多个客户端可同时 attach 同一 session）
//! - 管理 0..N 个 session（每个 = 一个容器内 root bash + PTY）
//! - 转发 client 的 stdin 到 bash、bash 输出 fan-out 到所有 client
//! - 监听 SIGCHLD / SIGTERM / SIGINT，bash 退出时广播 `rc.exited`
//!
//! ## 客户端协议
//! hello → (rc.ping | rc.new | rc.attach | rc.list)
//! hello 后是 rc.ping：探测存活（不订阅），发完即断
//! hello 后是 rc.new：建新 session，回 session_id + 桥流
//! hello 后是 rc.attach：attach 到指定 session，回 session_id + 桥流（session 不存在 → err）
//! hello 后是 rc.list：列 sessions，回 JSON（不订阅），发完即断

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::Context;
use futures::{SinkExt, StreamExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::oneshot;
use tokio_util::codec::Framed;
use tracing::{debug, error, info, warn};

use easytidy_protocol::frame::FrameCodec;
use easytidy_protocol::rc::{
    RcAttach, RcAttachAck, RcCloseReq, RcListResp, RcNew, RcNewAck, RcPing, RcPingResp,
    RcResize, SessionInfo, ROOT_STREAM_ID,
};
use easytidy_protocol::{Frame, Handshake, HandshakeAck, Message, MsgKind, PROTOCOL_VERSION, RpcError};

use crate::session::{spawn_root_shell, RootSession};

/// daemon 内部 socket 路径（容器内绝对路径——daemon 跑在容器里）
pub const DAEMON_SOCKET: &str = "/run/easytidy/dock.sock";

/// session map 类型（session_id → session）
type SessionMap = Arc<tokio::sync::RwLock<HashMap<u64, Arc<RootSession>>>>;

/// 全局会话管理（daemon 单实例）
pub struct DaemonState {
    pub sessions: SessionMap,
    pub conn_id: AtomicU64,
}

impl DaemonState {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            conn_id: AtomicU64::new(1),
        }
    }
}

/// daemon 主入口
pub async fn run_daemon() -> anyhow::Result<()> {
    let state = Arc::new(DaemonState::new());

    // 清理旧 socket（防上次非正常退出残留）
    let _ = tokio::fs::remove_file(DAEMON_SOCKET).await;
    // 确保 socket 目录存在（/run/easytidy，与日志同目录）
    if let Some(parent) = std::path::Path::new(DAEMON_SOCKET).parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    let listener = UnixListener::bind(DAEMON_SOCKET)
        .with_context(|| format!("bind {} failed", DAEMON_SOCKET))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = tokio::fs::set_permissions(
            DAEMON_SOCKET,
            std::fs::Permissions::from_mode(0o600),
        )
        .await;
    }
    info!("easytidy-dock daemon ready at {}", DAEMON_SOCKET);

    // 信号
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut sigchld = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::child())?;

    // 启动会话清理任务（SIGCHLD 触发时：waitpid 收割 zombie + 清理已死 session）
    let cleanup_state = state.clone();
    let cleanup_task = tokio::spawn(async move {
        loop {
            sigchld.recv().await;
            reap_zombie_children();
            reap_dead_sessions(&cleanup_state).await;
        }
    });

    let (_sd_tx, mut sd_rx) = oneshot::channel::<()>();

    loop {
        tokio::select! {
            _ = sigterm.recv() => {
                info!("SIGTERM received, shutting down");
                break;
            }
            _ = sigint.recv() => {
                info!("SIGINT received, shutting down");
                break;
            }
            _ = &mut sd_rx => {
                info!("shutdown signal received");
                break;
            }
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _)) => {
                        let token = state.conn_id.fetch_add(1, Ordering::SeqCst);
                        let s = state.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_client(stream, s, token).await {
                                warn!("client (token={}) handler failed: {e}", token);
                            }
                        });
                    }
                    Err(e) => {
                        error!("accept failed: {e}");
                        break;
                    }
                }
            }
        }
    }

    // 清理
    cleanup_task.abort();
    // 标记所有 session 死亡 + 广播
    let sessions = state.sessions.read().await;
    for s in sessions.values() {
        s.set_dead();
        s.broadcast_exited();
    }
    drop(sessions);
    let _ = tokio::fs::remove_file(DAEMON_SOCKET).await;
    info!("easytidy-dock daemon exited");
    Ok(())
}

/// waitpid(-1, WNOHANG) 收割所有 zombie 子进程（bash 由 portable-pty 直接
/// spawn，daemon 是它们的父进程——bash 死时必须 waitpid 回收，否则残留
/// `<defunct>` zombie）。循环收割直到没有更多子进程退出。
fn reap_zombie_children() {
    use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
    use nix::unistd::Pid;
    loop {
        match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) | Err(nix::errno::Errno::ECHILD) => break,
            Ok(status) => {
                debug!("reaped child: {status:?}");
            }
            Err(e) => {
                debug!("waitpid error: {e}");
                break;
            }
        }
    }
}

/// SIGCHLD 触发时清理已死的 session（bash 退出 → session.alive=false → 广播 + 删除）
async fn reap_dead_sessions(state: &DaemonState) {
    // waitpid(-1, WNOHANG) 收割所有 zombie，避免资源累积
    // 这里简化为：用 portable-pty spawn 的子进程不便直接 wait；
    // session.alive 由 reader task 在 EOF 时设为 false，我们这里清理 alive=false 的
    let mut sessions = state.sessions.write().await;
    let before = sessions.len();
    sessions.retain(|id, s| {
        if s.alive() {
            true
        } else {
            debug!("reaping dead session {}", id);
            false
        }
    });
    let after = sessions.len();
    if before != after {
        info!("reaped {} dead sessions ({} active)", before - after, after);
    }
}

/// 单客户端连接：握手 → 命令（ping/new/attach/list）→ 双向 I/O
async fn handle_client(
    stream: UnixStream,
    state: Arc<DaemonState>,
    token: u64,
) -> anyhow::Result<()> {
    let mut framed = Framed::new(stream, FrameCodec::new());

    // 握手
    let frame = next_frame_timed(&mut framed)
        .await
        .context("handshake timeout")?;
    let Frame::Json(msg) = &frame else {
        anyhow::bail!("handshake must be JSON");
    };
    if msg.op != "hello" {
        warn!("first frame is not hello (op={}), continue", msg.op);
    } else {
        let _hs: Handshake = serde_json::from_value(msg.payload.clone())?;
        let ack = HandshakeAck {
            v: PROTOCOL_VERSION,
            server: "easytidy-dock".into(),
            capabilities: vec!["root".into()],
            session_id: format!("rc-daemon-{token}"),
        };
        framed
            .send(resp(msg.id, "hello", &ack, None))
            .await
            .context("send handshake ack")?;
    }

    // 第一个 post-hello 命令决定模式
    let cmd_frame = next_frame_timed(&mut framed)
        .await
        .context("post-hello command timeout")?;
    let Frame::Json(cmd) = cmd_frame else {
        anyhow::bail!("expected JSON command");
    };

    match cmd.op.as_str() {
        "rc.ping" => {
            // 探测存活——不订阅，发完即断。**alive = daemon 在响应**（恒 true，
            // 因为能收到 ping 就说明 daemon 活着）。之前误用"是否有存活 session"
            // 当 alive——无 session 时返回 false，GUI `probe_root_channel` 以为
            // daemon 未就绪 → "启动后 1s 内未就绪"误报。session 存活看 rc.list。
            let _req: RcPing = serde_json::from_value(cmd.payload)?;
            framed
                .send(resp(cmd.id, "rc.ping", &RcPingResp { alive: true }, None))
                .await?;
            return Ok(());
        }
        "rc.list" => {
            let sessions = state.sessions.read().await;
            let list: Vec<SessionInfo> = sessions
                .values()
                .map(|s| SessionInfo {
                    id: s.id,
                    alive: s.alive(),
                    spawn_pid: s.spawn_pid,
                })
                .collect();
            framed
                .send(resp(cmd.id, "rc.list", &RcListResp { sessions: list }, None))
                .await?;
            return Ok(());
        }
        "rc.new" => {
            let req: RcNew = serde_json::from_value(cmd.payload)?;
            info!("client (token={}) requests new session {}x{}", token, req.cols, req.rows);
            let session = spawn_root_shell(req.cols, req.rows)
                .context("spawn root shell failed")?;
            let id = session.id;
            state.sessions.write().await.insert(id, session.clone());
            info!("session {} created (spawn_pid={})", id, session.spawn_pid);

            // 启动 reader task：PTY master → session.push_output；EOF → set_dead + broadcast_exited
            let reader_session = session.clone();
            tokio::task::spawn_blocking(move || {
                use std::io::Read;
                let mut reader = match reader_session.master.lock() {
                    Ok(m) => m.try_clone_reader().ok(),
                    Err(_) => None,
                };
                if let Some(mut r) = reader.take() {
                    let mut buf = [0u8; 4096];
                    loop {
                        match r.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => reader_session.push_output(&buf[..n]),
                            Err(_) => break,
                        }
                    }
                }
                reader_session.set_dead();
                reader_session.broadcast_exited();
                debug!("session {} reader task exited (EOF)", id);
            });

            framed
                .send(resp(
                    cmd.id,
                    "rc.new",
                    &RcNewAck {
                        session_id: id,
                        alive: true,
                        stream_id: ROOT_STREAM_ID,
                    },
                    None,
                ))
                .await?;
            // 进入 attach 桥流（同 attach 路径）
            return attach_session(&mut framed, state.clone(), session, token, cmd.id).await;
        }
        "rc.attach" => {
            let req: RcAttach = serde_json::from_value(cmd.payload)?;
            info!("client (token={}) attaches session {}", token, req.session_id);
            let session = {
                let sessions = state.sessions.read().await;
                sessions.get(&req.session_id).cloned()
            };
            let Some(session) = session else {
                framed
                    .send(resp_err(
                        cmd.id,
                        "rc.attach",
                        &format!("session {} not found", req.session_id),
                    ))
                    .await?;
                return Ok(());
            };
            // resize
            if let Ok(m) = session.master.lock() {
                let _ = m.resize(portable_pty::PtySize {
                    rows: req.rows,
                    cols: req.cols,
                    pixel_width: 0,
                    pixel_height: 0,
                });
            }
            framed
                .send(resp(
                    cmd.id,
                    "rc.attach",
                    &RcAttachAck {
                        stream_id: ROOT_STREAM_ID,
                        alive: session.alive(),
                    },
                    None,
                ))
                .await?;
            // 回放
            framed
                .send(Frame::Raw {
                    stream_id: ROOT_STREAM_ID,
                    data: session.replay(),
                })
                .await?;
            return attach_session(&mut framed, state.clone(), session, token, cmd.id).await;
        }
        "rc.close" => {
            // standalone 关闭指定 session（GUI 终端"关闭"按钮）：
            // kill bash → 从 map 移除 → 广播 exited → ack。
            let req: RcCloseReq = serde_json::from_value(cmd.payload)?;
            info!("client (token={}) requests close session {}", token, req.session_id);
            let removed = {
                let mut sessions = state.sessions.write().await;
                sessions.remove(&req.session_id)
            };
            match removed {
                Some(s) => {
                    s.kill();
                    s.broadcast_exited();
                    info!("session {} closed", req.session_id);
                    framed
                        .send(resp(cmd.id, "rc.close", &serde_json::Value::Null, None))
                        .await?;
                }
                None => {
                    framed
                        .send(resp_err(
                            cmd.id,
                            "rc.close",
                            &format!("session {} not found", req.session_id),
                        ))
                        .await?;
                }
            }
            Ok(())
        }
        "rc.resize" => {
            // 独立 resize（GUI xterm.fit 时经一次性 `client resize` exec
            // 触发）：找唯一 alive session → master.resize → 内核自动给
            // 前台进程组发 SIGWINCH → bash 重绘 prompt。**关键**：仅改
            // xterm.cols 而 bash 还在旧宽度 = 提示符 wrap（root 终端的
            // `~ ` 与 ` $ ` 错行显示）。attach 路径下的 `rc.resize` 由
            // `attach_session` 内的消息循环处理，不走这里。
            let req: RcResize = serde_json::from_value(cmd.payload)?;
            info!(
                "client (token={}) requests resize {}x{}",
                token, req.cols, req.rows
            );
            let session = {
                let sessions = state.sessions.read().await;
                sessions.values().find(|s| s.alive()).cloned()
            };
            match session {
                Some(s) => {
                    if let Ok(m) = s.master.lock() {
                        let _ = m.resize(portable_pty::PtySize {
                            rows: req.rows,
                            cols: req.cols,
                            pixel_width: 0,
                            pixel_height: 0,
                        });
                    }
                    framed
                        .send(resp(cmd.id, "rc.resize", &serde_json::Value::Null, None))
                        .await?;
                }
                None => {
                    framed
                        .send(resp_err(
                            cmd.id,
                            "rc.resize",
                            "no alive root session (root 终端未挂载)",
                        ))
                        .await?;
                }
            }
            Ok(())
        }
        other => {
            framed
                .send(resp_err(
                    cmd.id,
                    other,
                    &format!("unknown post-hello op: {other}"),
                ))
                .await?;
            Ok(())
        }
    }
}

/// 桥接：client ↔ session 流。
///
/// attach ack 已发出、replay 已发；此处循环：
/// - client Raw 帧 → session.write_input
/// - session 输出（订阅通道）→ client Raw 帧
/// - rc.resize → master.resize
/// - rc.close → session.kill（detach 不杀 bash：kill 只标记 alive=false；
///   bash 由 reader EOF 触发 set_dead。实际产品语义：rc.close 杀 bash + 杀 daemon）
async fn attach_session(
    framed: &mut Framed<UnixStream, FrameCodec>,
    _state: Arc<DaemonState>,
    session: Arc<RootSession>,
    token: u64,
    _attach_id: u64,
) -> anyhow::Result<()> {
    let mut rx = session.subscribe(token);

    loop {
        tokio::select! {
            incoming = framed.next() => {
                match incoming {
                    Some(Ok(Frame::Json(msg))) => {
                        match msg.op.as_str() {
                            "rc.resize" => {
                                let r: RcResize = serde_json::from_value(msg.payload)?;
                                if let Ok(m) = session.master.lock() {
                                    let _ = m.resize(portable_pty::PtySize {
                                        rows: r.rows,
                                        cols: r.cols,
                                        pixel_width: 0,
                                        pixel_height: 0,
                                    });
                                }
                            }
                            "rc.close" => {
                                info!("client (token={}) requested close", token);
                                session.kill();
                            }
                            "rc.ping" => {
                                let _: RcPing = serde_json::from_value(msg.payload)?;
                                let _ = framed
                                    .send(resp(msg.id, "rc.ping", &RcPingResp { alive: session.alive() }, None))
                                    .await;
                            }
                            other => {
                                let _ = framed
                                    .send(resp_err(msg.id, other, &format!("unknown op: {other}")))
                                    .await;
                            }
                        }
                    }
                    Some(Ok(Frame::Raw { stream_id, data })) => {
                        if stream_id == ROOT_STREAM_ID && !data.is_empty() {
                            session.write_input(&data);
                        }
                    }
                    Some(Err(e)) => {
                        warn!("frame read error: {e}");
                        break;
                    }
                    None => break,
                }
            }
            frame = rx.recv() => {
                match frame {
                    Some(f) => {
                        if framed.send(f).await.is_err() {
                            break;
                        }
                    }
                    None => break, // 输出泵 EOF → 客户端应收到 rc.exited 后自己断
                }
            }
        }
    }

    session.unsubscribe(token);
    debug!("client (token={}) detached", token);
    Ok(())
}

/// 读下一帧（握手/attach 阶段用——5s 超时）
async fn next_frame_timed(
    framed: &mut Framed<UnixStream, FrameCodec>,
) -> anyhow::Result<Frame> {
    tokio::time::timeout(std::time::Duration::from_secs(5), framed.next())
        .await
        .context("frame read timeout")?
        .context("connection closed")?
        .context("frame decode failed")
}

fn resp<T: serde::Serialize>(id: u64, op: &str, payload: &T, err: Option<RpcError>) -> Frame {
    Frame::Json(Message {
        id,
        kind: MsgKind::Resp,
        op: op.to_string(),
        payload: serde_json::to_value(payload).unwrap_or(serde_json::Value::Null),
        err,
    })
}

fn resp_err(id: u64, op: &str, msg: &str) -> Frame {
    Frame::Json(Message {
        id,
        kind: MsgKind::Resp,
        op: op.to_string(),
        payload: serde_json::Value::Null,
        err: Some(RpcError {
            code: "op_failed".into(),
            message: msg.into(),
        }),
    })
}
