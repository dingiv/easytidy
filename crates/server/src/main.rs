//! easytidy 容器内 server 入口。
//!
//! 分层（见各模块）：
//! - 连接层 `connection`：单条连接生命周期（读帧→路由→写响应）
//! - 路由层 `router`：协议消息 → 服务分发（handler 失败转 error 响应）
//! - 服务层 `services`：pty / fs / apps / config / lifecycle
//! - 持久层 `storage`：/home/easytidy 数据目录与配置读写
//! - `state`：共享状态（PTY 会话/托管进程/ID 发号）
//! - `setup`：用户环境（euid 分派建号/XDG/fontconfig）
//! - `http`：静态文件服务（图片预览）
//!
//! 部署形态：静态 musl 二进制，bind-mount 进容器，
//! 作为容器 entrypoint 主进程（PID 1 = catatonit，经 podman `--init` 注入）。

mod connection;
mod http;
mod router;
mod services;
mod setup;
mod state;
mod storage;

use std::collections::HashMap;
use std::fs;
use std::fs::OpenOptions;
use std::os::unix::io::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use easytidy_protocol::PROTOCOL_VERSION;
use tokio::net::UnixListener;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::RwLock;
use tracing::{error, info, Event, Subscriber};
use tracing_subscriber::fmt::{format::Writer, FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::prelude::*;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::{fmt, EnvFilter};

/// 统一日志前缀：`easytidy-server` 或 `easytidy-server(容器名)`。
fn log_prefix(container: &str) -> String {
    if container.is_empty() {
        "easytidy-server".to_string()
    } else {
        format!("easytidy-server({container})")
    }
}

/// 统一日志格式：`[LEVEL] easytidy-server(容器名): message`（无时间戳、无 target）。
///
/// 容器名取 `HOSTNAME`（podman 默认 hostname = 容器名）；非容器内运行回落 `easytidy-server`。
#[derive(Clone)]
struct EasyTidyFormat {
    container: String,
}

impl<S, N> FormatEvent<S, N> for EasyTidyFormat
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> std::fmt::Result {
        let level = match *event.metadata().level() {
            tracing::Level::TRACE => "TRACE",
            tracing::Level::DEBUG => "DEBUG",
            tracing::Level::INFO => "INFO",
            tracing::Level::WARN => "WARN",
            tracing::Level::ERROR => "ERROR",
        };
        write!(writer, "[{level}] {}: ", log_prefix(&self.container))?;
        ctx.field_format().format_fields(writer.by_ref(), event)?;
        writeln!(writer)
    }
}

use connection::handle_connection;
use http::start_http_server;
use services::apps::child_prune_task;
use services::lifecycle::perform_graceful_shutdown;
use setup::{ensure_xauthority, fixup_xdg_data_dirs, setup_user_identity};
use state::ServerState;

#[derive(Parser, Debug)]
#[command(name = "easytidy-server")]
struct Args {
    /// Socket path to listen on
    #[arg(long)]
    socket: PathBuf,

    /// Legacy entry command (host 侧已不再需要；此处保留仅为 CLI 兼容——
    /// 旧容器创建时仍传 `--entry`，直接拒绝会启动失败。server 现在忽略它，
    /// 启动应用改由容器内 passthrough auto-start 配置驱动)
    #[arg(long)]
    entry: Option<String>,

    /// Optional log file path. When set, all tracing output AND stderr
    /// (含 eprintln!) 都重定向到该文件;stdout 不动。
    ///
    /// 设计目的:开发期把 server 日志固定到 bind-mount 的宿主机文件,
    /// 不依赖 `podman logs`(后者依赖容器 stdout/stderr 被 podman 收集,
    /// 且容器重启即丢)。GUI/GUI host 侧可以直接 `tail` 该文件查看
    /// server 端实际报错。
    #[arg(long)]
    log_file: Option<PathBuf>,
}

