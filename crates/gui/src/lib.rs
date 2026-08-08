//! easytidy GUI Tauri 应用。
//!
//! 命令按模块拆分在 `commands/`(common/containers/config/socket/pty/fs/apps/passthrough),
//! 托管状态在 `state`。本文件仅保留 Tauri 启动(Builder + 命令注册)。

pub mod commands;
pub mod state;

use std::collections::HashMap;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

pub use state::AppMode;

/// Tauri 启动入口。
///
/// `mode`:中心化(容器管理)或单容器(per-container 窗口,`--container <name>`)。
pub fn run(mode: AppMode, _config_file: Option<String>) {
    // 第一步：应用 NVIDIA 规避措施（在 Tauri 初始化之前）
    commands::common::apply_nvidia_workaround();

    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env()
            .add_directive("easytidy_gui=info".parse().unwrap()))
        .init();

    // 第二步：启动 Tauri 应用
    // 根据模式决定是否初始化 GuiSession
    let gui_session = match &mode {
        AppMode::Container { name } => Some(state::GuiSession {
            container_name: name.clone(),
            socket: tokio::sync::Mutex::new(None),
            next_msg_id: AtomicU64::new(2), // 握手已用 1
            active_ptys: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        }),
        AppMode::Centralized => None,
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(mode)
        .manage(state::PodmanState::new())
        .manage(gui_session)
        .invoke_handler(tauri::generate_handler![
            // 通用
            commands::common::toggle_devtools,
            commands::common::get_app_mode,
            // 中心化模式
            commands::containers::list_containers,
            commands::containers::create_container,
            commands::containers::start_container,
            commands::containers::stop_container,
            commands::containers::restart_container,
            commands::containers::remove_container,
            commands::containers::inspect_container,
            commands::containers::build_server,
            commands::containers::open_container_window,
            commands::containers::container_shutdown,
            // 环境（env）语义
            commands::containers::flavor_list,
            commands::containers::env_list,
            commands::containers::env_new,
            commands::containers::env_rm,
            commands::containers::env_snapshot,
            commands::containers::env_fork,
            commands::containers::env_start,
            commands::containers::env_stop,
            // 配置管理器
            commands::config::get_container_config,
            commands::config::apply_container_config,
            // PTY 终端
            commands::pty::pty_open,
            commands::pty::pty_write,
            commands::pty::pty_resize,
            commands::pty::pty_close,
            commands::pty::pty_ping,
            commands::pty::pty_cwd,
            // 文件系统 + 传输
            commands::fs::fs_list,
            commands::fs::fs_read,
            commands::fs::fs_write,
            commands::fs::fs_copy,
            commands::fs::import_inspect,
            commands::fs::import_files,
            commands::fs::fetch_file_b64,
            commands::fs::export_file_dialog,
            // 桌面应用
            commands::apps::apps_list,
            // Passthrough
            commands::passthrough::passthrough_state,
            commands::passthrough::passthrough_export,
            commands::passthrough::passthrough_revoke,
            commands::passthrough::passthrough_set_auto_start,
            commands::passthrough::passthrough_add_custom,
            commands::passthrough::passthrough_remove_app,
            commands::passthrough::export_gui_shortcut,
            // 配置（容器内 server 配置读写）
            commands::passthrough::config_get,
            commands::passthrough::config_set,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
