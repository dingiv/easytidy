//! easytidy 容器内 server（M2 实现）。
//!
//! 部署形态：静态 musl 二进制，bind-mount 进容器，
//! 作为容器 entrypoint 主进程（PID 1 = catatonit，经 podman `--init` 注入）。
//! 职责（v0.5 需求）：业务 setup、容器内应用生命周期、socket 服务面
//! （PTY / 文件 / 应用枚举 / 配置）。

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, Context, Result};
use base64::Engine;
use clap::Parser;
use easytidy_protocol::{
    AppGetIcon, AppGetIconResp, AppInfo, AppsListResp, CfgGetResp,
    CfgSet, CfgSetResp, Frame, FsEntry, FsEntryType,
    FsList, FsListResp, FsRead, FsReadResp, FsStat, FsStatResp, FsWrite, FsWriteResp,
    Handshake, HandshakeAck, LifecycleEntryLaunch, Message, MsgKind,
    PROTOCOL_VERSION, PtyClose, PtyExited, PtyOpen, PtyOpenResp, PtyResize, RpcError, ShutdownAck,
};
use futures::{SinkExt, StreamExt};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde_json::json;
use tokio::net::{UnixListener, UnixStream};
use tokio::process::Command as TokioCommand;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::{mpsc, RwLock};
use tokio::time::timeout;
use tokio_util::codec::Framed;
use tracing::{debug, error, info, warn};
use tracing_subscriber::{EnvFilter, fmt};

/// Server arguments
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

/// Server state
struct ServerState {
    /// PTY sessions: stream_id -> session
    sessions: Arc<RwLock<HashMap<u32, Arc<PtySession>>>>,

    /// Child processes: pid -> ChildInfo
    children: Arc<RwLock<HashMap<u32, ChildInfo>>>,

    /// Next stream ID
    next_stream_id: Arc<AtomicU32>,

    /// Next message ID
    next_msg_id: Arc<AtomicU32>,

    /// Shutdown flag
    shutting_down: Arc<AtomicBool>,
}

/// PTY session
struct PtySession {
    writer: Arc<std::sync::Mutex<Box<dyn std::io::Write + Send>>>,
    master: Arc<std::sync::Mutex<Box<dyn MasterPty + Send>>>,
}

/// Child process info
#[derive(Debug, Clone)]
struct ChildInfo {
    #[allow(dead_code)]
    pid: u32,
    kind: String,
    #[allow(dead_code)]
    entry_id: Option<String>,
}

impl Clone for ServerState {
    fn clone(&self) -> Self {
        ServerState {
            sessions: self.sessions.clone(),
            children: self.children.clone(),
            next_stream_id: self.next_stream_id.clone(),
            next_msg_id: self.next_msg_id.clone(),
            shutting_down: self.shutting_down.clone(),
        }
    }
}

/// Configuration file path
const CONFIG_PATH: &str = "/run/easytidy/config.json";

/// 用户映射（容器内用户 = 宿主用户，distrobox 式）。
///
/// 由宿主侧 `create_with_config`（user_home=true）经 `EASYTIDY_USER_*` 注入；
/// server 启动时创建同名/同 uid/gid 用户，PTY 与 entry 经 `su` 以该用户拉起应用
/// ——避免容器内 root 读写宿主挂载目录的权限问题。
#[derive(Debug, Clone)]
struct UserMap {
    name: String,
    uid: u32,
    gid: u32,
    home: String,
}

/// 容器内用户固定名（与宿主用户名不同，符合"名字不同、uid 相同"语义）。
const CONTAINER_USER: &str = "node";

/// 用户映射全局态：`setup_user_mapping` 成功后才写入。
/// `user_map()` 返回 `None` 表示映射未生效（PTY/entry 回退 root /bin/sh，与旧版一致）。
static USER_MAP: OnceLock<UserMap> = OnceLock::new();

/// 当前生效的用户映射。
fn user_map() -> Option<&'static UserMap> {
    USER_MAP.get()
}

/// 从环境变量读取用户映射（EASYTIDY_USER_UID/GID，缺失返回 None）。
///
/// 容器内用户固定名为 `node`（与宿主用户名不同），home 为容器内
/// `/home/node`（用户add -m 创建，应用数据存容器层）——rootless podman 的
/// userns 偏移（容器 uid N → 宿主 100000+N）使"uid 对齐"仅为名义一致；
/// 宿主挂载 home 的写操作经免密 sudo 完成（见 setup_user_mapping 第 4 步）。
fn user_map_from_env() -> Option<UserMap> {
    let uid = std::env::var("EASYTIDY_USER_UID").ok()?.parse().ok()?;
    let gid = std::env::var("EASYTIDY_USER_GID").ok()?.parse().ok()?;
    Some(UserMap {
        name: CONTAINER_USER.to_string(),
        uid,
        gid,
        home: format!("/home/{CONTAINER_USER}"),
    })
}

