//! easytidy-ctool —— 容器内 root 一次性工具。
//!
//! 由宿主 ro bind-mount 进容器（`/usr/bin/easytidy-ctool`），与
//! easytidy-server 并列的第二个容器内二进制。以 root（exec --user 0）
//! 运行，**零容器内命令依赖**（无 sh/useradd/sed/awk）：全部逻辑为纯
//! Rust（`easytidy_core::incontainer`），alpine/busybox/debian 通吃。
//!
//! 宿主侧调用（core::podman::user::prepare_container）：
//!   exec_oneshot(container, "0", ["easytidy-ctool", "prepare", ...])
//!
//! 子命令面向「以 root 身份进入容器后的容器内操作」切面，后续可扩展
//! （如 run -- <cmd> 直 exec 容器内二进制并透传退出码）。

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "easytidy-ctool", about = "easytidy 容器内 root 一次性工具")]
struct Ctool {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// 容器内准备：fontconfig 接入 + 建号（可选）+ 家目录补齐（幂等）
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
}

fn main() {
    let ctool = Ctool::parse();
    let code = match run(ctool.command) {
        Ok(report) => {
            if let Some(reason) = report {
                eprintln!("{reason}");
            }
            0
        }
        Err(e) => {
            eprintln!("easytidy-ctool 失败：{e:#}");
            1
        }
    };
    std::process::exit(code);
}

/// 执行子命令。返回 `Some(提示)` = 非致命警告（输出 stderr，退出码 0）。
fn run(command: Commands) -> anyhow::Result<Option<String>> {
    match command {
        Commands::Prepare { uid, gid, name } => {
            let report =
                easytidy_core::incontainer::prepare_in_container(uid, gid, name.as_deref())
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
            Ok(report.skip_reason)
        }
    }
}
