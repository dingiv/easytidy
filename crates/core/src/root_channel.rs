//! root 通道（`easytidy-root-channel` 进程）宿主侧入口。
//!
//! 路径约定（与 root-channel crate 共用；**必须**独立于
//! `$XDG_RUNTIME_DIR/easytidy/` 之下——那目录按代 bind 进容器
//! `/run/easytidy`，socket 放里面会暴露给容器内 root，是提权漏洞）：
//! - socket：`$XDG_RUNTIME_DIR/easytidy-root/<name>.sock`（chmod 0600）
//! - flock：`$XDG_RUNTIME_DIR/easytidy-root/<name>.lock`（防多实例）
//!
//! [`ensure_running`] 是 GUI/CLI 共用的"按需拉起"入口：探测存活 →
//! 未运行则 spawn（orphan 化，GUI 退出后 OS 重父到会话 PID1）→
//! 轮询 connect 直至就绪。

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::error::{Error, Result};

/// root 通道状态目录（`$XDG_RUNTIME_DIR/easytidy-root`）。
pub fn root_channel_dir() -> Result<PathBuf> {
    let runtime = std::env::var("XDG_RUNTIME_DIR").map_err(|_| Error::NoXdgRuntime)?;
    Ok(PathBuf::from(runtime).join("easytidy-root"))
}

/// 某容器的 root 通道 socket 路径。
pub fn root_channel_socket_path(name: &str) -> Result<PathBuf> {
    Ok(root_channel_dir()?.join(format!("{name}.sock")))
}

/// 某容器的 root 通道 flock 文件路径。
pub fn root_channel_lock_path(name: &str) -> Result<PathBuf> {
    Ok(root_channel_dir()?.join(format!("{name}.lock")))
}

/// 按需拉起 root 通道：已运行（socket 可连）→ 直接返回；
/// 未运行 → spawn `easytidy-root-channel --container <name>`（stdout/err
/// 丢弃，orphan 化）并轮询 connect（2s 超时，100ms 间隔）。
///
/// 返回 socket 路径（调用方 connect）。
pub async fn ensure_running(name: &str) -> Result<PathBuf> {
    let socket = root_channel_socket_path(name)?;

    // 快速路径：已运行
    if tokio::net::UnixStream::connect(&socket).await.is_ok() {
        return Ok(socket);
    }

    // 拉起（目录由 root-channel 自管；此处确保存在以便其立即落 flock/socket）
    tokio::fs::create_dir_all(root_channel_dir()?)
        .await
        .map_err(|e| Error::Connect(format!("创建 root 通道目录失败：{e}")))?;

    let bin = crate::root_channel_binary_path()?;
    tracing::info!("拉起 root 通道：{} --container {name}", bin.display());
    tokio::process::Command::new(&bin)
        .arg("--container")
        .arg(name)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| Error::Connect(format!("spawn easytidy-root-channel 失败：{e}")))?;

    // 轮询 connect 直至就绪（root-channel 建 exec + bind socket 通常 <1s）
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match tokio::net::UnixStream::connect(&socket).await {
            Ok(_) => return Ok(socket),
            Err(_) => {
                if Instant::now() >= deadline {
                    return Err(Error::Connect(format!(
                        "root 通道未就绪（2s 内 socket {} 不可连——容器未运行或 root-channel 退出）",
                        socket.display()
                    )));
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}
