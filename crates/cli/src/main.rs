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

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

mod conn;

use crossterm::terminal;
use futures::{SinkExt, StreamExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::AsyncWriteExt;
use tracing::{debug, error, info, warn};
use tracing_subscriber::EnvFilter;

use easytidy_core::configfile::ConfigFile;
use easytidy_core::models::{ContainerConfig, MountConfig, NetworkMode};
use easytidy_core::podman::Podman;
use easytidy_protocol::ops::{PtyExited, PtyOpen, PtyOpenResp, PtyResize};
use easytidy_protocol::{Frame, Message, MsgKind};

#[derive(Parser)]
#[command(name = "easytidy")]
#[command(about = "easytidy - Linux 容器应用沙盒管理器", long_about = None)]
struct Cli {
    /// 容器配置根目录（可选，默认 FileLoader `CONTAINERS` namespace：
    /// dev = `crates/core/data/containers`，prod = `~/.easytidy/data/containers`；
    /// 每容器一个 `<name>.toml`）
    #[arg(short, long, global = true)]
    config_dir: Option<PathBuf>,

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

    /// 拉取镜像（显式动作；create 不再自动拉取）
    Pull {
        /// 镜像名（如 docker.io/library/ubuntu:24.04）
        #[arg(long)]
        image: String,
    },

    /// 创建新容器
    Create {
        /// 镜像名（如 docker.io/library/alpine:latest）
        #[arg(short, long)]
        image: String,

        /// 容器名
        #[arg(short, long)]
        name: String,

        /// 挂载卷（格式：src:dst，可多次指定）。注意家目录不挂载宿主 $HOME
        /// ——容器家目录 = 容器默认用户的 passwd home（容器层持久，2026-08-28
        /// 定案），要挂其他路径用本 flag。
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
        /// 快速重建（普通 commit + fork easytidy_fast 语义，亚秒级；默认为
        /// squash 全量重建）
        #[arg(long)]
        quick: bool,
    },

    /// 撤销 passthrough 导出（删除宿主导出的 .desktop；桌面右键 Remove 用）
    Unexport {
        /// 容器名
        #[arg(long)]
        container: String,

        /// 应用 id（`pt-<hash>` / `custom:<name>`；新格式首选）
        #[arg(long, required = true)]
        app_id: String,
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

        /// 以容器 root 身份运行（无命令 = 附接共享 root 终端（与 GUI 同屏互见，
        /// 退出 = detach）；带命令 = 一次性 exec）
        #[arg(long)]
        root: bool,

        /// 要执行的命令及参数（如：-- /bin/bash -l）
        #[arg(required = false)]
        command: Vec<String>,
    },

    /// 桌面快捷方式统一入口（垫片）：确保容器运行后，无命令 = 打开管理 GUI
    /// （silent_boot 容器仅启动不弹 GUI），带命令 = 透传运行容器内应用
    Open {
        /// 容器名
        #[arg(long)]
        container: String,

        /// 要运行的应用命令及参数（如：-- google-chrome）；省略 = 容器入口
        #[arg(required = false)]
        command: Vec<String>,
    },

    /// 按引用启动容器内应用（id 或名称）：server 查应用登记表自行决定
    /// 「启动谁、如何启动」——调用方不传命令行（导出 .desktop 的 Exec 用此）
    Launch {
        /// 容器名
        #[arg(long)]
        container: String,

        /// 应用 id（`pt-<hash>` / `custom:<name>`）或名称
        #[arg(long)]
        id: String,
    },

    /// 查看容器内 easytidy-dock 日志（诊断 root 终端"静默失败"）
    DockLogs {
        /// 容器名
        #[arg(long)]
        container: String,
        /// 只看末尾 N 行（默认 100）
        #[arg(long, default_value_t = 100)]
        tail: usize,
    },

    /// 自启动入口（systemd user unit 登录时触发）
    Boot {
        /// 容器名
        #[arg(long)]
        container: String,
        /// 非静默模式：容器启动后拉起 Worker GUI 窗口
        #[arg(long)]
        gui: bool,
    },

    /// 环境语义操作（docs/13-mutable-env-paradigm.md：新/删/快照/fork/运行/关闭）
    Env {
        #[command(subcommand)]
        cmd: EnvCmd,
    },

    /// 配置 flavor：按模板创建预配置容器（镜像 + setup 安装 + entry 应用）
    Flavor {
        #[command(subcommand)]
        cmd: FlavorCmd,
    },
}

/// 环境语义子命令
#[derive(Subcommand)]
enum EnvCmd {
    /// 新环境：创建干净环境开始操作
    New {
        /// 环境名
        #[arg(long)]
        name: String,
        /// flavor 名（可选：按模板创建，含 GUI 透传/用户映射）
        #[arg(long)]
        flavor: Option<String>,
        /// 镜像（无 flavor 时）
        #[arg(long)]
        image: Option<String>,
    },
    /// 删除环境（删干净：容器+配置+桌面图标+socket）
    Rm {
        /// 环境名
        name: String,
    },
    /// 快照：为当前环境创建保险（commit 容器层）
    Snapshot {
        /// 环境名
        name: String,
        /// 快照名（最终镜像 = easytidy/snapshot/<name>[:<tag>]；默认=<容器名>-<YYYYmmdd-HHMM>）
        #[arg(long)]
        snapshot: Option<String>,
        /// 普通 commit（保留分层历史）；默认 squash 单层
        #[arg(long)]
        no_squash: bool,
    },
    /// 运行环境（细粒度控制，与创建/销毁分离）
    Start { name: String },
    /// 关闭环境（保留，可随时恢复）
    Stop { name: String },
    /// 环境列表
    List,
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
    let filter = EnvFilter::from_default_env().add_directive(log_level.parse()?);
    tracing_subscriber::fmt().with_env_filter(filter).init();

    // 执行子命令（不都需要 podman 连接）
    match cli.command {
        Commands::Run {
            container,
            root,
            command,
        } => {
            let code = cmd_run(container, command, root).await?;
            std::process::exit(code);
        }
        Commands::Open { container, command } => {
            let code = cmd_open(container, command, cli.config_dir.clone()).await?;
            std::process::exit(code);
        }
        Commands::Launch { container, id } => {
            let code = cmd_launch(container, id).await?;
            std::process::exit(code);
        }
        Commands::DockLogs { container, tail } => {
            cmd_dock_logs(&container, tail).await?;
            Ok(())
        }
        Commands::Unexport { container, app_id } => {
            // 宿主侧操作，无需 podman 连接
            cmd_unexport(container, app_id)
        }
        // flavor list 是纯本地操作（读 flavors 目录），无需 podman 连接——
        // 放在 podman 连接前，避免 socket 没起时报"连接 podman 失败"
        Commands::Flavor {
            cmd: FlavorCmd::List,
        } => cmd_flavor_list(),
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
                Commands::Pull { image } => cmd_pull(podman, image).await,
                Commands::Create { image, name, volume } => {
                    cmd_create(podman, image, name, volume, cli.config_dir.clone()).await
                }
                Commands::Start { container } => cmd_start(podman, container).await,
                Commands::Stop { container } => cmd_stop(podman, container).await,
                Commands::Restart { container } => cmd_restart(podman, container).await,
                Commands::Rebuild { container, quick } => {
                    cmd_rebuild(podman, container, quick, cli.config_dir.clone()).await
                }
                Commands::Rm { container, force } => {
                    cmd_rm(podman, container, force, cli.config_dir.clone()).await
                }
                Commands::Inspect { container } => cmd_inspect(podman, container).await,
                Commands::Boot { container, gui } => cmd_boot(podman, container, gui).await,
                Commands::Env { cmd } => match cmd {
                    EnvCmd::New {
                        name,
                        flavor,
                        image,
                    } => {
                        cmd_env_new(podman, name, flavor, image, cli.config_dir.clone()).await
                    }
                    EnvCmd::Rm { name } => {
                        cmd_env_rm(podman, name, cli.config_dir.clone()).await
                    }
                    EnvCmd::Snapshot {
                        name,
                        snapshot,
                        no_squash,
                    } => cmd_env_snapshot(podman, name, snapshot, !no_squash).await,
                    EnvCmd::Start { name } => cmd_env_start(podman, name).await,
                    EnvCmd::Stop { name } => cmd_env_stop(podman, name).await,
                    EnvCmd::List => cmd_env_list(podman).await,
                },
                Commands::Flavor { cmd } => match cmd {
                    // List 已在 podman 连接前处理（纯本地，无需 podman）——此处不可达
                    FlavorCmd::List => unreachable!("flavor list handled before podman connect"),
                    FlavorCmd::Apply { flavor, container } => {
                        cmd_flavor_apply(
                            podman,
                            flavor,
                            container,
                            cli.config_dir.clone(),
                        )
                        .await
                    }
                },
                // 外层已处理的变体（Run/Open/DockLogs/Unexport/Flavor::List）——
                // 运行时不可达（它们在 podman 连接前就返回了），仅满足 match 穷尽
                _ => Ok(()),
            }
        }
    }
}

