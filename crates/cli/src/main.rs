//! easytidy 无头 one-shot CLI（M1 实现）。
//!
//! 子命令：
//! - list: 列出所有容器
//! - create: 创建新容器
//! - start: 启动容器
//! - stop: 停止容器
//! - restart: 重启容器
//! - rm: 删除容器
//! - inspect: 检查容器详情

use std::path::PathBuf;
use clap::{Parser, Subcommand};
use anyhow::{bail, Result};
use tracing::{info, error};
use tracing_subscriber::EnvFilter;

use easytidy_core::podman::Podman;
use easytidy_core::configfile::ConfigFile;
use easytidy_core::models::ContainerConfig;

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

    /// 静默启动（stub for M1）
    Boot {
        /// 配置文件路径（由 systemd unit 传入）
        #[arg(long)]
        config: Option<PathBuf>,
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

    // 连接 podman
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

    // 执行子命令
    match cli.command {
        Commands::List => cmd_list(podman).await,
        Commands::Create { image, name, home, volume } => {
            cmd_create(podman, image, name, home, volume).await
        }
        Commands::Start { container } => cmd_start(podman, container).await,
        Commands::Stop { container } => cmd_stop(podman, container).await,
        Commands::Restart { container } => cmd_restart(podman, container).await,
        Commands::Rm { container, force } => cmd_rm(podman, container, force).await,
        Commands::Inspect { container } => cmd_inspect(podman, container).await,
        Commands::Boot { config: _ } => {
            info!("静默启动（stub）：M1 占位，待 M2 实现");
            Ok(())
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

    // M1 临时方案：使用 /bin/sh 作为 stub server（M2 会使用真实的 server 二进制）
    let server_bin = PathBuf::from("/bin/sh");

    // 创建容器
    match podman.create(&name, &image, &server_bin).await {
        Ok(id) => {
            println!("容器 {} 创建成功（ID: {}）", name, id);

            // 注册到配置文件
            let config_path = ConfigFile::default_path()?;
            let config_file = ConfigFile::with_path(config_path);

            let container_config = ContainerConfig {
                name: name.clone(),
                image: image.clone(),
                entry: None,
                silent_boot: false,
                persistent: true,
            };

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

/// 检查容器详情（M1：复用 list_containers 投影；M2 起补完整 inspect）。
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

    Ok(())
}
