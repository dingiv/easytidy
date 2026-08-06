//! easytidy 无头 one-shot CLI（M2 实现）。
//!
//! 子命令：
//! - list: 列出所有容器
//! - create: 创建新容器
//! - start: 启动容器
//! - stop: 停止容器
//! - restart: 重启容器
//! - rebuild: 重建容器（应用 mounts/网络映射配置变更，M4 前置）
//! - rm: 删除容器
//! - inspect: 检查容器详情（含 mounts/网络）
//! - run: 头less 容器内应用运行（M2 新增）
//! - build-server: 构建并安装 musl server 二进制（M2 新增）

use std::path::PathBuf;
use clap::{Parser, Subcommand};
use anyhow::{bail, Result, Context};
use tracing::{info, error, debug};
use tracing_subscriber::EnvFilter;
use crossterm::terminal;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::net::UnixStream;
use tokio::io::AsyncWriteExt;
use tokio_util::codec::Framed;
use futures::{StreamExt, SinkExt};

use easytidy_core::podman::Podman;
use easytidy_core::configfile::ConfigFile;
use easytidy_core::models::{ContainerConfig, NetworkMode};
use easytidy_protocol::{Frame, FrameCodec, Message, MsgKind, PROTOCOL_VERSION};
use easytidy_protocol::{Handshake, HandshakeAck};
use easytidy_protocol::ops::{PtyOpen, PtyOpenResp, PtyResize, PtyExited};

#[derive(Parser)]
#[command(name = "easytidy")]
#[command(about = "easytidy - Linux 容器应用沙盒管理器", long_about = None)]
struct Cli {
    /// 配置文件路径（可选，默认 $XDG_CONFIG_HOME/easytidy/config.toml）
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,

    /// 启用详细日志
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// 列出所有容器
    List,

    /// 创建新容器
    Create {
        /// 镜像名（如 docker.io/library/alpine:latest）
        #[arg(short, long)]
        image: String,

        /// 容器名
        #[arg(short, long)]
        name: String,

        /// home 目录（可选，默认映射 $HOME）
        #[arg(long)]
        home: Option<String>,

        /// 挂载卷（格式：src:dst，可多次指定）
        #[arg(long)]
        volume: Vec<String>,
    },

    /// 启动容器
    Start {
        /// 容器名或 ID
        #[arg(long)]
        container: String,
    },

    /// 停止容器
    Stop {
        /// 容器名或 ID
        #[arg(long)]
        container: String,
    },

    /// 重启容器
    Restart {
        /// 容器名或 ID
        #[arg(long)]
        container: String,
    },

    /// 重建容器（应用配置变更：mounts/网络映射；创建后不可变 → 必须重建）
    Rebuild {
        /// 容器名
        #[arg(long)]
        container: String,
    },

    /// 删除容器
    Rm {
        /// 容器名或 ID
        #[arg(long)]
        container: String,

        /// 强制删除（运行中的容器）
        #[arg(short, long)]
        force: bool,
    },

    /// 检查容器详情
    Inspect {
        /// 容器名或 ID
        #[arg(long)]
        container: String,
    },

    /// 头less 运行容器内命令（M2 新增）
    Run {
        /// 容器名
        #[arg(long)]
        container: String,

        /// 要执行的命令及参数（如：-- /bin/bash -l）
        #[arg(required = false)]
        command: Vec<String>,
    },

    /// 构建并安装 musl server 二进制（M2 新增）
    BuildServer,

    /// 静默启动（stub for M1）
    Boot {
        /// 配置文件路径（由 systemd unit 传入）
        #[arg(long)]
        config: Option<PathBuf>,
    },

    /// 配置 flavor：按模板创建预配置容器（镜像 + setup 安装 + entry 应用）
    Flavor {
        #[command(subcommand)]
        cmd: FlavorCmd,
    },
}