/// 拉取镜像（显式动作；create 不再自动拉取）。
async fn cmd_pull(podman: Podman, image: String) -> Result<()> {
    info!("拉取镜像：{}", image);
    podman.pull_image(&image).await?;
    println!("镜像 {} 拉取成功", image);
    Ok(())
}

/// 列出所有容器（打印表格）。
async fn cmd_list(podman: Podman) -> Result<()> {
    let containers = podman.list_containers().await?;

    if containers.is_empty() {
        println!("没有找到容器");
        return Ok(());
    }

    // 打印表头
    println!(
        "{:<30} {:<15} {:<20} {:<10}",
        "NAME", "ID", "IMAGE", "STATUS"
    );
    println!("{}", "-".repeat(80));

    // 打印每个容器
    for c in containers {
        let managed = if c.managed { "✓" } else { "" };
        println!(
            "{:<30} {:<15} {:<20} {:<10} {}",
            c.name, c.id, c.image, c.status, managed
        );
    }

    Ok(())
}

/// 创建新容器。
async fn cmd_create(
    podman: Podman,
    image: String,
    name: String,
    volume: Vec<String>,
    config_dir: Option<PathBuf>,
) -> Result<()> {
    info!("创建容器：{} from {}", name, image);

    // 容器内二进制（server + ctool；dev→musl 构建树，prod→安装位，统一走 core helper）
    let bins = easytidy_core::ContainerBins::resolve()?;

    // --volume src:dst → MountConfig（用户显式挂载；家目录不在此列——见 flag doc）
    let mut mounts = Vec::new();
    for v in &volume {
        let (host, container) = v
            .split_once(':')
            .with_context(|| format!("--volume 格式应为 src:dst（收到：{v}）"))?;
        if host.is_empty() || container.is_empty() {
            bail!("--volume 的 src/dst 不能为空（收到：{v}）");
        }
        mounts.push(MountConfig {
            host_path: host.to_string(),
            container_path: container.to_string(),
            read_only: false,
        });
    }

    // 容器配置（网络默认 Host 模式，产品语义；distrobox 同款）
    let container_config = ContainerConfig {
        name: name.clone(),
        params: easytidy_core::models::ContainerParams {
            image: image.clone(),
            mounts,
            ..Default::default()
        },
        silent_boot: false,
        persistent: true,
        ..Default::default()
    };

    // 创建容器（走新入口，应用完整配置）
    match podman
        .create_with_config(&name, &image, &bins, &container_config)
        .await
    {
        Ok(id) => {
            println!("容器 {} 创建成功（ID: {}）", name, id);

            // 启动（对齐 GUI env_new：创建即启动）
            podman.start(&name).await.map_err(|e| {
                error!("启动容器失败：{}", e);
                e
            })?;

            // 容器内准备（fontconfig / useradd / 家目录补齐）：best-effort，
            // 失败不阻断创建（落日志，重建可重跑修复）
            if let Err(e) = podman
                .prepare_container(&name, &container_config.params)
                .await
            {
                error!("容器内准备失败（忽略，重建可修复）：{e}");
            }

            // 注册到配置文件（--config-dir 覆盖）
            let config_file = config_file_for(&config_dir)?;

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
/// 从 configfile 读取容器配置 → `Podman::rebuild`（commit 当前层 → 保留旧容器 →
/// 用新配置重建并启动 → 确认新容器就绪后才删旧；失败自动回滚，环境不中断）→
/// 打印新 ID。改配置的途径：直接编辑 `~/.easytidy/data/containers/<name>.toml` 或后续 GUI。
async fn cmd_rebuild(
    podman: Podman,
    container: String,
    quick: bool,
    config_dir: Option<PathBuf>,
) -> Result<()> {
    info!("重建容器：{}（quick={quick}）", container);

    // 从 configfile 读取容器配置（--config-dir 覆盖）
    let config_file = config_file_for(&config_dir)?;
    let Some(config) = config_file.get_container(&container)? else {
        bail!("容器配置不存在：{container}（请先 create，或编辑 <容器配置目录>/{container}.toml 的 mounts/network）");
    };

    // 容器内二进制需 bind-mount 进重建后的容器（与 create 同源，统一走 core helper）
    let bins = easytidy_core::ContainerBins::resolve()?;
    let new_id = if quick {
        podman.rebuild_quick(&container, &config, &bins).await?
    } else {
        podman.rebuild(&container, &config, &bins).await?
    };
    println!("容器 {} 重建成功（新 ID: {}）", container, new_id);

    // 容器内准备（对齐 GUI：重建后重跑 fontconfig / useradd / 家目录补齐）：
    // best-effort，失败不阻断（落日志）
    if let Err(e) = podman.prepare_container(&container, &config.params).await {
        error!("容器内准备失败（忽略，再次重建可修复）：{e}");
    }

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

/// 自启动入口（systemd user unit 登录时触发）。
///
/// - 容器未运行 → 启动（`Podman::start` 顺带触发 passthrough auto-start 拉起）
/// - `gui=true`（非静默模式）→ 等待 server socket 就绪后拉起 Worker GUI
///
/// 复用调用方已建的 podman 连接（dispatch 层），避免双连接。
async fn cmd_boot(podman: Podman, container: String, gui: bool) -> Result<()> {
    ensure_running(&podman, &container).await?;
    info!("自启动：容器 {container} 已就绪");

    if gui {
        // 等待 server socket 就绪（容器刚启动，server 需初始化）；
        // 仅 gui 分支等待——silent boot 今天就不等（server 启动失败
        // 不应把 oneshot unit 打成 failed）
        let socket = wait_socket_ready(&container).await?;
        // 拉起 Worker GUI（CLI 同目录 / 安装目录 / PATH 探测）
        let gui_bin =
            gui_binary_path().ok_or_else(|| anyhow::anyhow!("找不到 easytidy-gui 可执行文件"))?;
        std::process::Command::new(&gui_bin)
            .arg("--container")
            .arg(&container)
            .spawn()
            .map_err(|e| anyhow::anyhow!("拉起 Worker GUI 失败：{e}"))?;
        info!(
            "非静默启动：已拉起 Worker GUI（{}，socket {}）",
            gui_bin.display(),
            socket.display()
        );
    }
    Ok(())
}

/// 探测 Worker GUI 二进制（① CLI 同目录 ② 安装目录 ③ PATH）。
fn gui_binary_path() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let sibling = parent.join("easytidy-gui");
            if sibling.exists() {
                return Some(sibling);
            }
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let installed = PathBuf::from(home).join(".local/share/easytidy/bin/easytidy-gui");
        if installed.exists() {
            return Some(installed);
        }
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join("easytidy-gui"))
            .find(|p| p.exists())
    })
}