/// 启动期用户映射 setup（main 初始化后、listen 前调用）。
///
/// 1. 组：`getent group <gid>` 未命中则 `groupadd -g <gid> <name>`（缺失回退
///    `addgroup -g <gid> <name>`，alpine/busybox 系）
/// 2. 用户（容器内固定名 `node`，uid/gid 取宿主值，三种情形）：
///    - uid 未占用：`useradd -m -u <uid> -g <gid> -s <shell> node`（容器内 home
///      `/home/node`，应用数据存容器层；缺失回退 adduser，alpine 系）
///    - uid 已存在且同名：无需操作
///    - uid 被镜像默认用户占用（如 ubuntu 镜像 uid 1000 = `ubuntu`）：`usermod -l`
///      改名 + `-d /home/node` + `-g <gid>` 对齐（仅 debian 系有 usermod）
/// 3. 容器内 home（/home/node）存在性 + 属主（容器内目录，chown 安全）
/// 4. ⚠️ 绝不 chown 宿主挂载的 home：会改写宿主文件属主、致宿主用户失权
///    （2026-08-06 实测事故）
/// 5. 免密 sudo：`/etc/sudoers.d/easytidy-node`（宿主 home 写操作/包管理经此提升）
///
/// rootless 说明：userns 偏移（容器 uid N → 宿主 100000+N）使"uid 对齐"为名义一致；
/// 应用常态化权限 = 容器内 node 用户权限；宿主挂载目录读可达、写经免密 sudo。
///
/// 返回用户映射是否生效（env 齐全且用户创建成功）；失败回退 root 运行。
async fn setup_user_mapping() -> bool {
    let Some(user) = user_map_from_env() else {
        debug!("未收到 EASYTIDY_USER_* 环境变量，跳过用户映射（root 容器）");
        return false;
    };

    // 1. 确保组存在
    if !group_gid_exists(user.gid).await {
        let gid = user.gid.to_string();
        let group_ok = if command_available("groupadd") {
            run_cmd(&["groupadd", "-g", &gid, &user.name]).await
        } else if command_available("addgroup") {
            run_cmd(&["addgroup", "-g", &gid, &user.name]).await
        } else {
            warn!("容器内缺少 groupadd/addgroup，无法创建组 {}（gid {}）", user.name, gid);
            false
        };
        if !group_ok {
            warn!("组创建未成功（可能已存在），继续：{}（gid {}）", user.name, gid);
        }
    }

    // 2. 确保用户存在（同名/同 uid/gid）
    match username_for_uid(user.uid).await {
        // uid 未占用 → 创建（debian 系 useradd；alpine/busybox 系 adduser）
        None => {
            let uid = user.uid.to_string();
            let gid = user.gid.to_string();
            let shell = user_shell_path();
            let user_ok = if command_available("useradd") {
                // -m：容器内 home（/home/node），应用数据存容器层（重启/重建保留）；
                // 宿主挂载 home 只读可达，写操作经免密 sudo
                run_cmd(&["useradd", "-m", "-u", &uid, "-g", &gid, "-s", shell, &user.name]).await
            } else if command_available("adduser") {
                run_cmd(&[
                    "adduser", "-D", "-u", &uid, "-G", &user.name, "-s", shell, "-h", &user.home,
                    &user.name,
                ])
                .await
            } else {
                warn!("容器内缺少 useradd/adduser，无法创建用户 {}", user.name);
                false
            };
            if !user_ok {
                warn!("用户创建失败，回退 root 运行：{}", user.name);
                return false;
            }
        }
        // uid 已存在且同名 → 无需操作
        Some(existing) if existing == user.name => {}
        // uid 被镜像默认用户占用（ubuntu 镜像 uid 1000 = ubuntu）→ usermod 改名对齐
        Some(existing) => {
            if !command_available("usermod") {
                warn!(
                    "uid {} 已被镜像用户 {} 占用且容器内无 usermod，无法对齐，回退 root 运行",
                    user.uid, existing
                );
                return false;
            }
            let gid = user.gid.to_string();
            let renamed = run_cmd(&["usermod", "-l", &user.name, &existing]).await;
            let home_ok = renamed && run_cmd(&["usermod", "-d", &user.home, &user.name]).await;
            let gid_ok = home_ok && run_cmd(&["usermod", "-g", &gid, &user.name]).await;
            if !gid_ok {
                warn!(
                    "usermod 对齐用户失败（{} → {}），回退 root 运行",
                    existing, user.name
                );
                return false;
            }
            info!(
                "镜像默认用户 {} 已重命名为 {}（uid {}）",
                existing, user.name, user.uid
            );
        }
    }

    // 3. 确保容器内 home 存在并归用户所有（容器内目录，非宿主挂载，chown 安全）
    if !Path::new(&user.home).exists() {
        run_cmd(&["mkdir", "-p", &user.home]).await;
    }
    let uid_gid = format!("{}:{}", user.uid, user.gid);
    run_cmd(&["chown", &uid_gid, &user.home]).await;

    // 4. ⚠️ 绝不 chown 宿主挂载的 home！
    //    宿主 home 是 bind mount，chown 会改写宿主机文件属主，导致宿主用户失去访问权
    //    （2026-08-06 实测事故：宿主环境崩溃）。权限一致性靠 uid/gid 对齐实现——
    //    容器用户与宿主用户同 uid/gid，天然拥有相同权限，无需也不能动属主。

    // 5. 免密 sudo：容器用户提升权限的通道（宿主 home 写操作、包管理等）。
    //    直接写 /etc/sudoers.d（root 写文件不依赖 sudo 二进制是否已装；
    //    flavor setup 可能在 server 启动后才安装 sudo——文件先就位，装好即生效）。
    //    alpine/busybox 系无 sudo：文件写了无害，装 sudo 后自然生效。
    {
        let sudoers = format!("/etc/sudoers.d/easytidy-{}", user.name);
        let rule = format!("{} ALL=(ALL) NOPASSWD: ALL\n", user.name);
        // 基础镜像装 sudo 前可能没有 /etc/sudoers.d（apt 装 sudo 才创建）——先建目录
        let _ = std::fs::create_dir_all("/etc/sudoers.d");
        let write_result = std::fs::write(&sudoers, rule);
        let chmod_ok = write_result.is_ok() && run_cmd(&["chmod", "440", &sudoers]).await;
        if chmod_ok {
            info!("免密 sudo 已配置：{}", user.name);
        } else if let Err(e) = write_result {
            warn!("免密 sudo 配置失败（写 {} 出错：{e}）", sudoers);
        } else {
            warn!("免密 sudo 配置失败（chmod 440 失败：{}）", sudoers);
        }
    }

    // 6. 校验 + 记录
    if !user_exists(&user).await {
        warn!("用户映射校验失败（用户未创建成功），回退 root 运行：{}", user.name);
        return false;
    }

    info!("用户映射：{}({}:{}) home={}", user.name, user.uid, user.gid, user.home);
    let _ = USER_MAP.set(user);
    true
}

/// 探测 PATH 中是否存在可执行命令（区分镜像系：debian/ubuntu 用 useradd/groupadd，
/// alpine/busybox 用 adduser/addgroup）。
fn command_available(cmd: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| dir.join(cmd).is_file())
    })
}

/// 容器内可用 shell：/bin/bash 优先（ubuntu 系），缺失回退 /bin/sh（alpine/busybox）。
fn user_shell_path() -> &'static str {
    if Path::new("/bin/bash").exists() {
        "/bin/bash"
    } else {
        "/bin/sh"
    }
}

/// 运行命令（无输出捕获，返回成功与否）。
async fn run_cmd(args: &[&str]) -> bool {
    if args.is_empty() {
        return false;
    }
    match TokioCommand::new(args[0]).args(&args[1..]).status().await {
        Ok(s) => s.success(),
        Err(e) => {
            warn!("执行命令失败（{} {}）：{e}", args[0], args.join(" "));
            false
        }
    }
}

