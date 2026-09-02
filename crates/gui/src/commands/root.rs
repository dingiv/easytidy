//! root 终端命令（容器内 root 通道）。
//!
//! 新设计（2026-09-01）：root-channel 跑在**容器内**，由本模块通过
//! `podman exec --user 0 <container> /usr/bin/easytidy-root-channel --{mode}` 拉起。
//!
//! ## 启动流程（root_terminal_attach）
//! 1. exec `--bootstrap`：确保 daemon 在容器内运行（不存在 → `setsid -f` 启动）
//! 2. exec `--client new`：建新 session，返回 exec stream（stdin/stdout 桥到 daemon）
//! 3. 关闭 exec stream = detach；bash 状态由 daemon 保持，可 re-attach
//!
//! ## 生命周期
//! - 容器死亡 → conmon SIGKILL → daemon + bash + 残留 client 全死
//! - GUI 关 root 面板 → 关闭 exec stream → client exit → daemon 保留 bash
//! - GUI 重开 root 面板 → 新 exec → 选 new（建新 session）或 attach（续旧）
//! - **零宿主端残留**（socket/lock/PID 都在容器内 `/run/easytidy/`）

use futures::StreamExt;
use std::time::Duration;

use easytidy_protocol::rc::{RcListResp, SessionInfo};

use crate::state::{GuiSession, PodmanState};
use crate::state::PtyEvent;

/// 容器内 root-channel 二进制路径（exec 目标）。
///
/// 必须用**容器内路径** `/usr/bin/easytidy-root-channel`（bind-mount 目标）；
/// 宿主侧 `root_channel_binary_path()` 是 dev 相对路径 `crates/core/../../target/...`，
/// runc 在容器命名空间 stat 不到 → "no such file or directory"。bind-mount 阶段
/// 才用宿主路径（`create_with_config` 内部已处理）。
fn root_channel_bin() -> &'static str {
    easytidy_core::podman::Podman::ROOT_CHANNEL_TARGET
}

/// 在容器内启动 root-channel daemon（如未运行）。
/// 走 `podman exec --user 0 <container> /usr/bin/easytidy-root-channel bootstrap`。
/// daemon 由 `bootstrap` 内部 spawn（setsid 独立 session）启动，本进程不持有 daemon。
async fn bootstrap_root_channel(
    podman: &easytidy_core::podman::Podman,
    container: &str,
) -> anyhow::Result<()> {
    let cmd = vec![
        root_channel_bin().to_string(),
        "bootstrap".to_string(),
    ];
    let exec = podman
        .exec_no_tty(container, "0", cmd)
        .await
        .map_err(|e| anyhow::anyhow!("启动 root-channel daemon 失败：{e}"))?;
    // 等 exec 结束（bootstrap 是 fire-and-forget；产物是 daemon 子进程）。
    // bootstrap 的 stderr 也会进 output 流——收集起来，失败时随错误返回。
    let mut stream = exec.output;
    let mut stderr_buf = String::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(bytes) => {
                let s = String::from_utf8_lossy(&bytes);
                if !s.trim().is_empty() {
                    stderr_buf.push_str(&s);
                }
            }
            Err(e) => return Err(anyhow::anyhow!("bootstrap 流错误：{e}")),
        }
    }
    if !stderr_buf.trim().is_empty() {
        tracing::info!("bootstrap stderr: {}", stderr_buf.trim());
    }
    Ok(())
}

/// 读取容器内 root-channel 日志（best-effort；失败返回空串）。
async fn read_root_logs(
    podman: &easytidy_core::podman::Podman,
    container: &str,
) -> String {
    use futures::StreamExt;
    let cmd = vec![
        "tail".to_string(),
        "-n".to_string(),
        "50".to_string(),
        "/run/easytidy/root-channel.log".to_string(),
    ];
    let exec = match podman.exec_no_tty(container, "0", cmd).await {
        Ok(e) => e,
        Err(_) => return String::new(),
    };
    let mut stream = exec.output;
    let mut buf = String::new();
    while let Some(item) = stream.next().await {
        if let Ok(bytes) = item {
            buf.push_str(&String::from_utf8_lossy(&bytes));
        }
    }
    buf
}

