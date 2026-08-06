//! easytidy 宿主侧核心引擎。
//!
//! 被三个入口共用：中心化 GUI、per-容器 GUI、无头 CLI。
//! 职责：podman socket API 客户端（bollard + libpod 端点）、
//! 容器生命周期/快照、共享配置文件（原子写 + flock + 版本化）、
//! .desktop 生成、宿主 systemd unit 生成。
//!
//! 铁律：不调用任何 podman CLI（见 docs/08-requirements.md L2）。

/// 宿主导出 .desktop 的 Exec 前缀（passthrough 机制）
pub const EXEC_PREFIX: &str = "easytidy --container";

pub mod configfile;
pub mod desktop;
pub mod error;
pub mod events;
pub mod models;
pub mod podman;
pub mod systemd;