/// `getent <database> <key>` 查询（glibc 与 busybox 系均支持）。
async fn run_getent(database: &str, key: &str) -> bool {
    command_available("getent") && run_cmd(&["getent", database, key]).await
}

/// 组 gid 是否已存在（getent 优先；容器无 getent 时解析 /etc/group 兜底）。
async fn group_gid_exists(gid: u32) -> bool {
    if run_getent("group", &gid.to_string()).await {
        return true;
    }
    std::fs::read_to_string("/etc/group").ok().is_some_and(|content| {
        content.lines().any(|line| {
            let f: Vec<&str> = line.split(':').collect();
            f.len() >= 3 && f[2].parse::<u32>().ok() == Some(gid)
        })
    })
}

/// 用户（uid + name）是否已存在：/etc/passwd 直接解析（不依赖 getent）。
async fn user_exists(user: &UserMap) -> bool {
    username_for_uid(user.uid).await.as_deref() == Some(user.name.as_str())
}

/// /etc/passwd 中 uid 对应的用户名（不依赖 getent，容器无 getent 时兜底）。
async fn username_for_uid(uid: u32) -> Option<String> {
    let content = std::fs::read_to_string("/etc/passwd").ok()?;
    content.lines().find_map(|line| {
        let f: Vec<&str> = line.split(':').collect();
        if f.len() >= 3 && f[2].parse::<u32>().ok() == Some(uid) {
            Some(f[0].to_string())
        } else {
            None
        }
    })
}

/// POSIX shell 单引号转义：参数包在单引号内，内部 `'` 用 `'\''` 序列
/// （闭合-转义-重开），保证 `su -c '<cmd>'` 内命令原样传给用户 shell 解析。
fn shell_escape_single_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// 构建 `su -c` 的完整命令串：cmd + argv[1..] 逐个单引号转义后空格拼接
/// （argv 与 cmd 同构：CLI/GUI 均约定 argv[0] == cmd）。
fn build_su_command(cmd: &str, argv: &[String]) -> String {
    let mut parts = Vec::with_capacity(argv.len() + 1);
    parts.push(shell_escape_single_quote(cmd));
    parts.extend(argv.iter().skip(1).map(|a| shell_escape_single_quote(a)));
    parts.join(" ")
}

#[tokio::main]
async fn main() -> Result<()> {
    // Parse arguments
    let args = Args::parse();

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
        children: Arc::new(RwLock::new(HashMap::new())),
        next_stream_id: Arc::new(AtomicU32::new(1)),
        next_msg_id: Arc::new(AtomicU32::new(1)),
        shutting_down: Arc::new(AtomicBool::new(false)),
    });

    // Spawn child reaper
    let reaper_state = state.clone();
    tokio::spawn(async move {
        child_reaper_task(reaper_state).await;
    });

    // 用户一致性映射（distrobox 式）：容器内用户 = 宿主用户（同名/同 uid/gid）。
    // 宿主侧 create_with_config 在 user_home=true 时注入 EASYTIDY_USER_*；
    // 成功则 PTY/entry 经 su 以该用户运行；失败（env 缺失/工具缺失）回退 root，
    // 行为与旧版一致。server 仍以 root 运行——root 才有权创建用户/装包，
    // 应用层经 su 降权。
    setup_user_mapping().await;

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

/// Handle a single connection
async fn handle_connection(
    stream: UnixStream,
    state: Arc<ServerState>,
) -> Result<()> {
    let mut framed = Framed::new(stream, easytidy_protocol::frame::FrameCodec::new());

    // Create channel for outgoing frames (both JSON and Raw)
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<Frame>();

    info!("Client connected");

    // Track handshake completion
    let handshake_done = Arc::new(AtomicBool::new(false));

    // 本连接打开的 PTY 流：有活跃 PTY 时跳过空闲超时（交互 shell 会长时间无输入）
    let mut conn_ptys: std::collections::HashSet<u32> = std::collections::HashSet::new();

    // Main connection loop
    loop {
        if state.shutting_down.load(Ordering::SeqCst) {
            info!("Server shutting down, closing connection");
            break;
        }

        // 空闲超时：无 PTY 的连接 30s，有 PTY 的放宽到 1h（PTY 存活即视为活跃）
        let idle = if conn_ptys.is_empty() {
            Duration::from_secs(30)
        } else {
            Duration::from_secs(3600)
        };

        // Wait for either incoming frame from socket or outgoing event
        tokio::select! {
            // Incoming frame from socket
            frame_result = timeout(idle, framed.next()) => {
                match frame_result {
                    Ok(Some(Ok(frame))) => {
                        match frame {
                            Frame::Json(msg) => {
                                let event_tx_clone = event_tx.clone();
                                // 提前克隆：msg 会被 move 进 handle_message
                                let req_op = msg.op.clone();
                                let req_payload = msg.payload.clone();
                                let response = handle_message(
                                    msg,
                                    &state,
                                    &handshake_done,
                                    event_tx_clone,
                                ).await?;

                                if let Some(resp) = response {
                                    // pty.open 响应 → 登记本连接的 PTY；pty.close 请求 → 注销
                                    if let Frame::Json(ref rmsg) = resp {
                                        if rmsg.op == "pty.open" && rmsg.kind == MsgKind::Resp {
                                            if let Ok(open) =
                                                serde_json::from_value::<PtyOpenResp>(rmsg.payload.clone())
                                            {
                                                conn_ptys.insert(open.stream_id);
                                            }
                                        } else if req_op == "pty.close" && rmsg.kind == MsgKind::Resp {
                                            if let Ok(close) =
                                                serde_json::from_value::<PtyClose>(req_payload.clone())
                                            {
                                                conn_ptys.remove(&close.stream_id);
                                            }
                                        }
                                    }
                                    framed.send(resp).await?;
                                }
                            }
                            Frame::Raw { stream_id, data } => {
                                // Forward raw data to PTY
                                handle_raw_data(stream_id, &data, &state).await?;
                            }
                        }
                    }
                    Ok(Some(Err(e))) => {
                        error!("Frame error: {}", e);
                        break;
                    }
                    Ok(None) => {
                        info!("Client disconnected");
                        break;
                    }
                    Err(_) => {
                        warn!("Connection timeout (idle)");
                        break;
                    }
                }
            }
            // Outgoing frame to send to client
            Some(frame) = event_rx.recv() => {
                // pty.exited 事件 → 注销本连接的 PTY
                if let Frame::Json(ref msg) = frame {
                    if msg.op == "pty.exited" {
                        if let Ok(ev) = serde_json::from_value::<PtyExited>(msg.payload.clone()) {
                            conn_ptys.remove(&ev.stream_id);
                        }
                    }
                }
                framed.send(frame).await?;
            }
        }
    }

    Ok(())
}