#[tokio::main(worker_threads = 2)]
async fn main() -> Result<()> {
    // Parse arguments
    let args = Args::parse();

    // 日志文件:如指定,先把 stderr dup2 到 file,再 init tracing writer 到 file。
    // 这样 tracing 的 ANSI/INFO/DEBUG 行与所有 eprintln!(诊断行)都进同一份
    // bind-mount 上的宿主文件,容器重启后历史仍在(`podman logs` 容器
    // 退出即丢)。stdout 不动——entry 应用的输出走 stdout,文件会拿走。
    //
    // 必须在 Args::parse() 后立即做:fixup_xdg/ensure_xauthority/tracing 都可能
    // 写错误行,这些行必须进 log_file,否则开发期定位反而看不到。
    if let Some(ref log_path) = args.log_file {
        if let Err(e) = redirect_stderr_to(log_path) {
            eprintln!("failed to open log file {}: {e}", log_path.display());
        }
    }

    // XDG_DATA_DIRS 防御性修正：旧版 flavor 注入纯覆盖值 /usr/share/easytidy-host，
    // 容器内 gdk-pixbuf 找不到系统 loaders.cache → PNG 图标解码失败 → GTK 断言
    // 崩溃（2026-08-07 Chrome 保存图片实测）。对 env 已固化的旧容器追加系统
    // 默认目录（/usr/local/share:/usr/share，glib 默认；缺失路径无害）。
    fixup_xdg_data_dirs();

    // XAUTHORITY server 内置自动注入：路径含随机后缀,每次会话都变,不让用户配——
    // 自动探 /run/user/$uid 下 mutter-Xwaylandauth.* 或 xauth_*,覆盖进程 env
    // (忽略 podman create 时可能注入的旧值)。
    ensure_xauthority();

    // Initialize tracing — 双份日志，统一格式 `[LEVEL] easytidy-server(容器名): message`：
    // 1) stderr（已被 dup2 到 server.log，保留原文件）
    // 2) stdout → 容器 stdout → podman journald 驱动 → 宿主 systemd journal
    //    （用户环境 podman 默认 log driver 即 journald，实测；无需改容器配置）
    let env_filter = EnvFilter::from_default_env()
        .add_directive(tracing::Level::INFO.into())
        .add_directive("easytidy_server=debug".parse()?);
    let format = EasyTidyFormat {
        container: std::env::var("HOSTNAME").unwrap_or_default(),
    };
    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt::layer().event_format(format.clone()).with_writer(std::io::stderr))
        .with(fmt::layer().event_format(format).with_writer(std::io::stdout))
        .init();

    info!("easytidy-server starting (protocol v{})", PROTOCOL_VERSION);
    info!("socket path: {}", args.socket.display());

    // Setup server state
    let state = Arc::new(ServerState {
        sessions: Arc::new(RwLock::new(HashMap::new())),
        default_terminal: Arc::new(std::sync::RwLock::new(HashMap::new())),
        children: Arc::new(RwLock::new(HashMap::new())),
        next_stream_id: Arc::new(AtomicU32::new(1)),
        next_conn_id: Arc::new(AtomicU64::new(1)),
        next_msg_id: Arc::new(AtomicU32::new(1)),
        shutting_down: Arc::new(AtomicBool::new(false)),
    });

    // Spawn child prune task（清理已退出的托管进程条目）
    let reaper_state = state.clone();
    tokio::spawn(async move {
        child_prune_task(reaper_state).await;
    });

    // 身份自发现（新模型）：server 即容器默认用户（容器 User = 配置 uid:gid），
    // 经 /proc/self/status + /etc/passwd + EASYTIDY_HOME/USER_NAME 提示 env
    // 构造身份（setup.rs）。旧 root 容器（euid 0）仅告警后按 uid 0 继续。
    // 建号/sudoers/fontconfig 等 root 操作已迁宿主侧
    // （core::podman::user::prepare_container）。
    let identity = setup_user_identity();

    // 持久层初始化：{home}/.easytidy 数据目录 + 旧配置迁移
    // （/home/easytidy 硬编码已废——server 以配置 uid 运行时 /home 常不可写；
    // config 曾存 /run tmpfs，容器重启即丢）
    storage::init(&identity.home);

    // Setup signal handler for graceful shutdown
    let shutdown_flag = state.shutting_down.clone();
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;


    // FIXME: 不需要使用循环来包住 select! 吗? loop {}
    tokio::select! {
        _ = sigterm.recv() => {
            info!("Received SIGTERM, initiating graceful shutdown");
            shutdown_flag.store(true, Ordering::SeqCst);
        }
        _ = sigint.recv() => {
            info!("Received SIGINT, initiating graceful shutdown");
            shutdown_flag.store(true, Ordering::SeqCst);
        }
        result = run_server(args.socket.clone(), state.clone()) => {
            result?;
        }
    }

    // Graceful shutdown
    info!("Graceful shutdown: stopping children");
    perform_graceful_shutdown(state).await?;

    info!("easytidy-server exiting");
    Ok(())
}

