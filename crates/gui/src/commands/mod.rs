//! GUI Tauri 命令模块。
//!
//! - common:通用(NVIDIA 规避/devtools/应用模式)
//! - containers: Master GUI 容器生命周期 + env 语义 + flavor
//! - config:配置管理器(读取/应用)
//! - socket:容器 server socket 连接与请求
//! - pty:终端(attach 常驻会话)
//! - root:root 终端(宿主 root 通道,每容器共享 root shell)
//! - fs:文件系统 + 宿主↔容器传输
//! - apps:桌面应用枚举
//! - passthrough:宿主 .desktop 导出/auto-start
//! - desktop_icons:桌面快捷方式管理（纯宿主侧扫描/移除/图标重编）

pub mod apps;
pub mod common;
pub mod config;
pub mod containers;
pub mod desktop_icons;
pub mod fs;
pub mod passthrough;
pub mod pty;
pub mod root;
pub mod socket;
