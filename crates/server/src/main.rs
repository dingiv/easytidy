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
use tracing::{error, info, warn};
use tracing_subscriber::{EnvFilter, fmt};

use connection::handle_connection;
use http::start_http_server;
use services::apps::{child_prune_task, launch_entry_command};
use services::lifecycle::perform_graceful_shutdown;
use setup::{ensure_xauthority, fixup_xdg_data_dirs, setup_fontconfig, setup_user_mapping};
use state::ServerState;

#[derive(Parser, Debug)]
#[command(name = "easytidy-server")]
struct Args {
    /// Socket path to listen on
    #[arg(long)]
    socket: PathBuf,

    /// Optional entry command to launch on startup (for silent-boot entry chaining)
    #[arg(long)]
    entry: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Parse arguments
    let args = Args::parse();

    // XDG_DATA_DIRS 防御性修正：旧版 flavor 注入纯覆盖值 /usr/share/easytidy-host，
    // 容器内 gdk-pixbuf 找不到系统 loaders.cache → PNG 图标解码失败 → GTK 断言
    // 崩溃（2026-08-07 Chrome 保存图片实测）。对 env 已固化的旧容器追加系统
    // 默认目录（/usr/local/share:/usr/share，glib 默认；缺失路径无害）。
    fixup_xdg_data_dirs();

    // XAUTHORITY server 内置自动注入：路径含随机后缀,每次会话都变,不让用户配——
    // 自动探 /run/user/$uid 下 mutter-Xwaylandauth.* 或 xauth_*,覆盖进程 env
    // (忽略 podman create 时可能注入的旧值)。
    ensure_xauthority();

    // Initialize tracing
    let env_filter = EnvFilter::from_default_env()
        .add_directive(tracing::Level::INFO.into())
        .add_directive("easytidy_server=debug".parse()?);
    fmt()
        .with_env_filter(env_filter)
        .with_target(false)
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

    // 用户一致性映射（distrobox 式）：容器内用户 = 宿主用户（同名/同 uid/gid）。
    // 宿主侧 create_with_config 在 user_home=true 时注入 EASYTIDY_USER_*；
    // 成功则 PTY/entry 经 su 以该用户运行；失败（env 缺失/工具缺失）回退 root，
    // 行为与旧版一致。server 仍以 root 运行——root 才有权创建用户/装包，
    // 应用层经 su 降权。
    setup_user_mapping().await;

    // 宿主字体接入 fontconfig（flavor gui=true 把宿主字体挂到 /usr/share/easytidy-host，
    // 写 local.conf 让 fontconfig 找到——不能覆盖容器自身 /usr/share/fonts，
    // 否则字体/图标包 postinst 写入失败导致 dpkg 安装中断，实测）
    setup_fontconfig();

    // 持久层初始化：/home/easytidy 数据目录 + 旧配置迁移
    // （config 曾存 /run tmpfs，容器重启即丢）
    storage::init();

    // Setup signal handler for graceful shutdown
    let shutdown_flag = state.shutting_down.clone();
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;

    tokio::select! {
        _ = sigterm.recv() => {
            info!("Received SIGTERM, initiating graceful shutdown");
            shutdown_flag.store(true, Ordering::SeqCst);
        }
        _ = sigint.recv() => {
            info!("Received SIGINT, initiating graceful shutdown");
            shutdown_flag.store(true, Ordering::SeqCst);
        }
        result = run_server(args.socket.clone(), state.clone(), args.entry) => {
            result?;
        }
    }

    // Graceful shutdown
    info!("Graceful shutdown: stopping children");
    perform_graceful_shutdown(state).await?;

    info!("easytidy-server exiting");
    Ok(())
}

/// Run the main server loop
async fn run_server(
    socket_path: PathBuf,
    state: Arc<ServerState>,
    entry_cmd: Option<String>,
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

    // Launch entry command if provided
    if let Some(entry_cmd) = entry_cmd {
        if let Err(e) = launch_entry_command(&state, entry_cmd).await {
            warn!("Failed to launch entry command: {}", e);
        }
    }

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