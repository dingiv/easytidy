//! easytidy-dock：容器内 root 工具（root 通道 + 容器准备，单二进制）。
//!
//! 由 `easytidy-root-channel` 与 `easytidy-ctool` 合并而来（2026-09-02）：
//! 两个容器内 root 二进制合成一个 `easytidy-dock`，统一 bind-mount 到
//! `/run/easytidy-bin/easytidy-dock`。
//!
//! ## 进程位置
//! 跑在**容器内**。由 GUI/CLI 通过
//! `podman exec --user 0 <container> /run/easytidy-bin/easytidy-dock <mode>`
//! 拉起。子命令：
//!
//! - **`prepare`**（一次性，源自 ctool）：fontconfig 接入 + 建号（可选）+
//!   家目录补齐（幂等）。`prepare_container` 的 exec 目标。
//! - **`daemon`**（长驻，源自 root-channel）：bind
//!   `/run/easytidy/root-channel.sock`、管理 0..N 个 root bash session
//!   （每个 session = 独立 PTY + bash）；多 client 可同时 attach 同一
//!   session（fan-out + 128KB 回放）。
//! - **`bootstrap`**（一次性，源自 root-channel）：确保 daemon 在跑
//!   （socket 可连即返回；否则 `setsid <exe> daemon` 启动）。由首次
//!   root 操作的 GUI/CLI 触发。
//! - **`client {new|attach|ping|list|close}`**（短命，源自 root-channel）：
//!   连 daemon、建/attach session、桥 stdio。生命周期 = podman exec 流
//!   （GUI 关面板即 detach）。
//!
//! ## 生命周期
//! daemon 由 bootstrap 启动后独立 session 跑——bootstrap 退出不影响它。
//! 容器销毁 → conmon SIGKILL → daemon + bash + 残留 client 全死。
//! **零宿主端残留**：所有 socket/lock/PID 文件都在容器内 `/run/easytidy/`。
//!
//! ## detach 语义
//! client exit ≠ session 死。daemon 持有 PTY master，session 跨 client
//! 重连保持（attach `<sid>` 续 bash 状态）。bash 主动 exit 或容器死
//! 才真死。

use anyhow::Context;
use clap::{Parser, Subcommand};

use crate::client::{ClientArgs, ClientCmd};

mod bootstrap;
mod client;
mod daemon;
mod logfile;
mod session;

#[derive(Parser)]
#[command(name = "easytidy-dock")]
struct Args {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

/// 默认（无子命令） = daemon（兼容历史 / 让容器内 `easytidy-dock`
/// 直接跑就是 daemon；`setsid <exe> daemon` 显式传子命令也可）。
#[derive(Subcommand)]
enum Cmd {
    /// 容器内准备：fontconfig 接入 + 建号（可选）+ 家目录补齐（幂等）。
    /// 源自 easytidy-ctool（prepare_container 的 exec 目标）。
    Prepare {
        /// 容器默认用户 uid（与容器 User 字段一致）
        #[arg(long)]
        uid: u32,
        /// 容器默认用户 gid（与容器 User 字段一致）
        #[arg(long)]
        gid: u32,
        /// 配置用户名（缺省 = 不建号，仅 fontconfig + 家目录补齐）
        #[arg(long)]
        name: Option<String>,
    },
    /// 长驻 daemon：bind socket、管理 session、桥流
    Daemon,
    /// 一次性：确保 daemon 在跑（不存在则 setsid 启动后等 socket）
    Bootstrap,
    /// 短命 client：连 daemon、建/attach session、桥 stdio
    Client {
        /// client 子命令
        #[arg(value_enum)]
        cmd: ClientCmd,
        /// attach 时的 session id（cmd=attach 必填）
        #[arg(long)]
        session_id: Option<u64>,
        /// 终端列数（默认 80）
        #[arg(long, default_value_t = 80u16)]
        cols: u16,
        /// 终端行数（默认 24）
        #[arg(long, default_value_t = 24u16)]
        rows: u16,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    // 先建日志目录再初始化日志（文件日志依赖目录存在）
    logfile::ensure_dir();

    let mode = match args.cmd {
        Some(Cmd::Prepare { .. }) => "prepare",
        Some(Cmd::Daemon) | None => "daemon",
        Some(Cmd::Bootstrap) => "bootstrap",
        Some(Cmd::Client { .. }) => "client",
    };
    // client 模式 stderr 会被 podman exec 捕获并桥进终端（残留日志污染）；
    // 日志只进共享文件。daemon/bootstrap/prepare 保留 stderr（prepare 的
    // skip_reason、bootstrap 的报错都走 stderr 供宿主收集）。
    init_logging(mode != "client");

    tracing::info!("easytidy-dock {mode} starting (pid={})", std::process::id());

    match args.cmd.unwrap_or(Cmd::Daemon) {
        Cmd::Prepare { uid, gid, name } => {
            let report = easytidy_core::env::prepare_in_container(uid, gid, name.as_deref())
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            if let Some(reason) = report.skip_reason {
                eprintln!("{reason}");
            }
            Ok(())
        }
        Cmd::Daemon => daemon::run_daemon().await,
        Cmd::Bootstrap => bootstrap::run_bootstrap().await,
        Cmd::Client { cmd, session_id, cols, rows } => {
            // 短命 client：桥结束后必须显式 exit——实测 bridge 返回后若靠
            // tokio runtime 自然退出会卡住（`tokio::io::stdin()` 的阻塞读
            // 线程不随 runtime 回收，进程残留）。显式 exit 保证 close 后
            // 容器内不残留 client 进程。
            let result = client::run_client(ClientArgs {
                cmd,
                session_id,
                cols,
                rows,
            })
            .await;
            match result {
                Ok(()) => std::process::exit(0),
                Err(e) => {
                    tracing::error!("client error: {e:#}");
                    std::process::exit(1);
                }
            }
        }
    }
    .context("easytidy-dock main")
}

fn init_logging(to_stderr: bool) {
    use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};
    let _ = tracing_subscriber::registry()
        .with(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with(
            fmt::layer()
                .with_writer(logfile::layered_writer(to_stderr))
                .with_target(false)
                .with_thread_ids(false),
        )
        .try_init();
}