/// Handle a JSON message
async fn handle_message(
    msg: Message,
    state: &Arc<ServerState>,
    handshake_done: &Arc<AtomicBool>,
    event_tx: mpsc::UnboundedSender<Frame>,
) -> Result<Option<Frame>> {
    // Require handshake first
    if msg.op != "hello" && !handshake_done.load(Ordering::SeqCst) {
        return Ok(Some(Frame::Json(Message {
            id: msg.id,
            kind: MsgKind::Resp,
            op: msg.op,
            payload: json!(null),
            err: Some(RpcError {
                code: "not_handshaked".to_string(),
                message: "Must handshake first".to_string(),
            }),
        })));
    }

    match (msg.kind, msg.op.as_str()) {
        (MsgKind::Req, "hello") => {
            Ok(Some(handle_handshake(msg, handshake_done).await?))
        }
        (MsgKind::Req, "ping") => {
            Ok(Some(handle_ping(msg).await?))
        }
        (MsgKind::Req, "pty.open") => {
            Ok(Some(handle_pty_open(msg, state, event_tx).await?))
        }
        (MsgKind::Req, "pty.resize") => {
            Ok(Some(handle_pty_resize(msg, state).await?))
        }
        (MsgKind::Req, "pty.close") => {
            Ok(Some(handle_pty_close(msg, state).await?))
        }
        (MsgKind::Req, "fs.list") => {
            Ok(Some(handle_fs_list(msg).await?))
        }
        (MsgKind::Req, "fs.stat") => {
            Ok(Some(handle_fs_stat(msg).await?))
        }
        (MsgKind::Req, "fs.read") => {
            Ok(Some(handle_fs_read(msg).await?))
        }
        (MsgKind::Req, "fs.write") => {
            Ok(Some(handle_fs_write(msg).await?))
        }
        (MsgKind::Req, "apps.list") => {
            Ok(Some(handle_apps_list(msg).await?))
        }
        (MsgKind::Req, "apps.getIcon") => {
            Ok(Some(handle_apps_get_icon(msg).await?))
        }
        (MsgKind::Req, "config.get") => {
            Ok(Some(handle_config_get(msg).await?))
        }
        (MsgKind::Req, "config.set") => {
            Ok(Some(handle_config_set(msg).await?))
        }
        (MsgKind::Req, "lifecycle.entryLaunch") => {
            Ok(Some(handle_lifecycle_entry_launch(msg, state, event_tx).await?))
        }
        (MsgKind::Req, "lifecycle.shutdown") => {
            Ok(Some(handle_lifecycle_shutdown(msg, state).await?))
        }
        (MsgKind::Req, op) => {
            warn!("Unknown operation: {}", op);
            Ok(Some(Frame::Json(Message {
                id: msg.id,
                kind: MsgKind::Resp,
                op: op.to_string(),
                payload: json!(null),
                err: Some(RpcError {
                    code: "unknown_op".to_string(),
                    message: format!("Unknown operation: {}", op),
                }),
            })))
        }
        _ => {
            // Ignore non-requests or unhandled
            Ok(None)
        }
    }
}

/// Handle handshake
async fn handle_handshake(
    msg: Message,
    handshake_done: &Arc<AtomicBool>,
) -> Result<Frame> {
    let hs: Handshake = serde_json::from_value(msg.payload)
        .context("Failed to parse Handshake")?;

    info!("Handshake from client '{}' (v{}, wants: {:?})", hs.client, hs.v, hs.wants);

    if hs.v != PROTOCOL_VERSION {
        return Ok(Frame::Json(Message {
            id: msg.id,
            kind: MsgKind::Resp,
            op: "hello".to_string(),
            payload: json!(null),
            err: Some(RpcError {
                code: "version_mismatch".to_string(),
                message: format!("Protocol version mismatch: client={}, server={}", hs.v, PROTOCOL_VERSION),
            }),
        }));
    }

    handshake_done.store(true, Ordering::SeqCst);

    let ack = HandshakeAck {
        v: PROTOCOL_VERSION,
        server: "easytidy-server".to_string(),
        capabilities: vec![
            "pty".to_string(),
            "fs".to_string(),
            "apps".to_string(),
            "config".to_string(),
            "lifecycle".to_string(),
        ],
    };

    Ok(Frame::Json(Message {
        id: msg.id,
        kind: MsgKind::Resp,
        op: "hello".to_string(),
        payload: serde_json::to_value(ack)?,
        err: None,
    }))
}

/// Handle ping
async fn handle_ping(msg: Message) -> Result<Frame> {
    Ok(Frame::Json(Message {
        id: msg.id,
        kind: MsgKind::Resp,
        op: "ping".to_string(),
        payload: msg.payload,
        err: None,
    }))
}