/// 停止容器。
async fn cmd_stop(podman: Podman, container: String) -> Result<()> {
    info!("停止容器：{}", container);

    podman.stop(&container).await?;
    println!("容器 {} 停止成功", container);

    Ok(())
}

/// 撤销 passthrough 导出（删除宿主导出的 .desktop；桌面右键 Remove 调用）。
fn cmd_unexport(container: String, app_id: String) -> Result<()> {
    let removed = easytidy_core::desktop::remove_passthrough(&container, &app_id)?;
    println!("已撤销导出：{}（{}）", app_id, removed.display());
    Ok(())
}

/// 存储健康诊断 + 可选一键修复（docs/18 道路二）。
/// 用最近一份备份回滚 storage.conf（宿主侧操作，无需 podman）。
/// 重启容器。
async fn cmd_restart(podman: Podman, container: String) -> Result<()> {
    info!("重启容器：{}", container);

    podman.restart(&container).await?;
    println!("容器 {} 重启成功", container);

    Ok(())
}

/// 删除容器。
async fn cmd_rm(
    podman: Podman,
    container: String,
    force: bool,
    config_dir: Option<PathBuf>,
) -> Result<()> {
    info!("删除容器：{} (force: {})", container, force);

    podman.remove(&container, force).await?;
    println!("容器 {} 删除成功", container);

    // 从配置文件注销（--config-dir 覆盖）
    let config_file = config_file_for(&config_dir)?;

    if let Err(e) = config_file.unregister_container(&container) {
        error!("注销容器配置失败：{}", e);
    }

    Ok(())
}

/// 新环境：flavor 模板或指定镜像创建干净环境。
async fn cmd_env_new(
    podman: Podman,
    name: String,
    flavor: Option<String>,
    image: Option<String>,
    config_dir: Option<PathBuf>,
) -> Result<()> {
    match flavor {
        Some(f) => {
            cmd_flavor_apply(podman, f, Some(name), config_dir.clone()).await
        }
        None => {
            let Some(image) = image else {
                bail!("env new 需要 --flavor <f> 或 --image <img>");
            };
            let bins = easytidy_core::ContainerBins::resolve()?;
            let config = ContainerConfig {
                name: name.clone(),
                params: easytidy_core::models::ContainerParams {
                    image: image.clone(),
                    ..Default::default()
                },
                ..Default::default()
            };
            let id = podman
                .create_with_config(&name, &image, &bins, &config)
                .await?;
            podman.start(&name).await?;
            // 容器内准备（fontconfig / useradd / 家目录补齐）：best-effort，失败
            // 不阻断（对齐 cmd_create / GUI env_new；core 契约见 podman/user.rs）
            if let Err(e) = podman
                .prepare_container(&name, &config.params)
                .await
            {
                error!("容器内准备失败（忽略，重建可修复）：{e}");
            }
            let config_file = config_file_for(&config_dir)?;
            config_file.register_container(config)?;
            println!(
                "新环境 {name} 已创建并运行（ID: {}）",
                &id[..12.min(id.len())]
            );
            Ok(())
        }
    }
}

