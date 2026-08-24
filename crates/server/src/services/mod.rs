//! 服务层：按协议消息族划分的业务实现。
//!
//! - pty：终端（open/attach/回放/resize/close/枚举/cwd）
//! - fs：容器内文件系统
//! - apps：桌面应用枚举/图标/launch + 托管进程
//! - config：容器配置（持久层 /home/easytidy/config.json）
//! - lifecycle：entry 拉起 / shutdown
//! - passthrough：容器内 auto-start 应用配置（list/set + 启动自读拉起）

pub(crate) mod apps;
pub(crate) mod config;
pub(crate) mod fs;
pub(crate) mod lifecycle;
pub(crate) mod passthrough;
pub(crate) mod pty;