/// 检查 daemon 是否在运行（`client ping`，fire-and-forget）。
async fn probe_root_channel(
    podman: &easytidy_core::podman::Podman,
    container: &str,
) -> anyhow::Result<bool> {
    let cmd = vec![
        root_channel_bin().to_string(),
        "client".to_string(),
        "ping".to_string(),
    ];
    let exec = match podman.exec_no_tty(container, "0", cmd).await {
        Ok(e) => e,
        Err(_) => return Ok(false),
    };
    let mut stream = exec.output;
    let mut buf = String::new();
    while let Some(item) = stream.next().await {
        if let Ok(bytes) = item {
            buf.push_str(&String::from_utf8_lossy(&bytes));
        }
    }
    // ping 输出 "alive: true|false\n"
    Ok(buf.contains("alive: true"))
}

/// root 会话目标：新建（`client new`）或复用（`client attach <sid>`）。
enum RootTarget {
    /// 建新 root bash session
    New,
    /// 复用已存在的 session（幂等 attach——不新建 bash）
    Attach { session_id: u64 },
}

/// 在容器内 exec `client {new|attach}`，把 exec stream 的输出桥到 GUI IPC
/// channel，并把 exec.stdin 写侧存入 `root_sink`（供 `root_terminal_write` 写入）。
///
/// client 进程本身桥 stdio 到 daemon PTY；GUI 侧：
/// - exec output → IPC channel（bash 输出）
/// - GUI 输入（`root_terminal_write`）→ exec input → client stdin → daemon PTY
///
/// **幂等**：`Attach` 复用既有 session（daemon fan-out，不新建 bash），
/// 与 `root_terminal_attach` 的 `root_attach_lock` 配合，杜绝 StrictMode
/// 双 attach 生成两个 root bash。
async fn open_root_session(
    podman: &easytidy_core::podman::Podman,
    container: &str,
    cols: u16,
    rows: u16,
    target: RootTarget,
    session: &GuiSession,
    on_event: tauri::ipc::Channel<PtyEvent>,
) -> anyhow::Result<()> {
    let mut cmd = vec![
        root_channel_bin().to_string(),
        "client".to_string(),
    ];
    match target {
        RootTarget::New => {
            cmd.push("new".to_string());
        }
        RootTarget::Attach { session_id } => {
            cmd.push("attach".to_string());
            cmd.push("--session-id".to_string());
            cmd.push(session_id.to_string());
        }
    }
    cmd.push("--cols".to_string());
    cmd.push(cols.to_string());
    cmd.push("--rows".to_string());
    cmd.push(rows.to_string());

    let exec = podman
        .exec_no_tty(container, "0", cmd)
        .await
        .map_err(|e| anyhow::anyhow!("exec root-channel client 失败：{e}"))?;

    // 保存 exec stdin 写侧到 root_sink（root_terminal_write 用）。
    // 关键：必须持有 input 句柄——一旦 drop，podman exec 侧 stdin 立即 EOF，
    // client 的 stdin 桥立即结束 → client 退出 → root 终端"没反应"。
    *session.root_sink.lock().await = Some(exec.input.clone());
    let mut output_stream = exec.output;

    // reader task：exec output → IPC channel
    tokio::spawn(async move {
        while let Some(item) = output_stream.next().await {
            match item {
                Ok(bytes) => {
                    if !bytes.is_empty() {
                        let event = PtyEvent {
                            kind: "data".to_string(),
                            data: Some(bytes.to_vec()),
                            code: None,
                            cwd: None,
                        };
                        if on_event.send(event).is_err() {
                            break;
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("root-client 流错误：{e}");
                    break;
                }
            }
        }
        let _ = on_event.send(PtyEvent {
            kind: "exited".to_string(),
            data: None,
            code: Some(0),
            cwd: None,
        });
    });

    Ok(())
}

/// 探测 root-channel daemon 是否在运行（不拉起；只读语义）。
#[tauri::command]
pub async fn root_terminal_status(
    podman: tauri::State<'_, PodmanState>,
    session: tauri::State<'_, Option<GuiSession>>,
) -> Result<bool, String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    let name = sess.container_name.clone();
    let p = podman.get().await.map_err(|e| e.to_string())?;
    let result = probe_root_channel(&p, &name).await;
    podman.return_podman(p).await;
    result.map_err(|e| e.to_string())
}

/// 挂载 root 终端（幂等）。
///
/// 流程：bootstrap daemon（如未跑）→ 持 `root_attach_lock` → 查既有 alive
/// session → **有则 `client attach` 复用**（不新建 bash，daemon fan-out）、
/// 无则 `client new` 新建。
///
/// 幂等保证：React StrictMode 双 invoke / 面板重开 / 断线重连，都只会在容器里
/// 存在**一个** root bash（多余 attach 复用到同一 session）。daemon 支持多
/// client 同时 attach 同一 session（fan-out）。
#[tauri::command]
pub async fn root_terminal_attach(
    podman: tauri::State<'_, PodmanState>,
    session: tauri::State<'_, Option<GuiSession>>,
    on_event: tauri::ipc::Channel<PtyEvent>,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    let container_name = sess.container_name.clone();

    let p = podman.get().await.map_err(|e| e.to_string())?;

    // Step 1: bootstrap（如 daemon 未跑）
    if !probe_root_channel(&p, &container_name)
        .await
        .unwrap_or(false)
    {
        if let Err(e) = bootstrap_root_channel(&p, &container_name).await {
            podman.return_podman(p).await;
            return Err(format!(
                "启动 root-channel daemon 失败：{e}\n\n提示：可执行 `easytidy root_channel_logs`（GUI 亦可）查看容器内日志 /run/easytidy/root-channel.log"
            ));
        }
        // 等 daemon 就绪（bootstrap 内部已等 socket，再 ping 一次保险）
        let mut ready = false;
        for _ in 0..20 {
            if probe_root_channel(&p, &container_name)
                .await
                .unwrap_or(false)
            {
                ready = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if !ready {
            // 读日志辅助定位（best-effort，失败不阻塞主错误）
            let logs = read_root_logs(&p, &container_name).await;
            podman.return_podman(p).await;
            let logs_hint = if logs.is_empty() {
                "(日志为空)".to_string()
            } else {
                format!("\n--- root-channel 日志 ---\n{logs}")
            };
            return Err(format!(
                "root-channel daemon 启动后 1s 内未就绪：{}",
                logs_hint
            ));
        }
    }

    // Step 2: 幂等 attach。持锁串行「查 session → 建/attach」，防 StrictMode
    // 双 invoke 并发各建一个 session。
    let _guard = sess.root_attach_lock.lock().await;
    let result = async {
        // 查既有 alive session
        let sessions = list_root_sessions(&p, &container_name).await?;
        let target = match sessions.iter().find(|s| s.alive) {
            Some(s) => RootTarget::Attach { session_id: s.id },
            None => RootTarget::New,
        };
        open_root_session(&p, &container_name, cols, rows, target, &sess, on_event).await
    }
    .await;
    podman.return_podman(p).await;
    result.map_err(|e| e.to_string())
}

/// 查询 root-channel 现有 session（`client list`）。失败返回空 Vec（幂等兜底：
/// 查不到就新建，不阻塞 attach）。
async fn list_root_sessions(
    podman: &easytidy_core::podman::Podman,
    container: &str,
) -> anyhow::Result<Vec<SessionInfo>> {
    use futures::StreamExt;
    let cmd = vec![
        root_channel_bin().to_string(),
        "client".to_string(),
        "list".to_string(),
    ];
    let exec = match podman.exec_no_tty(container, "0", cmd).await {
        Ok(e) => e,
        Err(_) => return Ok(Vec::new()),
    };
    let mut stream = exec.output;
    let mut buf = String::new();
    while let Some(item) = stream.next().await {
        if let Ok(bytes) = item {
            buf.push_str(&String::from_utf8_lossy(&bytes));
        }
    }
    match serde_json::from_str::<RcListResp>(&buf) {
        Ok(r) => Ok(r.sessions),
        Err(e) => {
            tracing::warn!("解析 rc.list 失败：{e}（原样：{buf:?}）");
            Ok(Vec::new())
        }
    }
}

/// 向 root 会话写入输入（经 exec stdin → client → daemon PTY）。
#[tauri::command]
pub async fn root_terminal_write(
    session: tauri::State<'_, Option<GuiSession>>,
    data: Vec<u8>,
) -> Result<(), String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    let sink = sess.root_sink.lock().await;
    let Some(sink) = sink.as_ref() else {
        return Err("root 会话未挂载（先 root_terminal_attach）".to_string());
    };
    use tokio::io::AsyncWriteExt;
    let mut w = sink.lock().await;
    w.write_all(&data)
        .await
        .map_err(|e| format!("写 root 终端失败：{e}"))?;
    Ok(())
}

/// 调整 root 会话 TTY 尺寸（PoC 占位）。
#[tauri::command]
pub async fn root_terminal_resize(
    _session: tauri::State<'_, Option<GuiSession>>,
    _cols: u16,
    _rows: u16,
) -> Result<(), String> {
    // TODO: 给 exec'd client 进程发 SIGWINCH（client 内 ioctl → rc.resize）；
    // 或经 daemon 协议（client 重启后收到 SIGWINCH 自动转发）。
    Ok(())
}

/// 主动关闭 root 会话（drop exec stream → client exit → daemon 保留 bash）。
#[tauri::command]
pub async fn root_terminal_close(
    session: tauri::State<'_, Option<GuiSession>>,
) -> Result<(), String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    // detach 语义：清 root_sink。drop exec input 句柄 → podman exec 侧 stdin
    // EOF → client exit → daemon 保留 bash。root 终端输出流也会随之关闭。
    *sess.root_sink.lock().await = None;
    Ok(())
}

/// 读取 root-channel 日志（容器内 `/run/easytidy/root-channel.log`）。
/// 用于定位 root 终端「静默失败」：daemon stderr 被 /dev/null 丢弃，
/// 唯一诊断线索就是这个共享日志文件。
#[tauri::command]
pub async fn root_channel_logs(
    podman: tauri::State<'_, PodmanState>,
    session: tauri::State<'_, Option<GuiSession>>,
    tail: Option<usize>,
) -> Result<String, String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    let name = sess.container_name.clone();
    let p = podman.get().await.map_err(|e| e.to_string())?;

    let mut cmd = vec!["tail".to_string(), "-n".to_string()];
    cmd.push(tail.unwrap_or(100).to_string());
    cmd.push("/run/easytidy/root-channel.log".to_string());

    let result = async {
        let exec = p
            .exec_no_tty(&name, "0", cmd)
            .await
            .map_err(|e| format!("读取 root-channel 日志失败（tail 不可用?）：{e}"))?;
        let mut stream = exec.output;
        let mut buf = String::new();
        while let Some(item) = stream.next().await {
            if let Ok(bytes) = item {
                buf.push_str(&String::from_utf8_lossy(&bytes));
            }
        }
        if buf.trim().is_empty() {
            return Ok::<_, String>("(root-channel 日志为空——daemon 可能未启动或无输出)".to_string());
        }
        Ok(buf)
    }
    .await;
    podman.return_podman(p).await;
    result
}

/// 列出所有 root session（经 daemon `--client list`）。
#[tauri::command]
pub async fn root_session_list(
    podman: tauri::State<'_, PodmanState>,
    session: tauri::State<'_, Option<GuiSession>>,
) -> Result<Vec<SessionInfo>, String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    let name = sess.container_name.clone();
    let p = podman.get().await.map_err(|e| e.to_string())?;

    let cmd = vec![
        root_channel_bin().to_string(),
        "client".to_string(),
        "list".to_string(),
    ];
    let result = async {
        let exec = p
            .exec_no_tty(&name, "0", cmd)
            .await
            .map_err(|e| e.to_string())?;
        let mut stream = exec.output;
        let mut buf = String::new();
        while let Some(item) = stream.next().await {
            if let Ok(bytes) = item {
                buf.push_str(&String::from_utf8_lossy(&bytes));
            }
        }
        let r: RcListResp = serde_json::from_str(&buf)
            .map_err(|e| format!("解析 rc.list 响应失败：{e}"))?;
        Ok::<_, String>(r.sessions)
    }
    .await;
    podman.return_podman(p).await;
    result
}