/// 删除环境：容器 + 配置 + 桌面图标 + socket 目录全清理（快照为资产保留并提示）。
async fn cmd_env_rm(podman: Podman, name: String, config_dir: Option<PathBuf>) -> Result<()> {
    podman.remove(&name, true).await?;
    let config_file = config_file_for(&config_dir)?;
    if let Err(e) = config_file.unregister_container(&name) {
        error!("注销配置失败：{}", e);
    }
    // 桌面图标（新旧格式 × 菜单/桌面副本，全清）
    let _ = easytidy_core::desktop::remove_entry_desktops(&name);
    // 清理 socket 目录（$XDG_RUNTIME_DIR/easytidy/<name>-<hash>，全代；尽力而为）
    let _ = easytidy_core::remove_socket_dirs(&name);
    println!("环境 {name} 已删除（无残留）");
    println!("  提示：该环境的快照（easytidy/snapshot/* 下按你命名的镜像）为独立资产，已保留");
    Ok(())
}

/// 快照：commit 当前容器层（仅文件系统层，bind mount 不入快照）。
/// 未提供 --snapshot 时 core 兜底为可读默认名 <容器名>-<YYYYmmdd-HHMM>。
/// `squash` = true 单层（默认）/ false 保留分层历史（--no-squash）。
async fn cmd_env_snapshot(
    podman: Podman,
    name: String,
    snapshot: Option<String>,
    squash: bool,
) -> Result<()> {
    let image_ref = podman.snapshot(&name, snapshot.as_deref(), squash).await?;
    println!("环境 {name} 快照完成：{image_ref}");
    Ok(())
}

/// 运行环境。
async fn cmd_env_start(podman: Podman, name: String) -> Result<()> {
    podman.start(&name).await?;
    println!("环境 {name} 已运行");
    Ok(())
}

/// 关闭环境（保留，可随时恢复）。
async fn cmd_env_stop(podman: Podman, name: String) -> Result<()> {
    podman.stop(&name).await?;
    println!("环境 {name} 已关闭（保留）");
    Ok(())
}

/// 环境列表。
async fn cmd_env_list(podman: Podman) -> Result<()> {
    cmd_list(podman).await
}