/// flavor 子命令
#[derive(Subcommand)]
enum FlavorCmd {
    /// 列出可用 flavor
    List,
    /// 应用 flavor：创建容器 → 执行 setup → 注册配置（GUI 透传自动注入宿主显示环境）
    Apply {
        /// flavor 名
        flavor: String,
        /// 容器名（默认 = flavor 名）
        #[arg(long)]
        container: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // 初始化日志
    let log_level = if cli.verbose { "debug" } else { "info" };
    let filter = EnvFilter::from_default_env()
        .add_directive(log_level.parse()?);
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .init();

    // 执行子命令（不都需要 podman 连接）
    match cli.command {
        Commands::BuildServer => {
            cmd_build_server()?;
            Ok(())
        }
        Commands::Run { container, command } => {
            let code = cmd_run(container, command).await?;
            std::process::exit(code);
        }
        _ => {
            // 需要 podman 连接的命令
            let podman = match Podman::connect().await {
                Ok(p) => {
                    info!("成功连接到 podman socket");
                    p
                }
                Err(e) => {
                    error!("连接 podman 失败：{}", e);
                    return Err(e.into());
                }
            };

            match cli.command {
                Commands::List => cmd_list(podman).await,
                Commands::Create { image, name, home, volume } => {
                    cmd_create(podman, image, name, home, volume).await
                }
                Commands::Start { container } => cmd_start(podman, container).await,
                Commands::Stop { container } => cmd_stop(podman, container).await,
                Commands::Restart { container } => cmd_restart(podman, container).await,
                Commands::Rebuild { container } => cmd_rebuild(podman, container).await,
                Commands::Rm { container, force } => cmd_rm(podman, container, force).await,
                Commands::Inspect { container } => cmd_inspect(podman, container).await,
                        Commands::Boot { config: _ } => {
                    info!("静默启动（stub）：M1 占位，待 M2 实现");
                    Ok(())
                }
                Commands::Flavor { cmd } => match cmd {
                    FlavorCmd::List => cmd_flavor_list(),
                    FlavorCmd::Apply { flavor, container } => {
                        cmd_flavor_apply(podman, flavor, container).await
                    }
                },
                _ => Ok(()), // 已经在上面处理
            }
        }
    }
}

/// 列出所有容器（打印表格）。
async fn cmd_list(podman: Podman) -> Result<()> {
    let containers = podman.list_containers().await?;

    if containers.is_empty() {
        println!("没有找到容器");
        return Ok(());
    }

    // 打印表头
    println!("{:<30} {:<15} {:<20} {:<10}", "NAME", "ID", "IMAGE", "STATUS");
    println!("{}", "-".repeat(80));

    // 打印每个容器
    for c in containers {
        let managed = if c.managed { "✓" } else { "" };
        println!("{:<30} {:<15} {:<20} {:<10} {}",
            c.name, c.id, c.image, c.status, managed);
    }

    Ok(())
}

/// 创建新容器。
async fn cmd_create(
    podman: Podman,
    image: String,
    name: String,
    _home: Option<String>,
    _volume: Vec<String>,
) -> Result<()> {
    info!("创建容器：{} from {}", name, image);

    // M2: 使用真实的 server 二进制路径
    let server_bin = easytidy_core::server_binary_path()
        .context("无法解析 server 二进制路径（请先运行 `easytidy build-server`）")?;

    info!("使用 server 二进制：{}", server_bin.display());

    // 容器配置（网络默认 Host 模式，产品语义；distrobox 同款）
    let container_config = ContainerConfig {
        name: name.clone(),
        image: image.clone(),
        entry: None,
        silent_boot: false,
        persistent: true,
        ..Default::default()
    };

    // 创建容器（走新入口，应用完整配置）
    match podman.create_with_config(&name, &image, &server_bin, &container_config).await {
        Ok(id) => {
            println!("容器 {} 创建成功（ID: {}）", name, id);

            // 注册到配置文件
            let config_path = ConfigFile::default_path()?;
            let config_file = ConfigFile::with_path(config_path);

            if let Err(e) = config_file.register_container(container_config) {
                error!("注册容器配置失败：{}", e);
            }

            Ok(())
        }
        Err(e) => {
            error!("创建容器失败：{}", e);
            Err(e.into())
        }
    }
}

/// 重建容器（M4 前置：mount/网络映射配置变更后生效）。
///
/// 从 configfile 读取容器配置 → `Podman::rebuild`（commit 当前层 → 删旧 → 同名重建 → 启动）→
/// 打印新 ID。改配置的途径：直接编辑 `~/.config/easytidy/config.toml` 或后续 GUI。
async fn cmd_rebuild(podman: Podman, container: String) -> Result<()> {
    info!("重建容器：{}", container);

    // 从 configfile 读取容器配置
    let config_path = ConfigFile::default_path()?;
    let config_file = ConfigFile::with_path(config_path);
    let Some(config) = config_file.get_container(&container)? else {
        bail!("容器配置不存在：{container}（请先 create，或在 config.toml 中编辑 mounts/network 配置）");
    };

    let new_id = podman.rebuild(&container, &config).await?;
    println!("容器 {} 重建成功（新 ID: {}）", container, new_id);

    // 回写配置（保持 configfile 与容器一致）
    config_file.register_container(config)?;
    Ok(())
}

/// 启动容器。
async fn cmd_start(podman: Podman, container: String) -> Result<()> {
    info!("启动容器：{}", container);

    podman.start(&container).await?;
    println!("容器 {} 启动成功", container);

    Ok(())
}

/// 停止容器。
async fn cmd_stop(podman: Podman, container: String) -> Result<()> {
    info!("停止容器：{}", container);

    podman.stop(&container).await?;
    println!("容器 {} 停止成功", container);

    Ok(())
}

/// 重启容器。
async fn cmd_restart(podman: Podman, container: String) -> Result<()> {
    info!("重启容器：{}", container);

    podman.restart(&container).await?;
    println!("容器 {} 重启成功", container);

    Ok(())
}

/// 删除容器。
async fn cmd_rm(podman: Podman, container: String, force: bool) -> Result<()> {
    info!("删除容器：{} (force: {})", container, force);

    podman.remove(&container, force).await?;
    println!("容器 {} 删除成功", container);

    // 从配置文件注销
    let config_path = ConfigFile::default_path()?;
    let config_file = ConfigFile::with_path(config_path);

    if let Err(e) = config_file.unregister_container(&container) {
        error!("注销容器配置失败：{}", e);
    }

    Ok(())
}

/// 列出可用 flavor。
fn cmd_flavor_list() -> Result<()> {
    let flavors = easytidy_core::flavor::Flavor::list()?;
    if flavors.is_empty() {
        println!("没有可用 flavor（{}）", easytidy_core::flavor::Flavor::flavors_dir()?.display());
        return Ok(());
    }
    println!("可用 flavor：");
    for f in flavors {
        println!("  {f}");
    }
    Ok(())
}

/// 应用 flavor：创建容器 → 启动 → 经 server 无头执行 setup → 注册配置。
async fn cmd_flavor_apply(
    podman: Podman,
    flavor_name: String,
    container: Option<String>,
) -> Result<()> {
    let flavor = easytidy_core::flavor::Flavor::load(&flavor_name)?;
    let name = container.unwrap_or_else(|| flavor_name.clone());

    info!("应用 flavor {flavor_name}：镜像 {}，容器 {name}", flavor.image);

    // 展开配置（GUI 透传自动注入宿主显示环境）
    let config = flavor.build_config(&name)?;

    let server_bin = easytidy_core::server_binary_path()?;
    let id = podman.create_with_config(&name, &flavor.image, &server_bin, &config).await?;
    println!("容器 {name} 创建成功（ID: {}）", &id[..12.min(id.len())]);

    podman.start(&name).await?;
    println!("容器 {name} 已启动，开始执行 setup（{} 条命令）...", flavor.setup.len());

    // 经 server PTY 无头执行每条 setup 命令（run 在非 TTY 下自动跳过 raw mode）。
    // 无头安装必须非交互：注入 DEBIAN_FRONTEND=noninteractive 防 debconf 卡死。
    for (i, cmd) in flavor.setup.iter().enumerate() {
        println!("[setup {}/{}] {}", i + 1, flavor.setup.len(), cmd);
        let full = format!("export DEBIAN_FRONTEND=noninteractive TZ=UTC; {cmd}");
        let code = cmd_run(name.clone(), vec!["bash".to_string(), "-c".to_string(), full.clone()])
            .await?;
        if code != 0 {
            bail!("setup 命令失败（退出码 {code}）：{cmd}");
        }
    }

    // 注册配置（entry 应用 + GUI 透传 env/mounts 落盘，供后续 run/passthrough 使用）
    let config_file = ConfigFile::with_path(ConfigFile::default_path()?);
    config_file.register_container(config)?;

    println!("✅ flavor {flavor_name} 已应用。启动容器内应用：");
    println!("   easytidy run --container {name} -- {entry}{args}",
        entry = flavor.entry.clone().unwrap_or_else(|| "（无 entry，可指定任意命令）".into()),
        args = flavor.entry_args.join(" "));

    Ok(())
}

/// 检查容器详情（M1：复用 list_containers 投影；M2 起补完整 inspect；
/// M4 前置：追加当前生效的 mounts / 网络配置）。
async fn cmd_inspect(podman: Podman, container: String) -> Result<()> {
    info!("检查容器：{}", container);

    let containers = podman.list_containers().await?;
    let Some(c) = containers.iter().find(|c| c.name == container) else {
        bail!("容器不存在：{container}");
    };

    println!("name:    {}", c.name);
    println!("id:      {}", c.id);
    println!("image:   {}", c.image);
    println!("status:  {}", c.status);
    println!("managed: {}", if c.managed { "yes ✓" } else { "no" });

    // 当前生效的 mounts / 网络（来自 podman inspect）
    match podman.inspect_config(&container).await {
        Ok(view) => {
            println!();
            println!("mounts:");
            if view.mounts.is_empty() {
                println!("  (none)");
            }
            for m in &view.mounts {
                println!("  {} -> {} ({})", m.host_path, m.container_path,
                    if m.read_only { "ro" } else { "rw" });
            }

            let mode = match view.network.mode {
                NetworkMode::Host => "host",
                NetworkMode::Mapped => "mapped",
            };
            println!();
            println!("network:");
            println!("  mode:  {}", mode);
            if view.network.ports.is_empty() {
                println!("  ports: (none)");
            }
            for p in &view.network.ports {
                println!("  {} -> {}/{}", p.host_port, p.container_port, p.protocol);
            }
        }
        Err(e) => {
            println!("（获取 mounts/网络失败：{}）", e);
        }
    }

    Ok(())
}

/// 头less 运行容器内命令（M2 实现）。
///
/// 流程：
/// 1. 确保容器运行中
/// 2. 连接到 host_socket_path(name)
/// 3. hello 握手
/// 4. pty.open（获取 stream_id）
/// 5. 循环：stdin → Raw 帧 → stdout，SIGWINCH → pty.resize，pty.exited → 退出
async fn cmd_run(container: String, command: Vec<String>) -> Result<i32> {
    info!("运行容器内命令：container={}, cmd={:?}", container, command);

    // 1. 确保容器运行中
    let podman = Podman::connect().await?;
    let containers = podman.list_containers().await?;
    let container_info = containers.iter()
        .find(|c| c.name == container)
        .context(format!("容器不存在：{}", container))?;

    if container_info.status != "running" {
        info!("容器未运行，正在启动...");
        podman.start(&container).await?;
        // 等待 server 启动
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
    }

    // 2. 连接到 socket
    let socket_path = easytidy_core::host_socket_path(&container)?;
    info!("连接到 socket：{}", socket_path.display());

    let stream = UnixStream::connect(&socket_path)
        .await
        .context(format!("连接 socket 失败（容器可能未就绪）：{}",
            socket_path.display()))?;

    // 3. 握手
    let codec = FrameCodec::new();
    let mut framed = Framed::new(stream, codec);

    // 发送握手
    let handshake = Handshake {
        v: PROTOCOL_VERSION,
        client: "easytidy-cli".to_string(),
        wants: vec!["pty".to_string()],
    };
    let handshake_msg = Message {
        id: 1,
        kind: MsgKind::Req,
        op: "hello".to_string(),
        payload: serde_json::to_value(handshake)?,
        err: None,
    };
    framed.send(Frame::Json(handshake_msg)).await
        .context("发送握手失败")?;

    // 接收握手确认
    let ack_frame = framed.next().await
        .context("接收握手确认失败")?
        .context("握手确认帧为空")?;

    let ack_msg = match ack_frame {
        Frame::Json(msg) => msg,
        Frame::Raw { .. } => bail!("握手响应应为 JSON 帧"),
    };

    if ack_msg.kind != MsgKind::Resp || ack_msg.op != "hello" {
        bail!("握手响应格式错误");
    }

    let ack: HandshakeAck = serde_json::from_value(ack_msg.payload)
        .context("解析握手确认失败")?;

    debug!("握手成功：server={}, v={}", ack.server, ack.v);

    if ack.v != PROTOCOL_VERSION {
        bail!("协议版本不匹配：客户端={}，服务端={}",
            PROTOCOL_VERSION, ack.v);
    }

    // 4. 准备 PTY 打开
    // 获取当前终端尺寸
    let (cols, rows) = terminal::size()
        .context("获取终端尺寸失败")?;

    // 确定命令：未显式指定时发空 cmd，由容器内 server 默认到 /bin/sh
    // （宿主 $SHELL 不一定存在于容器镜像，如 alpine 无 bash）
    let (cmd, argv) = if command.is_empty() {
        (String::new(), Vec::new())
    } else {
        (command[0].clone(), command.clone())
    };

    // 工作目录：容器内可能不存在宿主 HOME，默认 / 避免 spawn 失败
    let cwd = "/".to_string();

    // 获取环境变量
    let env: std::collections::HashMap<String, String> = std::env::vars()
        .filter(|(k, _)| {
            // 过滤掉可能干扰容器的变量
            !matches!(k.as_str(),
                "DISPLAY" | "WAYLAND_DISPLAY" | "XDG_RUNTIME_DIR" | "DBUS_SESSION_BUS_ADDRESS"
            )
        })
        .collect();

    // 发送 pty.open
    let pty_open = PtyOpen {
        cmd: cmd.clone(),
        argv: argv.clone(),
        env: env.clone(),
        cwd: cwd.clone(),
        cols,
        rows,
    };

    let pty_open_msg = Message {
        id: 2,
        kind: MsgKind::Req,
        op: "pty.open".to_string(),
        payload: serde_json::to_value(pty_open)?,
        err: None,
    };
    framed.send(Frame::Json(pty_open_msg)).await
        .context("发送 pty.open 失败")?;

    // 接收 pty.open 响应
    let open_resp_frame = framed.next().await
        .context("接收 pty.open 响应失败")?
        .context("pty.open 响应为空")?;

    let open_resp_msg = match open_resp_frame {
        Frame::Json(msg) => msg,
        Frame::Raw { .. } => bail!("pty.open 响应为 JSON 帧"),
    };

    let open_resp: PtyOpenResp = serde_json::from_value(open_resp_msg.payload)
        .context("解析 pty.open 响应失败")?;

    let stream_id = open_resp.stream_id;
    debug!("PTY 打开成功：stream_id={}", stream_id);

    // 5. 进入主循环
    // 设置终端为 raw 模式（非 TTY（如管道/无头 setup）时跳过，仍可流式 I/O）
    let mut raw_enabled = false;
    match terminal::enable_raw_mode() {
        Ok(()) => raw_enabled = true,
        Err(e) => debug!("非 TTY 场景，跳过 raw 模式：{}", e),
    }

    // 设置退出标志（用于异步关闭）
    let running = std::sync::Arc::new(AtomicBool::new(true));

    // 设置 SIGWINCH 处理（终端尺寸变化）
    let signal_running = running.clone();
    let stream_id_for_signal = stream_id;

    // 创建用于信号处理的 channel
    let (signal_tx, mut signal_rx) = tokio::sync::mpsc::unbounded_channel();

    // 在单独线程中监听 SIGWINCH
    std::thread::spawn(move || {
        use signal_hook::iterator::Signals;

        let mut signals = Signals::new([signal_hook::consts::SIGWINCH])
            .expect("注册信号处理失败");

        while signal_running.load(Ordering::Relaxed) {
            if signals.forever().next().is_some() {
                let _ = signal_tx.send(());
            }
        }
    });

    // 主 I/O 循环
    let mut stdout = tokio::io::stdout();

    // stdin → Frame::Raw 通道（raw 模式下逐块读，独立阻塞线程）
    let (stdin_tx, mut stdin_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 4096];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if stdin_tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let running_clone = running.clone();
    let exit_code = tokio::spawn(async move {
        let mut exit_code: Option<i32> = None;

        loop {
            tokio::select! {
                // 处理帧接收
                Some(frame_result) = framed.next() => {
                    match frame_result {
                        Ok(frame) => {
                            match frame {
                                Frame::Raw { stream_id: sid, data } => {
                                    if sid == stream_id {
                                        // PTY 输出，写入 stdout
                                        if let Err(e) = stdout.write_all(&data).await {
                                            error!("写入 stdout 失败：{}", e);
                                            break;
                                        }
                                        let _ = stdout.flush().await;
                                    } else {
                                        debug!("忽略未知 stream_id: {}", sid);
                                    }
                                }
                                Frame::Json(msg) => {
                                    // 处理事件消息
                                    if msg.op == "pty.exited" {
                                        match serde_json::from_value::<PtyExited>(msg.payload) {
                                            Ok(exited) => {
                                                info!("PTY 进程退出：code={}", exited.code);
                                                exit_code = Some(exited.code);
                                                running_clone.store(false, Ordering::Relaxed);
                                                break;
                                            }
                                            Err(e) => {
                                                error!("解析 pty.exited 失败：{}", e);
                                            }
                                        }
                                    } else {
                                        debug!("忽略其他事件：{}", msg.op);
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            error!("帧解码错误：{}", e);
                            break;
                        }
                    }
                }
                // stdin → PTY（Frame::Raw）
                Some(data) = stdin_rx.recv() => {
                    let _ = framed
                        .send(Frame::Raw { stream_id: stream_id_for_signal, data })
                        .await;
                }
                // 处理终端尺寸变化信号
                Some(_) = signal_rx.recv() => {
                    if let Ok((new_cols, new_rows)) = terminal::size() {
                        let resize = PtyResize {
                            stream_id: stream_id_for_signal,
                            cols: new_cols,
                            rows: new_rows,
                        };

                        let resize_msg = Message {
                            id: 3,
                            kind: MsgKind::Req,
                            op: "pty.resize".to_string(),
                            payload: serde_json::to_value(resize).unwrap(),
                            err: None,
                        };

                        let _ = framed.send(Frame::Json(resize_msg)).await;
                        debug!("发送 pty.resize: {}x{}", new_cols, new_rows);
                    }
                }
                // 检查退出条件
                else => break,
            }
        }

        exit_code
    });

    // stdin → PTY 输入循环（简化版：暂时不实现）
    // 完整实现需要使用 select! 同时处理 stdin 读取和 framed 接收

    // 等待退出
    let code = exit_code.await?.unwrap_or(0);
    running.store(false, Ordering::Relaxed);

    // 清理（仅当之前成功进入 raw 模式）
    if raw_enabled {
        terminal::disable_raw_mode()
            .context("恢复终端模式失败")?;
    }

    // 恢复终端并打印换行
    println!();

    Ok(code)
}

/// 构建并安装 musl server 二进制。
fn cmd_build_server() -> Result<()> {
    use std::process::Command;

    info!("开始构建 musl server 二进制...");

    // 检查 musl target
    let output = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .context("执行 rustup 失败（请确保已安装 rustup）")?;

    let installed = String::from_utf8_lossy(&output.stdout);
    if !installed.contains("x86_64-unknown-linux-musl") {
        info!("添加 musl target...");
        Command::new("rustup")
            .args(["target", "add", "x86_64-unknown-linux-musl"])
            .status()
            .context("添加 musl target 失败")?;
    }

    info!("构建 easytidy-server（musl static）...");
    let status = Command::new("cargo")
        .args([
            "build",
            "-p", "easytidy-server",
            "--release",
            "--target", "x86_64-unknown-linux-musl",
        ])
        .current_dir("/home/jiugui5209/Documents/codes/easy-tidy/easytidy")
        .status()
        .context("构建失败")?;

    if !status.success() {
        bail!("构建失败（退出码：{:?}）", status.code());
    }

    // 确定安装目录
    let install_dir = if let Ok(data_home) = std::env::var("XDG_DATA_HOME") {
        PathBuf::from(data_home).join("easytidy/bin")
    } else {
        let home = std::env::var("HOME")
            .context("无法确定 HOME 目录")?;
        PathBuf::from(home).join(".local/share/easytidy/bin")
    };

    // 创建目录
    std::fs::create_dir_all(&install_dir)
        .context(format!("创建安装目录失败：{}", install_dir.display()))?;

    // 源文件路径
    let source = PathBuf::from("/home/jiugui5209/Documents/codes/easy-tidy/easytidy")
        .join("target/x86_64-unknown-linux-musl/release/easytidy-server");

    // 目标文件路径
    let target = install_dir.join("easytidy-server");

    // 复制文件
    std::fs::copy(&source, &target)
        .context(format!("复制文件失败：{} → {}",
            source.display(), target.display()))?;

    // 设置可执行权限
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&target)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&target, perms)?;
    }

    println!("server 二进制安装成功：{}", target.display());
    info!("验证：file {}", target.display());

    // 验证静态链接
    let output = Command::new("file")
        .arg(&target)
        .output()
        .context("执行 file 命令失败")?;

    println!("{}", String::from_utf8_lossy(&output.stdout));

    Ok(())
}
