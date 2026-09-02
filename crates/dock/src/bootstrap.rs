//! easytidy-dock bootstrap 模式（确保 daemon 在容器内运行）。
//!
//! 调用入口：宿主 GUI/CLI → `podman exec --user 0 <container> /run/easytidy-bin/easytidy-dock bootstrap`
//! 运行身份：容器内 root（uid=0，因 exec --user 0；server 是 uid 1000 不能拉起 root）
//!
//! 行为：
//! 1. 检查 daemon socket `/run/easytidy/dock.sock` 是否存在
//! 2. 不存在 → `setsid <exe> daemon` 启动 daemon（busybox setsid 无 `-f`；
//!    setsid 创建**新 session**，podman exec 流关闭时 SIGHUP 打不到 daemon），
//!    stdio=/dev/null
//! 3. 等 socket 出现（最长 5s）
//! 4. bootstrap exit 0；daemon 在独立 session 中存活（容器死才死）
//!
//! 注意：busybox `setsid` 不支持 util-linux 的 `-f`（实测 `setsid -f` 直接
//! 报错退出，daemon 从未启动）；`--daemon` 也非法（clap subcommand 是 `daemon`
//! 不带 `--`）。`process_group(0)` 同理不可靠（继承 exec session，流关闭被杀）。
//! 正确形式是 `setsid <exe> daemon`——setsid 自身 exec 成 daemon，天然新 session。

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::Context;

use crate::daemon::DAEMON_SOCKET;

pub async fn run_bootstrap() -> anyhow::Result<()> {
    let socket = Path::new(DAEMON_SOCKET);

    // 快速路径：daemon 已在
    if socket.exists() {
        // 进一步验证 socket 可连（防 stale 文件）
        match tokio::net::UnixStream::connect(DAEMON_SOCKET).await {
            Ok(_) => {
                tracing::info!("easytidy-dock daemon already running");
                return Ok(());
            }
            Err(e) => {
                tracing::warn!("socket exists but not connectable ({e}), removing stale socket");
                // stale socket → 清理后重启
                let _ = tokio::fs::remove_file(DAEMON_SOCKET).await;
            }
        }
    }

    // 拉起 daemon：`setsid <exe> daemon`（busybox 无 `-f`；subcommand 无 `--` 前缀）。
    // busybox/util-linux 的 `setsid` 都直接 setsid() + exec PROG——进程本身变
    // 成 daemon，进入**新 session**，脱离 podman exec 的进程组/控制终端。
    // bootstrap 退出 → daemon 被 PID 1 收养，独立存活（容器死才死）。
    // stdio=/dev/null → daemon 日志只进共享文件 /run/easytidy/dock.log。
    let self_exe = std::env::current_exe().context("current_exe failed")?;
    tracing::info!("spawning easytidy-dock daemon via setsid: {} daemon", self_exe.display());
    let spawned = std::process::Command::new("setsid")
        .arg(&self_exe)
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match spawned {
        Ok(child) => {
            tracing::info!("daemon spawned (child_pid={})", child.id());
        }
        Err(e) => {
            tracing::error!("setsid spawn failed: {e} (is setsid installed?)");
            return Err(anyhow::Error::new(e).context(
                "setsid spawn failed (is setsid installed in the container?)",
            ));
        }
    }

    // 等 socket 出现（daemon 启动 + bind 通常 < 500ms）
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if socket.exists() {
            // 进一步验证可连
            if tokio::net::UnixStream::connect(DAEMON_SOCKET)
                .await
                .is_ok()
            {
                tracing::info!("easytidy-dock daemon ready");
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            anyhow::bail!(
                "easytidy-dock daemon not ready in 5s (socket {} not connectable)",
                DAEMON_SOCKET
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