/// 把 stderr fd 重定向到给定日志文件（追加模式）。
///
/// 目的:让所有 `eprintln!`(诊断)+ tracing 默认 writer(也写 stderr)都进
/// 同一份文件,且能在文件里看完整时间序。stdout 不动 —— entry 应用或
/// 后续 pipe 仍走 stdout。
///
/// 实现:`open(O_APPEND|O_CREAT|O_WRONLY, 0o644)` + `dup2(fd, STDERR_FILENO)`
/// —— dup2 之后,任何写到 fd=2 的 syscall/fwrite 都重定向到 file。
///
/// 父目录:不存在则创建(容器内通常是 `/run/easytidy`,podman-init 已建)。
fn redirect_stderr_to(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create log dir {}", parent.display()))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o644)
        .custom_flags(libc::O_APPEND | libc::O_CREAT | libc::O_WRONLY)
        .open(path)
        .with_context(|| format!("open log file {}", path.display()))?;
    let fd = file.as_raw_fd();
    // dup2 之后 file fd 可关闭(已 dup 到 fd=2)
    let r = unsafe { libc::dup2(fd, libc::STDERR_FILENO) };
    if r == -1 {
        return Err(anyhow!("dup2 to STDERR failed: {}", std::io::Error::last_os_error()));
    }
    Ok(())
}

/// Run the main server loop
async fn run_server(
    socket_path: PathBuf,
    state: Arc<ServerState>,
) -> Result<()> {
    // Remove socket if it exists
    if let Err(e) = fs::remove_file(&socket_path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            return Err(anyhow!("Failed to remove existing socket: {}", e));
        }
    }

    // Create parent directory
    if let Some(parent) = socket_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create socket directory: {}", parent.display()))?;
    }

    // Bind and listen
    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("Failed to bind socket: {}", socket_path.display()))?;

    // Set socket permissions
    fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o777))
        .with_context(|| format!("Failed to set socket permissions: {}", socket_path.display()))?;

    info!("Listening on {}", socket_path.display());

    // HTTP 静态文件服务（图片预览/大文件下载;动态端口经 server.info 查询）
    start_http_server().await;

    // 容器内 auto-start 应用：server 自读配置并拉起（容器自包含，不依赖宿主推送）。
    // entry/entry_args（旧 --entry）已弃用：启动应用改由 passthrough auto-start 驱动。
    services::passthrough::launch_auto_start(&state).await;

    // Accept loop
    loop {
        if state.shutting_down.load(Ordering::SeqCst) {
            info!("Shutting down: no longer accepting connections");
            break;
        }

        match listener.accept().await {
            Ok((stream, _addr)) => {
                let state = state.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, state).await {
                        error!("Connection error: {}", e);
                    }
                });
            }
            Err(e) => {
                if state.shutting_down.load(Ordering::SeqCst) {
                    break;
                }
                error!("Accept error: {}", e);
            }
        }
    }

    Ok(())
}
#[cfg(test)]
mod tests {
    use super::log_prefix;

    #[test]
    fn test_log_prefix() {
        assert_eq!(log_prefix("chrome"), "easytidy-server(chrome)");
        assert_eq!(log_prefix(""), "easytidy-server");
    }
}