/// Handle PTY open
async fn handle_pty_open(
    msg: Message,
    state: &Arc<ServerState>,
    event_tx: mpsc::UnboundedSender<Frame>,
) -> Result<Frame> {
    let req: PtyOpen = serde_json::from_value(msg.payload)
        .context("Failed to parse PtyOpen")?;

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

    // 用户映射生效且非 as_root 时经 su 降权到容器内用户 node（distrobox 式）：
    // - cmd 为空：`su - <name>`（登录 shell，HOME/环境由 su 设置）
    // - cmd 非空：`su <name> -c '<shell 转义后的完整命令>'`（cmd + argv[1..] 单引号转义拼接）
    // as_root=true（setup/包管理）：直接以容器 root 运行（rootless 下 = 宿主用户权）
    // 映射未生效（env 缺失/用户创建失败）：维持现状（/bin/sh root）。
    let mut cmd_builder = if req.as_root {
        let cmd = if req.cmd.is_empty() { "/bin/sh".to_string() } else { req.cmd.clone() };
        let argv = if req.argv.is_empty() { vec![cmd.clone()] } else { req.argv.clone() };

        let mut b = CommandBuilder::new(cmd);
        for arg in &argv[1..] {
            b.arg(arg);
        }
        b
    } else if let Some(user) = user_map() {
        if req.cmd.is_empty() {
            let mut b = CommandBuilder::new("su");
            b.arg("-");
            b.arg(&user.name);
            b
        } else {
            let full = build_su_command(&req.cmd, &req.argv);
            let mut b = CommandBuilder::new("su");
            b.arg("-c");
            b.arg(full);
            b.arg(&user.name);
            b
        }
    } else {
        let cmd = if req.cmd.is_empty() { "/bin/sh".to_string() } else { req.cmd.clone() };
        let argv = if req.argv.is_empty() { vec![cmd.clone()] } else { req.argv.clone() };

        let mut b = CommandBuilder::new(cmd);
        for arg in &argv[1..] {
            b.arg(arg);
        }
        b
    };

    // Set environment variables
    for (k, v) in &req.env {
        cmd_builder.env(k, v);
    }

    // Set working directory
    cmd_builder.cwd(&req.cwd);

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

    // Allocate stream ID
    let stream_id = state.next_stream_id.fetch_add(1, Ordering::SeqCst);

    // Create session
    let session = Arc::new(PtySession {
        writer: Arc::new(std::sync::Mutex::new(writer)),
        master: master.clone(),
    });

    // Store session
    {
        let mut sessions = state.sessions.write().await;
        sessions.insert(stream_id, session);
    }

    // Drop the slave after spawn (else master never sees EOF)
    drop(slave);

    info!("PTY opened: stream_id={}, cmd={}", stream_id, req.cmd);

    // Spawn PTY reader thread (owns the child for reaping)
    let msg_id = state.next_msg_id.fetch_add(1, Ordering::SeqCst) as u64;
    let stream_id_copy = stream_id;
    thread::spawn(move || {
        pty_reader_thread(stream_id_copy, reader, child, event_tx, msg_id);
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
fn pty_reader_thread(
    stream_id: u32,
    reader: Box<dyn std::io::Read + Send>,
    mut child: Box<dyn portable_pty::Child + Send>,
    event_tx: mpsc::UnboundedSender<Frame>,
    msg_id: u64,
) {
    info!("PTY reader thread started: stream_id={}", stream_id);

    let mut reader = std::io::BufReader::new(reader);
    let mut buf = [0u8; 8192];

    loop {
        match reader.read(&mut buf) {
            Ok(0) => {
                info!("PTY EOF: stream_id={}", stream_id);
                break;
            }
            Ok(n) => {
                debug!("PTY read {} bytes: stream_id={}", n, stream_id);
                // Send Frame::Raw with actual bytes to client
                let frame = Frame::Raw {
                    stream_id,
                    data: buf[..n].to_vec(),
                };
                if event_tx.send(frame).is_err() {
                    error!("Failed to send PTY data, channel closed");
                    break;
                }
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
    let exit_code = match child.try_wait() {
        Ok(Some(status)) => {
            if status.success() { 0 } else { -1 }
        }
        Ok(None) => {
            // Child still running, try to kill and reap
            info!("PTY child still running at EOF, killing");
            let _ = child.kill();
            match child.wait() {
                Ok(status) => {
                    if status.success() { 0 } else { -1 }
                }
                Err(_) => -1,
            }
        }
        Err(e) => {
            error!("Failed to wait for PTY child: {}", e);
            -1
        }
    };

    // Send pty.exited event
    let _ = event_tx.send(Frame::Json(Message {
        id: msg_id,
        kind: MsgKind::Evt,
        op: "pty.exited".to_string(),
        payload: serde_json::json!({
            "stream_id": stream_id,
            "code": exit_code,
        }),
        err: None,
    }));

    info!("PTY reader thread ended: stream_id={}, exit_code={}", stream_id, exit_code);
}

/// Handle PTY resize
async fn handle_pty_resize(
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
async fn handle_pty_close(
    msg: Message,
    state: &Arc<ServerState>,
) -> Result<Frame> {
    let req: PtyClose = serde_json::from_value(msg.payload)
        .context("Failed to parse PtyClose")?;

    let mut sessions = state.sessions.write().await;
    if let Some(_session) = sessions.remove(&req.stream_id) {
        // Dropping the session will close the writer, causing PTY to see EOF
        // The reader thread will naturally exit after reaping the child

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
async fn handle_raw_data(
    stream_id: u32,
    data: &[u8],
    state: &Arc<ServerState>,
) -> Result<()> {
    let sessions = state.sessions.read().await;
    if let Some(session) = sessions.get(&stream_id) {
        let writer = session.writer.clone();
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
    } else {
        warn!("PTY session {} not found for write", stream_id);
    }
    Ok(())
}

/// Handle fs.list
async fn handle_fs_list(msg: Message) -> Result<Frame> {
    let req: FsList = serde_json::from_value(msg.payload)
        .context("Failed to parse FsList")?;

    let path = Path::new(&req.path);

    let mut entries = Vec::new();

    if let Ok(iter) = fs::read_dir(path) {
        for entry in iter.flatten() {
            let metadata = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };

            let name = entry.file_name().to_string_lossy().to_string();
            let entry_type = if metadata.is_dir() {
                FsEntryType::Dir
            } else if metadata.is_symlink() {
                FsEntryType::Symlink
            } else {
                FsEntryType::File
            };

            let size = if metadata.is_file() {
                Some(metadata.len())
            } else {
                None
            };

            let mode = Some(format!("{:o}", metadata.permissions().mode() & 0o777));

            entries.push(FsEntry {
                name,
                entry_type,
                size,
                mode,
            });
        }
    }

    Ok(Frame::Json(Message {
        id: msg.id,
        kind: MsgKind::Resp,
        op: "fs.list".to_string(),
        payload: serde_json::to_value(FsListResp { entries })?,
        err: None,
    }))
}

/// Handle fs.stat
async fn handle_fs_stat(msg: Message) -> Result<Frame> {
    let req: FsStat = serde_json::from_value(msg.payload)
        .context("Failed to parse FsStat")?;

    let path = Path::new(&req.path);

    let metadata = fs::metadata(path)
        .with_context(|| format!("Failed to stat: {}", req.path))?;

    let entry_type = if metadata.is_dir() {
        FsEntryType::Dir
    } else if metadata.is_symlink() {
        FsEntryType::Symlink
    } else {
        FsEntryType::File
    };

    let size = if metadata.is_file() {
        Some(metadata.len())
    } else {
        None
    };

    let mode = Some(format!("{:o}", metadata.permissions().mode() & 0o777));

    let mtime = metadata.modified()
        .unwrap_or(SystemTime::UNIX_EPOCH)
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs() as i64;

    let atime = metadata.accessed()
        .unwrap_or(SystemTime::UNIX_EPOCH)
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs() as i64;

    let ctime = metadata.created()
        .unwrap_or(SystemTime::UNIX_EPOCH)
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs() as i64;

    Ok(Frame::Json(Message {
        id: msg.id,
        kind: MsgKind::Resp,
        op: "fs.stat".to_string(),
        payload: serde_json::to_value(FsStatResp {
            entry: FsEntry {
                name: req.path.clone(),
                entry_type,
                size,
                mode,
            },
            mtime,
            atime,
            ctime,
        })?,
        err: None,
    }))
}

/// Handle fs.read
async fn handle_fs_read(msg: Message) -> Result<Frame> {
    let req: FsRead = serde_json::from_value(msg.payload)
        .context("Failed to parse FsRead")?;

    let path = Path::new(&req.path);

    let data = fs::read(path)
        .with_context(|| format!("Failed to read file: {}", req.path))?;

    let offset = req.offset.unwrap_or(0) as usize;
    let default_len = if data.len() > offset { data.len() - offset } else { 0 };
    let len = req.len.unwrap_or(default_len as u64) as usize;

    let end = std::cmp::min(offset + len, data.len());
    let slice = &data[offset..end];

    let data_b64 = base64::engine::general_purpose::STANDARD.encode(slice);

    Ok(Frame::Json(Message {
        id: msg.id,
        kind: MsgKind::Resp,
        op: "fs.read".to_string(),
        payload: serde_json::to_value(FsReadResp { data_b64 })?,
        err: None,
    }))
}

/// Handle fs.write
async fn handle_fs_write(msg: Message) -> Result<Frame> {
    let req: FsWrite = serde_json::from_value(msg.payload)
        .context("Failed to parse FsWrite")?;

    let data = base64::engine::general_purpose::STANDARD.decode(&req.data_b64)
        .context("Failed to decode base64 data")?;

    fs::write(&req.path, data)
        .with_context(|| format!("Failed to write file: {}", req.path))?;

    Ok(Frame::Json(Message {
        id: msg.id,
        kind: MsgKind::Resp,
        op: "fs.write".to_string(),
        payload: serde_json::to_value(FsWriteResp {
            bytes_written: req.data_b64.len() as u64 * 3 / 4, // Approximate
        })?,
        err: None,
    }))
}

/// Handle apps.list
async fn handle_apps_list(msg: Message) -> Result<Frame> {
    let mut apps = Vec::new();

    let paths = [
        "/usr/share/applications",
        "/usr/local/share/applications",
        "~/.local/share/applications",
    ];

    for base in &paths {
        let base_path = if let Some(rest) = base.strip_prefix("~/") {
            if let Ok(home) = std::env::var("HOME") {
                PathBuf::from(home).join(rest)
            } else {
                continue;
            }
        } else {
            PathBuf::from(base)
        };

        if let Ok(iter) = fs::read_dir(base_path) {
            for entry in iter.flatten() {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some("desktop") {
                    continue;
                }

                if let Ok(app) = parse_desktop_file(&path) {
                    apps.push(app);
                }
            }
        }
    }

    Ok(Frame::Json(Message {
        id: msg.id,
        kind: MsgKind::Resp,
        op: "apps.list".to_string(),
        payload: serde_json::to_value(AppsListResp { apps })?,
        err: None,
    }))
}

/// Parse a .desktop file
fn parse_desktop_file(path: &Path) -> Result<AppInfo> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read .desktop file: {}", path.display()))?;

    let mut name = None;
    let mut icon_path = None;
    let mut exec = None;
    let mut comment = None;
    let mut no_display = false;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
            continue;
        }

        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            let value = value.trim();

            match key {
                "Name" => name = Some(value.to_string()),
                "Icon" => icon_path = Some(value.to_string()),
                "Exec" => exec = Some(value.to_string()),
                "Comment" => comment = Some(value.to_string()),
                "NoDisplay" => no_display = value == "true",
                _ => {}
            }
        }
    }

    if no_display {
        return Err(anyhow!("NoDisplay=true"));
    }

    let name = name.ok_or_else(|| anyhow!("Missing Name"))?;
    let exec = exec.ok_or_else(|| anyhow!("Missing Exec"))?;

    Ok(AppInfo {
        desktop_file: path.display().to_string(),
        name,
        icon_path,
        exec,
        comment,
    })
}

/// Handle apps.getIcon
async fn handle_apps_get_icon(msg: Message) -> Result<Frame> {
    let req: AppGetIcon = serde_json::from_value(msg.payload)
        .context("Failed to parse AppGetIcon")?;

    let path = Path::new(&req.path);

    let data = fs::read(path)
        .with_context(|| format!("Failed to read icon: {}", path.display()))?;

    let format = path.extension()
        .and_then(|s| s.to_str())
        .unwrap_or("png")
        .to_string();

    let data_b64 = base64::engine::general_purpose::STANDARD.encode(&data);

    Ok(Frame::Json(Message {
        id: msg.id,
        kind: MsgKind::Resp,
        op: "apps.getIcon".to_string(),
        payload: serde_json::to_value(AppGetIconResp { format, data_b64 })?,
        err: None,
    }))
}

/// Handle config.get
async fn handle_config_get(msg: Message) -> Result<Frame> {
    let config = if Path::new(CONFIG_PATH).exists() {
        let content = fs::read_to_string(CONFIG_PATH)
            .with_context(|| format!("Failed to read config: {}", CONFIG_PATH))?;
        serde_json::from_str(&content)?
    } else {
        json!({
            "version": 1,
            "mounts": [],
            "network": {},
            "passthrough": []
        })
    };

    Ok(Frame::Json(Message {
        id: msg.id,
        kind: MsgKind::Resp,
        op: "config.get".to_string(),
        payload: serde_json::to_value(CfgGetResp { config })?,
        err: None,
    }))
}

/// Handle config.set
async fn handle_config_set(msg: Message) -> Result<Frame> {
    let req: CfgSet = serde_json::from_value(msg.payload)
        .context("Failed to parse CfgSet")?;

    // Read existing config
    let config = if Path::new(CONFIG_PATH).exists() {
        let content = fs::read_to_string(CONFIG_PATH)
            .with_context(|| format!("Failed to read config: {}", CONFIG_PATH))?;
        serde_json::from_str(&content)?
    } else {
        json!({
            "version": 1,
            "mounts": [],
            "network": {},
            "passthrough": []
        })
    };

    // TODO: Implement key path resolution (e.g., "mounts.0.path")
    // For now, just log
    info!("Config set: {} = {}", req.key, req.value);

    // Write back
    let content = serde_json::to_string_pretty(&config)?;
    fs::write(CONFIG_PATH, content)
        .with_context(|| format!("Failed to write config: {}", CONFIG_PATH))?;

    Ok(Frame::Json(Message {
        id: msg.id,
        kind: MsgKind::Resp,
        op: "config.set".to_string(),
        payload: serde_json::to_value(CfgSetResp { success: true })?,
        err: None,
    }))
}

/// Handle lifecycle.entryLaunch
async fn handle_lifecycle_entry_launch(
    msg: Message,
    state: &Arc<ServerState>,
    event_tx: mpsc::UnboundedSender<Frame>,
) -> Result<Frame> {
    let req: LifecycleEntryLaunch = serde_json::from_value(msg.payload)
        .context("Failed to parse LifecycleEntryLaunch")?;

    // TODO: Look up entry config and spawn the entry process
    // For now, just spawn a simple shell；用户映射生效时同样经 su 拉起
    info!("Entry launch requested: {}", req.entry_id);

    let mut child = if let Some(user) = user_map() {
        TokioCommand::new("su")
            .args(["-", &user.name])
            .spawn()
            .context("Failed to spawn entry (su)")?
    } else {
        TokioCommand::new("/bin/sh")
            .spawn()
            .context("Failed to spawn entry")?
    };

    let pid = child.id().unwrap();

    // Track child
    {
        let mut children = state.children.write().await;
        children.insert(pid, ChildInfo {
            pid,
            kind: "entry".to_string(),
            entry_id: Some(req.entry_id.clone()),
        });
    }

    // Wait for child in background
    let msg_id = state.next_msg_id.fetch_add(1, Ordering::SeqCst) as u64;
    let entry_id = req.entry_id.clone();
    tokio::spawn(async move {
        match child.wait().await {
            Ok(status) => {
                info!("Entry process exited: pid={}, entry_id={}, status={}", pid, entry_id, status);
                // TODO: Emit child.exited event
            }
            Err(e) => {
                error!("Failed to wait for entry process: {}", e);
            }
        }
    });

    // Emit entry.started event
    event_tx.send(Frame::Json(Message {
        id: msg_id,
        kind: MsgKind::Evt,
        op: "entry.started".to_string(),
        payload: serde_json::json!({
            "pid": pid,
        }),
        err: None,
    }))?;

    Ok(Frame::Json(Message {
        id: msg.id,
        kind: MsgKind::Resp,
        op: "lifecycle.entryLaunch".to_string(),
        payload: json!(null),
        err: None,
    }))
}

/// Handle lifecycle.shutdown
async fn handle_lifecycle_shutdown(
    msg: Message,
    state: &Arc<ServerState>,
) -> Result<Frame> {
    info!("Shutdown requested by client");

    state.shutting_down.store(true, Ordering::SeqCst);

    Ok(Frame::Json(Message {
        id: msg.id,
        kind: MsgKind::Resp,
        op: "lifecycle.shutdown".to_string(),
        payload: serde_json::to_value(ShutdownAck)?,
        err: None,
    }))
}

/// Launch entry command on startup
async fn launch_entry_command(state: &Arc<ServerState>, entry_cmd: String) -> Result<()> {
    info!("Launching entry command: {}", entry_cmd);

    // 用户映射生效时经 su 以容器内宿主用户拉起（entry_cmd 为整条 shell 命令串，
    // 单引号转义后交给 su -c 内的用户 shell 解析）；否则维持旧行为（root 直接 spawn）。
    let mut child = if let Some(user) = user_map() {
        let full = shell_escape_single_quote(&entry_cmd);
        TokioCommand::new("su")
            .arg("-c")
            .arg(full)
            .arg(&user.name)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("Failed to spawn entry command (su)")?
    } else {
        let parts: Vec<&str> = entry_cmd.split_whitespace().collect();
        if parts.is_empty() {
            return Err(anyhow!("Empty entry command"));
        }

        let cmd = parts[0];
        let args = &parts[1..];

        TokioCommand::new(cmd)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("Failed to spawn entry command")?
    };

    let pid = child.id().unwrap();

    // Track child
    {
        let mut children = state.children.write().await;
        children.insert(pid, ChildInfo {
            pid,
            kind: "entry".to_string(),
            entry_id: Some("default".to_string()),
        });
    }

    // Wait for child in background
    tokio::spawn(async move {
        match child.wait().await {
            Ok(status) => {
                info!("Entry process exited: pid={}, status={}", pid, status);
            }
            Err(e) => {
                error!("Failed to wait for entry process: {}", e);
            }
        }
    });

    Ok(())
}

/// Child reaper task
async fn child_reaper_task(state: Arc<ServerState>) {
    info!("Child reaper task started");

    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;

        let children_to_remove = {
            let children = state.children.read().await;
            let mut to_remove = Vec::new();

            for pid in children.keys() {
                // Check if process is still alive
                if let Ok(output) = TokioCommand::new("kill")
                    .arg("-0")
                    .arg(pid.to_string())
                    .output()
                    .await
                {
                    if !output.status.success() {
                        to_remove.push(*pid);
                    }
                }
            }

            to_remove
        };

        for pid in children_to_remove {
            let mut children = state.children.write().await;
            if let Some(child_info) = children.remove(&pid) {
                info!("Child reaped: pid={}, kind={}", pid, child_info.kind);
                // TODO: Emit child.exited event
            }
        }

        if state.shutting_down.load(Ordering::SeqCst) {
            break;
        }
    }

    info!("Child reaper task ended");
}

/// Perform graceful shutdown
async fn perform_graceful_shutdown(state: Arc<ServerState>) -> Result<()> {
    // Give children time to exit gracefully
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Force kill remaining children
    let children = state.children.read().await;
    for (pid, child_info) in children.iter() {
        info!("Killing child: pid={}, kind={}", pid, child_info.kind);
        if let Err(e) = TokioCommand::new("kill")
            .arg("-SIGKILL")
            .arg(pid.to_string())
            .status()
            .await
        {
            warn!("Failed to kill child {}: {}", pid, e);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use easytidy_protocol::FrameCodec;
    use tempfile::NamedTempFile;

    /// Test handshake roundtrip
    #[tokio::test]
    async fn test_handshake_roundtrip() {
        // Create a temporary socket
        let temp_dir = tempfile::tempdir().unwrap();
        let socket_path = temp_dir.path().join("test.sock");

        // Spawn server in background
        let _state = Arc::new(ServerState {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            children: Arc::new(RwLock::new(HashMap::new())),
            next_stream_id: Arc::new(AtomicU32::new(1)),
            next_msg_id: Arc::new(AtomicU32::new(1)),
            shutting_down: Arc::new(AtomicBool::new(false)),
        });

        let socket_path_clone = socket_path.clone();
        tokio::spawn(async move {
            let listener = UnixListener::bind(&socket_path_clone).unwrap();
            if let Ok((stream, _)) = listener.accept().await {
                let mut framed = Framed::new(stream, FrameCodec::new());
                // Accept one frame and respond
                if let Some(Ok(Frame::Json(msg))) = framed.next().await {
                    if msg.op == "hello" {
                        let ack = Frame::Json(Message {
                            id: msg.id,
                            kind: MsgKind::Resp,
                            op: "hello".to_string(),
                            payload: serde_json::to_value(HandshakeAck {
                                v: PROTOCOL_VERSION,
                                server: "easytidy-server".to_string(),
                                capabilities: vec!["pty".to_string()],
                            }).unwrap(),
                            err: None,
                        });
                        let _ = framed.send(ack).await;
                    }
                }
            }
        });

        // Give server time to start
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Connect as client
        let stream = UnixStream::connect(&socket_path).await.unwrap();
        let mut framed = Framed::new(stream, FrameCodec::new());

        // Send handshake
        let handshake = Frame::Json(Message {
            id: 1,
            kind: MsgKind::Req,
            op: "hello".to_string(),
            payload: serde_json::to_value(Handshake {
                v: PROTOCOL_VERSION,
                client: "test-client".to_string(),
                wants: vec!["pty".to_string()],
            }).unwrap(),
            err: None,
        });

        framed.send(handshake).await.unwrap();

        // Receive response
        if let Some(Ok(Frame::Json(resp))) = framed.next().await {
            assert_eq!(resp.op, "hello");
            assert_eq!(resp.kind, MsgKind::Resp);
        } else {
            panic!("Expected handshake response");
        }
    }

    /// Test FS list
    #[tokio::test]
    async fn test_fs_list() {
        // Create a temporary directory with some files
        let temp_dir = tempfile::tempdir().unwrap();
        let temp_file = temp_dir.path().join("test.txt");
        fs::write(&temp_file, b"test content").unwrap();

        let req = Message {
            id: 1,
            kind: MsgKind::Req,
            op: "fs.list".to_string(),
            payload: serde_json::to_value(FsList {
                path: temp_dir.path().to_str().unwrap().to_string(),
            }).unwrap(),
            err: None,
        };

        let msg = handle_fs_list(req).await.unwrap();
        if let Frame::Json(resp) = msg {
            assert_eq!(resp.op, "fs.list");
            if let Ok(list_resp) = serde_json::from_value::<FsListResp>(resp.payload) {
                assert!(!list_resp.entries.is_empty());
            } else {
                panic!("Failed to parse FsListResp");
            }
        } else {
            panic!("Expected JSON frame");
        }
    }

    /// Test apps list parsing
    #[test]
    fn test_parse_desktop_file() {
        let temp_file = NamedTempFile::new().unwrap();
        let desktop_path = temp_file.path().with_extension("desktop");

        let content = r#"[Desktop Entry]
Name=Test App
Exec=test-app --option
Comment=A test application
Icon=test-icon
"#;

        fs::write(&desktop_path, content).unwrap();

        let app = parse_desktop_file(&desktop_path).unwrap();
        assert_eq!(app.name, "Test App");
        assert_eq!(app.exec, "test-app --option");
        assert_eq!(app.comment, Some("A test application".to_string()));
        assert_eq!(app.icon_path, Some("test-icon".to_string()));
    }

    /// 单引号转义：普通 / 含单引号 / 空串 / 含空白与变量 / unicode
    #[test]
    fn test_shell_escape_single_quote() {
        assert_eq!(shell_escape_single_quote("whoami"), "'whoami'");
        assert_eq!(shell_escape_single_quote("a'b"), "'a'\\''b'");
        assert_eq!(shell_escape_single_quote(""), "''");
        assert_eq!(shell_escape_single_quote("echo $HOME"), "'echo $HOME'");
        assert_eq!(shell_escape_single_quote("中文"), "'中文'");
    }

    /// su -c 完整命令拼接：cmd + argv[1..]（argv[0] == cmd 时与整体 argv 拼接等价）
    #[test]
    fn test_build_su_command() {
        // CLI 形态：easytidy run --container n -- sh -c 'echo $HOME'
        assert_eq!(
            build_su_command(
                "sh",
                &["sh".to_string(), "-c".to_string(), "echo $HOME".to_string()]
            ),
            "'sh' '-c' 'echo $HOME'"
        );
        // 单命令无参
        assert_eq!(build_su_command("whoami", &[]), "'whoami'");
        // 命令内含单引号（如 grep 'a b'）
        assert_eq!(
            build_su_command(
                "bash",
                &["bash".to_string(), "-c".to_string(), "echo 'a b'".to_string()]
            ),
            "'bash' '-c' 'echo '\\''a b'\\'''"
        );
    }

    /// 环境变量解析：缺失任一 EASYTIDY_USER_* → None
    #[test]
    fn test_user_map_from_env_missing() {
        // 测试进程通常无 EASYTIDY_USER_*；即便宿主注入也逐项移除（并行测试安全：
        // 其他用例不读这些变量）
        for key in ["EASYTIDY_USER_NAME", "EASYTIDY_USER_UID", "EASYTIDY_USER_GID", "EASYTIDY_USER_HOME"] {
            std::env::remove_var(key);
        }
        assert!(user_map_from_env().is_none());
    }

    /// Test config get/set
    #[tokio::test]
    async fn test_config_get() {
        let req = Message {
            id: 1,
            kind: MsgKind::Req,
            op: "config.get".to_string(),
            payload: serde_json::json!({}),
            err: None,
        };

        let msg = handle_config_get(req).await.unwrap();
        if let Frame::Json(resp) = msg {
            assert_eq!(resp.op, "config.get");
            if let Ok(get_resp) = serde_json::from_value::<CfgGetResp>(resp.payload) {
                assert!(get_resp.config.is_object());
            } else {
                panic!("Failed to parse CfgGetResp");
            }
        } else {
            panic!("Expected JSON frame");
        }
    }
}