/// 列出可用 flavor。
fn cmd_flavor_list() -> Result<()> {
    let flavors = easytidy_core::flavor::Flavor::list()?;
    if flavors.is_empty() {
        println!(
            "没有可用 flavor（{}）",
            easytidy_core::flavor::Flavor::flavors_dir()?.display()
        );
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
    config_dir: Option<PathBuf>,
) -> Result<()> {
    let flavor = easytidy_core::flavor::Flavor::load(&flavor_name)?;
    let name = container.unwrap_or_else(|| flavor_name.clone());

    info!(
        "应用 flavor {flavor_name}：镜像 {}，容器 {name}",
        flavor.params.image
    );

    // 展开配置（GUI 透传自动注入宿主显示环境）
    let config = flavor.build_config(&name)?;

    let bins = easytidy_core::ContainerBins::resolve()?;
    let id = podman
        .create_with_config(&name, &flavor.params.image, &bins, &config)
        .await?;
    println!("容器 {name} 创建成功（ID: {}）", &id[..12.min(id.len())]);

    podman.start(&name).await?;
    // 容器内准备（fontconfig / useradd / 家目录补齐）：best-effort，失败不阻断。
    // flavor 的 setup 命令（装包等）依赖 fontconfig/用户建号已就绪——对齐
    // cmd_create / GUI（core 契约见 podman/user.rs）。
    if let Err(e) = podman
        .prepare_container(&name, &config.params)
        .await
    {
        error!("容器内准备失败（忽略，setup 可能受影响）：{e}");
    }
    println!(
        "容器 {name} 已启动，开始执行 setup（{} 条命令）...",
        flavor.setup.len()
    );

    // 经 server PTY 无头执行每条 setup 命令（run 在非 TTY 下自动跳过 raw mode）。
    // 无头安装必须非交互：注入 DEBIAN_FRONTEND=noninteractive 防 debconf 卡死。
    for (i, cmd) in flavor.setup.iter().enumerate() {
        println!("[setup {}/{}] {}", i + 1, flavor.setup.len(), cmd);
        let full = format!("export DEBIAN_FRONTEND=noninteractive TZ=UTC; {cmd}");
        // setup 以容器 root（uid 0）运行（= 装包身份，宿主侧 subuid 100000，
        // 容器文件系统属主；apt/装包需要）——经 root 通道 exec_oneshot
        let code = cmd_run(
            name.clone(),
            vec!["bash".to_string(), "-c".to_string(), full.clone()],
            true,
        )
        .await?;
        if code != 0 {
            bail!("setup 命令失败（退出码 {code}）：{cmd}");
        }
    }

    // 注册配置（entry 应用 + GUI 透传 env/mounts 落盘，供后续 run/passthrough 使用；
    // --config-dir 覆盖）
    let config_file = config_file_for(&config_dir)?;
    config_file.register_container(config)?;

    println!("✅ flavor {flavor_name} 已应用。启动容器内应用：");
    println!(
        "   easytidy run --container {name} -- {entry}{args}",
        entry = flavor
            .params
            .entry
            .clone()
            .unwrap_or_else(|| "（无 entry，可指定任意命令）".into()),
        args = flavor.params.entry_args.join(" ")
    );

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
                println!(
                    "  {} -> {} ({})",
                    m.host_path,
                    m.container_path,
                    if m.read_only { "ro" } else { "rw" }
                );
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
///
/// 确保容器运行中（未运行则 `podman.start`）。容器不存在 → Err。
async fn ensure_running(podman: &Podman, name: &str) -> Result<()> {
    let containers = podman.list_containers().await?;
    let info = containers
        .iter()
        .find(|c| c.name == name)
        .with_context(|| format!("容器不存在：{name}"))?;
    if info.status != "running" {
        info!("容器 {name} 未运行，正在启动...");
        podman.start(name).await?;
    }
    Ok(())
}

/// 轮询等待容器 server socket 就绪，返回 socket 路径。
///
/// 250ms × 40 ≈ 10s（server 先 bind 再 accept，socket 文件存在即可连；
/// 取代旧的固定 sleep(2s)——容器刚 start 时 server 未 bind 会连接失败）。
async fn wait_socket_ready(name: &str) -> Result<PathBuf> {
    let socket = easytidy_core::host_socket_path(name)?;
    // 判就绪必须试连，不能只看文件存在：容器停止/podman.service 重启后
    // 旧 socket 文件会残留在磁盘上（存在但无监听），曾致首次启动必败
    //（exists → 立即返回 → connect 被拒 Connection refused，而 server
    // 稍后才真正 listen）。连接成功即就绪（连接随即关闭，无害）。
    for _ in 0..40 {
        if socket.exists()
            && tokio::net::UnixStream::connect(&socket).await.is_ok()
        {
            return Ok(socket);
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    bail!("等待容器 server 就绪超时（{}）", socket.display())
}

/// 确保容器运行 + 等待 server socket 就绪（[`cmd_open`] / [`cmd_run`] 共用）。
async fn ensure_running_and_ready(podman: &Podman, name: &str) -> Result<PathBuf> {
    ensure_running(podman, name).await?;
    wait_socket_ready(name).await
}


/// 连接容器 server socket → hello → pty.open → 双向流式转发，
/// 阻塞至应用退出，返回其退出码（server 侧有损 0/-1，见 server pty.rs）。
///
/// [`cmd_run`]（非 root 带命令）与 [`cmd_open`]（passthrough 快捷方式）共用。
async fn forward_pty_command(socket: &Path, command: Vec<String>) -> Result<i32> {
    let mut framed = conn::connect_server(socket, "easytidy-cli").await?;

    // 获取当前终端尺寸；无 tty 场景（桌面图标/CRON 启动，ioctl 返回
    // EAGAIN——journalctl 实测）回退 80×24，不影响命令执行
    let (cols, rows) = terminal::size().unwrap_or((80, 24));

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
            !matches!(
                k.as_str(),
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
        // CLI 执行命令 = 独立会话（不接线常驻终端）
        attach: false,
        persistent: false,
        attach_stream: None,
    };

    let pty_open_msg = Message {
        id: 2,
        kind: MsgKind::Req,
        op: "pty.open".to_string(),
        payload: serde_json::to_value(pty_open)?,
        err: None,
    };
    framed
        .send(Frame::Json(pty_open_msg))
        .await
        .context("发送 pty.open 失败")?;

    // 接收 pty.open 响应
    let open_resp_frame = framed
        .next()
        .await
        .context("接收 pty.open 响应失败")?
        .context("pty.open 响应为空")?;

    let open_resp_msg = match open_resp_frame {
        Frame::Json(msg) => msg,
        Frame::Raw { .. } => bail!("pty.open 响应应为 JSON 帧（收到 Raw 帧）"),
    };
    // 错误响应优先检查（payload=null，直接 from_value 会报解析错误掩盖真实原因）
    if let Some(err) = &open_resp_msg.err {
        bail!("pty.open 失败：{} - {}", err.code, err.message);
    }

    let open_resp: PtyOpenResp =
        serde_json::from_value(open_resp_msg.payload).context("解析 pty.open 响应失败")?;

    let stream_id = open_resp.stream_id;
    debug!("PTY 打开成功：stream_id={}", stream_id);

    // 进入主循环
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

        let mut signals = Signals::new([signal_hook::consts::SIGWINCH]).expect("注册信号处理失败");

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
        // 请求 id 单调计数器（握手=1 / pty.open=2 已用；resize 从 3 自增——
        // id 是请求↔响应关联键，硬编码会撞）
        let mut next_id: u64 = 3;

        loop {
            tokio::select! {
                // 帧接收——**不带 `Some(...)` 模式**：否则 socket 关闭返回 None 时
                // 该分支被 select! 禁用，stdin/signal 永不结束 → 永久挂起。区分
                // Some(Ok)=正常帧 / Some(Err)=解码错 / None=对端关闭。
                frame_result = framed.next() => {
                    match frame_result {
                        Some(Ok(frame)) => {
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
                        Some(Err(e)) => {
                            error!("帧解码错误：{}", e);
                            break;
                        }
                        // 对端关闭（server 退出/崩溃）但未发 pty.exited——给非 0
                        // 退出码，区别于正常退出码 0（原实现会永久挂起，只有
                        // Ctrl+D 能退出且退出码还是 0）。
                        None => {
                            if exit_code.is_none() {
                                info!("server 连接关闭（未收到 pty.exited）");
                                exit_code = Some(1);
                            }
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

                        let resize_id = next_id;
                        next_id += 1;
                        let resize_msg = Message {
                            id: resize_id,
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

    // 等待退出
    let code = exit_code.await?.unwrap_or(0);
    running.store(false, Ordering::Relaxed);

    // 清理（仅当之前成功进入 raw 模式）
    if raw_enabled {
        terminal::disable_raw_mode().context("恢复终端模式失败")?;
    }

    // 恢复终端并打印换行
    println!();

    Ok(code)
}

/// 按 `--config-dir` 覆盖解析 ConfigFile（None = 默认实例）。
/// global flag `--config-dir` 透传到所有读容器配置的命令（create/rebuild/rm/
/// env new/rm/flavor apply），避免"只有 open 生效、其余静默忽略"。
fn config_file_for(config_dir: &Option<PathBuf>) -> Result<ConfigFile> {
    match config_dir {
        Some(d) => Ok(ConfigFile::with_base_dir(d.clone())),
        None => ConfigFile::default_instance().map_err(|e| anyhow::anyhow!("{e}")),
    }
}

/// 读注册表中容器的 `silent_boot`；无配置/无条目/读失败 → false（Default 语义）。
/// `config_dir` = 容器配置根目录覆盖（`--config-dir`），缺省走 FileLoader `CONTAINERS`。
fn silent_boot_for(config_dir: Option<&Path>, name: &str) -> bool {
    let cf = match config_dir {
        Some(d) => ConfigFile::with_base_dir(d.to_path_buf()),
        None => match ConfigFile::default_instance() {
            Ok(c) => c,
            Err(_) => return false,
        },
    };
    cf.get_container(name)
        .ok()
        .flatten()
        .map(|c| c.silent_boot)
        .unwrap_or(false)
}

/// 弹出终端窗口显示友好错误（桌面快捷点击无 TTY，stderr 用户看不到）。
///
/// 探测顺序：xterm → konsole → gnome-terminal → xfce4-terminal →
/// notify-send → stderr 兜底。快捷方式拉起的 CLI 带完整会话 PATH，
/// `spawn` 成功即视为可用（NotFound 则试下一个）。
fn show_error_in_terminal(message: &str) {
    // 单引号串内转义：`'` → `'\''`（闭合单引号 + 转义单引号 + 重开单引号）
    // ——`\'` 在单引号内是字面量（非法转义），会让脚本变成未闭合引号。
    let script = format!(
        "printf '%s\\n' '{}'; read -r _",
        message.replace('\'', r"'\''")
    );
    let attempts: &[(&str, &[&str])] = &[
        ("xterm", &["-hold", "-e", "sh", "-c"]),
        ("konsole", &["-e", "sh", "-c"]),
        ("gnome-terminal", &["--", "sh", "-c"]),
        ("xfce4-terminal", &["-x", "sh", "-c"]),
    ];
    for (term, prefix) in attempts {
        let mut cmd = std::process::Command::new(term);
        cmd.args(*prefix).arg(&script);
        match cmd.spawn() {
            Ok(_) => return,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => continue,
        }
    }
    // 无终端 → 桌面通知兜底
    let mut notify = std::process::Command::new("notify-send");
    notify.args(["-u", "critical", "easytidy", message]);
    let _ = notify.status();
    eprintln!("{message}");
}

/// 桌面快捷方式统一入口（垫片，三段式语义）。
///
/// - 容器不存在/已删除 → 友好终端报错 + exit 1
/// - 未运行 → 先拉起（auto_start 应用由容器内 server 自拉起，见
///   server `launch_auto_start`）
/// - 无命令（容器入口）：`silent_boot` = true 仅保活不弹 GUI；
///   false = 等 socket 就绪后 spawn Worker GUI（detach）
/// - 带命令（passthrough 应用）：经 server PTY 转发（[`forward_pty_command`]），
///   阻塞至应用退出，返回退出码
async fn cmd_open(
    container: String,
    command: Vec<String>,
    config_dir: Option<PathBuf>,
) -> Result<i32> {
    info!("打开容器：container={container}, cmd={command:?}");

    let podman = Podman::connect().await?;

    // ① 容器不存在（未创建/已删除）：友好终端报错
    if !podman
        .list_containers()
        .await?
        .iter()
        .any(|c| c.name == container)
    {
        show_error_in_terminal(&format!(
            "容器 {container} 不存在（可能已删除或尚未创建）。\n\n\
             可通过 easytidy GUI（Master）或命令行创建：\n\
             easytidy create --image <镜像> --name {container}"
        ));
        bail!("容器不存在：{container}");
    }

    // ② 确保运行 + 等待 server socket 就绪
    let socket = ensure_running_and_ready(&podman, &container).await?;

    // ③ 分支
    if command.is_empty() {
        if silent_boot_for(config_dir.as_deref(), &container) {
            // 静默：只保证容器在跑（auto_start 应用由 server 自拉起）
            info!("silent_boot：仅确保容器 {container} 运行（不弹 GUI）");
            return Ok(0);
        }
        // 非静默：拉起 Worker GUI（detached），复用 cmd_boot 的二进制探测。
        // 找不到 Worker GUI：**降级保活**——容器已拉起，桌面点击至少生效；
        // 弹终端可见地报告缺失原因（直接 error 会在桌面启动场景下无声失败：
        // 用户看不到 stderr，曾致「点了没反应」）。重新导出前需重装/补齐
        // easytidy-gui（与 CLI 同目录或 PATH）。
        match gui_binary_path() {
            Some(gui_bin) => {
                std::process::Command::new(&gui_bin)
                    .arg("--container")
                    .arg(&container)
                    .spawn()
                    .map_err(|e| anyhow::anyhow!("拉起 Worker GUI 失败：{e}"))?;
                info!("已拉起 Worker GUI（{container}）");
            }
            None => {
                warn!("找不到 easytidy-gui 可执行文件：容器 {container} 已保活，但无法打开管理窗口");
                show_error_in_terminal(
                    "找不到 easytidy-gui 可执行文件（Worker GUI 未安装或不在 CLI 同目录/PATH）。\n\n\
                     容器已启动；请补齐 easytidy-gui 后重试，或用 easytidy GUI（Master）打开。",
                );
            }
        }
        return Ok(0);
    }

    // 透传应用命令（阻塞至应用退出，退出码透传）
    forward_pty_command(&socket, command).await
}

/// 按引用启动容器内应用（[`Commands::Launch`]）：
/// 确保容器运行 + socket 就绪 → `apps.launch_app` one-shot op。server 查
/// 应用登记表/自定义应用解析出 exec 并自行 spawn（stdio 缓冲 + 退出码记录，
/// 可经 apps.ps / apps.logs 跟踪）；CLI 只报结果即退出。
///
/// 与 [`cmd_open`] 带命令路径（PTY 阻塞转发）的区别：launch 是「点火即走」
/// ——桌面图标点击应秒回，应用生命周期归 server 所有。
async fn cmd_launch(container: String, id_or_name: String) -> Result<i32> {
    info!("launch：container={container}, app={id_or_name}");

    let podman = Podman::connect().await?;

    // 容器不存在（未创建/已删除）：友好终端报错
    if !podman
        .list_containers()
        .await?
        .iter()
        .any(|c| c.name == container)
    {
        show_error_in_terminal(&format!(
            "容器 {container} 不存在（可能已删除或尚未创建）。\n\n\
             可通过 easytidy GUI（Master）或命令行创建：\n\
             easytidy create --image <镜像> --name {container}"
        ));
        bail!("容器不存在：{container}");
    }

    let socket = ensure_running_and_ready(&podman, &container).await?;
    let framed = conn::connect_server(&socket, "easytidy-cli").await?;

    let req = easytidy_protocol::ops::AppLaunchApp { id_or_name };
    let payload = conn::send_json_op(framed, "apps.launch_app", &req).await?;
    let resp: easytidy_protocol::ops::AppLaunchAppResp =
        serde_json::from_value(payload).context("解析 apps.launch_app 响应失败")?;

    match resp.pid {
        Some(pid) => {
            println!(
                "已启动 {}（id={}, pid={pid}）",
                resp.name.unwrap_or_default(),
                resp.id.unwrap_or_default()
            );
            Ok(0)
        }
        None => {
            let err = resp.error.unwrap_or_else(|| "启动失败".to_string());
            if let Some(available) = &resp.available {
                if !available.is_empty() {
                    eprintln!("可用应用 id：{}", available.join(", "));
                }
            }
            Err(anyhow::anyhow!("{err}"))
        }
    }
}

async fn cmd_run(container: String, command: Vec<String>, as_root: bool) -> Result<i32> {
    info!("运行容器内命令：container={}, cmd={:?}", container, command);

    let podman = Podman::connect().await?;

    // root 身份：容器 server 无 root（新身份模型）→ 走宿主 root 通道：
    // 无命令 = 附接共享 root 终端（与 GUI 同屏，退出 = detach）；
    // 带命令 = 一次性 root exec（exec_oneshot）
    if as_root {
        return cmd_run_root(&podman, &container, command).await;
    }

    // 确保运行 + 等 server socket 就绪（修复旧的固定 sleep(2s) 竞态）
    let socket = ensure_running_and_ready(&podman, &container).await?;
    info!("连接到 socket：{}", socket.display());

    // 经 server PTY 转发命令（阻塞至应用退出，退出码透传）
    forward_pty_command(&socket, command).await
}

/// 以容器 root 身份运行。
///
/// 新身份模型下容器 server 不再是 root（协议 v2 删 as_root），root 走
/// **容器内 root 通道**（`easytidy-dock` daemon，exec --user 0 拉起）：
/// - **无命令** = 附接 root 终端（与 GUI root 终端同屏互见；
///   CLI 退出 = **detach**，会话继续运行）
/// - **带命令** = 一次性 root exec（`exec_oneshot`，拿真实退出码）
async fn cmd_run_root(podman: &Podman, container: &str, command: Vec<String>) -> Result<i32> {
    // 确保容器运行中（root 通道有自己的就绪管理——ensure_running 足够，
    // 无需等 server socket：exec_oneshot 走 podman socket 而非容器内 server）
    ensure_running(podman, container).await?;

    // 带命令 = 一次性 root exec
    if !command.is_empty() {
        let out = podman.exec_oneshot(container, "0", command).await?;
        if !out.stdout.is_empty() {
            print!("{}", out.stdout);
        }
        if !out.stderr.is_empty() {
            eprint!("{}", out.stderr);
        }
        return Ok(out.code);
    }

    // 无命令 = 附接 root 终端（容器内 root 通道）
    cmd_run_root_attach(podman, container).await
}

/// 从 `client list` 的 stdout（RcListResp JSON `{"sessions":[{id,alive,spawn_pid},...]}`）
/// 解析首个 alive session 的 id。无 alive session / 解析失败 → None（回退 `client new`）。
fn parse_alive_session_id(stdout: &str) -> Option<u64> {
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    let sessions = v.get("sessions")?.as_array()?;
    sessions
        .iter()
        .find(|s| s.get("alive").and_then(|a| a.as_bool()) == Some(true))
        .and_then(|s| s.get("id").and_then(|i| i.as_u64()))
}

/// 附接 root 终端（容器内 root 通道）。
///
/// 新设计（2026-09-01）：root 通道（easytidy-dock daemon）跑在**容器内**。本函数：
/// 1. 确保容器运行（`ensure_running`）
/// 2. bootstrap daemon（`podman exec --user 0 <bin> bootstrap`，未跑则 spawn）
/// 3. `exec_pty` 起 client（`<bin> client new`），client 桥 stdio 到 daemon PTY
/// 4. 双向桥：CLI stdin→exec.input、exec.output→CLI stdout、SIGWINCH→resize_exec_pty
///
/// CLI 退出 = **detach**（drop exec stream；client exit，bash 状态由 daemon 保留）。
async fn cmd_run_root_attach(podman: &Podman, container: &str) -> Result<i32> {
    let (cols, rows) = terminal::size().unwrap_or((80, 24));

    // Step 1: 确保容器运行
    ensure_running(podman, container).await?;

    // Step 2: bootstrap daemon（如未跑）。exec 必须用**容器内路径**
    // `/run/easytidy-bin/easytidy-dock`（宿主相对路径 runc stat 不到）。
    let dock_bin = easytidy_core::podman::Podman::DOCK_TARGET;
    bootstrap_dock_daemon(podman, container, dock_bin).await?;

    // Step 3: 幂等 attach（对齐 GUI：每容器一个 root bash）——先 `client list`
    // 查 alive session，有则 attach 复用（daemon fan-out）、无则 new。原实现无条件
    // `client new`，每次 detach 退出后重进都新增一个 root bash，无限累积。
    let list_cmd = vec![
        dock_bin.to_string(),
        "client".to_string(),
        "list".to_string(),
    ];
    let list_out = podman.exec_oneshot(container, "0", list_cmd).await?;
    let client_cmd = match parse_alive_session_id(&list_out.stdout) {
        Some(sid) => {
            info!("复用已有 root session {sid}");
            vec![
                dock_bin.to_string(),
                "client".to_string(),
                "attach".to_string(),
                "--session-id".to_string(),
                sid.to_string(),
                "--cols".to_string(),
                cols.to_string(),
                "--rows".to_string(),
                rows.to_string(),
            ]
        }
        None => {
            info!("无存活 root session，新建");
            vec![
                dock_bin.to_string(),
                "client".to_string(),
                "new".to_string(),
                "--cols".to_string(),
                cols.to_string(),
                "--rows".to_string(),
                rows.to_string(),
            ]
        }
    };
    let exec = podman.exec_pty(container, "0", cols, rows, client_cmd).await?;
    let exec_id = exec.exec_id.clone();
    let input = exec.input;
    let mut output = exec.output;

    // CLI 终端进入 raw 模式（交互 shell）
    let mut raw_enabled = false;
    match terminal::enable_raw_mode() {
        Ok(()) => raw_enabled = true,
        Err(e) => debug!("非 TTY 场景，跳过 raw 模式：{}", e),
    }

    // stdin → exec.input（阻塞读 + channel 转发）
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

    // SIGWINCH → resize_exec_pty（重设 exec TTY → client 收到 SIGWINCH → rc.resize → bash PTY）
    let (signal_tx, mut signal_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    std::thread::spawn(move || {
        use signal_hook::iterator::Signals;
        let mut signals = Signals::new([signal_hook::consts::SIGWINCH]).expect("注册信号处理失败");
        for _ in signals.forever() {
            if signal_tx.send(()).is_err() {
                break;
            }
        }
    });

    let mut stdout = tokio::io::stdout();
    let mut exited = false;
    loop {
        tokio::select! {
            // exec output（client stdout，即 bash 输出）→ CLI stdout
            item = output.next() => {
                match item {
                    Some(Ok(bytes)) => {
                        if !bytes.is_empty() {
                            if let Err(e) = stdout.write_all(&bytes).await {
                                error!("写入 stdout 失败：{}", e);
                                break;
                            }
                            let _ = stdout.flush().await;
                        }
                    }
                    Some(Err(e)) => {
                        error!("root 通道读取错误：{}", e);
                        break;
                    }
                    None => {
                        info!("root 通道连接关闭（client exit / bash 退出）");
                        exited = true;
                        break;
                    }
                }
            }
            // stdin → exec.input
            Some(data) = stdin_rx.recv() => {
                use tokio::io::AsyncWriteExt;
                let mut w = input.lock().await;
                if let Err(e) = w.write_all(&data).await {
                    debug!("写 exec.input 失败：{e}");
                    break;
                }
            }
            // SIGWINCH → resize_exec_pty
            Some(()) = signal_rx.recv() => {
                if let Ok((c, r)) = terminal::size() {
                    if let Err(e) = podman.resize_exec_pty(&exec_id, c, r).await {
                        debug!("resize_exec_pty 失败：{e}");
                    }
                }
            }
        }
    }

    // CLI 退出 = detach：drop exec stream → client exit，bash 状态由 daemon 保留
    if raw_enabled {
        terminal::disable_raw_mode().context("恢复终端模式失败")?;
    }
    println!();
    if exited {
        info!("root 会话已退出");
    } else {
        info!("已 detach（root 会话继续运行，GUI root 终端同屏可见）");
    }
    Ok(0)
}

/// 查看容器内 easytidy-dock 日志（`/run/easytidy/dock.log`）。
/// 诊断 root 终端"静默失败"：daemon stderr 被 /dev/null，唯一线索在此文件。
async fn cmd_dock_logs(container: &str, tail: usize) -> Result<()> {
    let podman = Podman::connect().await?;
    let cmd = vec![
        "tail".to_string(),
        "-n".to_string(),
        tail.to_string(),
        "/run/easytidy/dock.log".to_string(),
    ];
    let out = podman
        .exec_oneshot(container, "0", cmd)
        .await
        .context("读取 easytidy-dock 日志失败（容器运行中？tail 可用？）")?;
    if out.stdout.trim().is_empty() {
        println!("(easytidy-dock 日志为空——daemon 可能未启动或无输出)");
    } else {
        print!("{}", out.stdout);
    }
    if !out.stderr.trim().is_empty() {
        eprint!("{}", out.stderr);
    }
    Ok(())
}

/// 在容器内启动 easytidy-dock daemon（如未运行）。
async fn bootstrap_dock_daemon(
    podman: &Podman,
    container: &str,
    dock_bin: &str,
) -> Result<()> {
    // 先探测：client ping 看 daemon 是否活着
    let ping_cmd = vec![
        dock_bin.to_string(),
        "client".to_string(),
        "ping".to_string(),
    ];
    let probe = podman
        .exec_oneshot(container, "0", ping_cmd.clone())
        .await
        .map(|o| o.stdout.contains("alive: true"))
        .unwrap_or(false);
    if probe {
        return Ok(());
    }

    // 未跑 → bootstrap（spawn --daemon 子进程，bootstrap 本身 fire-and-forget 退出）
    let boot_cmd = vec![
        dock_bin.to_string(),
        "bootstrap".to_string(),
    ];
    podman
        .exec_oneshot(container, "0", boot_cmd)
        .await
        .context("bootstrap easytidy-dock daemon 失败")?;

    // 等 daemon 就绪
    for _ in 0..40 {
        let alive = podman
            .exec_oneshot(container, "0", ping_cmd.clone())
            .await
            .map(|o| o.stdout.contains("alive: true"))
            .unwrap_or(false);
        if alive {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    bail!("easytidy-dock daemon 未就绪（2s 超时）")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_silent_boot_for_missing_file() {
        // 无配置文件 → false（Default 语义）
        assert!(!silent_boot_for(None, "nosuch"));
    }

    #[test]
    fn test_silent_boot_for_reads_config() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path().to_path_buf();
        let cf = ConfigFile::with_base_dir(base.clone());
        cf.register_container(ContainerConfig {
            name: "c1".to_string(),
            params: easytidy_core::models::ContainerParams {
                image: "alpine:latest".to_string(),
                ..Default::default()
            },
            silent_boot: true,
            ..Default::default()
        })
        .unwrap();

        assert!(
            silent_boot_for(Some(&base), "c1"),
            "silent_boot=true 应读回 true"
        );
        assert!(!silent_boot_for(Some(&base), "c2"), "未注册容器 → false");
    }
}
