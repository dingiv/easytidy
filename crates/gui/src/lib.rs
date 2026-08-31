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
/// `mode`:Master(总控)或 Worker(单容器窗口,`--container <name>`)。
pub fn run(mode: AppMode, _config_file: Option<String>) {
    // 第一步：应用 NVIDIA 规避措施（在 Tauri 初始化之前）
    commands::common::apply_nvidia_workaround();

    // 初始化日志：stdout + 文件双写。
    // 桌面启动的 GUI 无终端（stdout 丢失），文件是唯一持久日志出口——
    // 容器创建/启动链路多步易错，排障依赖完整日志。位置：
    // ~/.easytidy/logs/easytidy-gui.log（单文件追加，手动清理即可）
    {
        use tracing_subscriber::prelude::*;

        let filter = tracing_subscriber::EnvFilter::from_default_env()
            .add_directive("easytidy_gui=info".parse().unwrap())
            .add_directive("easytidy_core=info".parse().unwrap())
            .add_directive("easytidy=info".parse().unwrap());
        let file_layer = easytidy_core::appdata::app_data_dir().ok().map(|dir| {
            let dir = dir.join("logs");
            let _ = std::fs::create_dir_all(&dir);
            let appender = tracing_appender::rolling::never(&dir, "easytidy-gui.log");
            tracing_subscriber::fmt::layer()
                .with_writer(appender)
                .with_ansi(false)
        });
        tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer())
            .with(file_layer)
            .init();
    }

    // 存量桌面快捷方式迁移：旧 Exec（GUI 直连 / `easytidy run`）重写为 CLI
    // 垫片 `easytidy open` 格式。幂等且便宜（只扫两个目录、只改 easytidy-*.desktop），
    // Master/Worker 都跑。旧格式升级后仍可用，迁移是补齐而非防坏。
    {
        let n = easytidy_core::desktop::migrate_shortcuts(
            &commands::passthrough::cli_path(),
        );
        if n > 0 {
            tracing::info!("已迁移 {n} 个桌面快捷方式到 CLI 垫片格式");
        }
    }

    // 第二步：启动 Tauri 应用
    // 根据模式决定是否初始化 GuiSession
    let gui_session = match &mode {
        AppMode::Worker { name } => Some(state::GuiSession {
            container_name: name.clone(),
            socket: tokio::sync::Mutex::new(None),
            conn_state: std::sync::Mutex::new(state::ConnectionState::Unconnected),
            session_id: std::sync::Mutex::new(None),
            next_msg_id: AtomicU64::new(2), // 握手已用 1
            active_ptys: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            root_sink: Arc::new(tokio::sync::Mutex::new(None)),
        }),
        AppMode::Master => None,
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
            // Master GUI（容器管理 + 模板 + 镜像 + 配置）
            commands::containers::list_containers,
            commands::containers::start_container,
            commands::containers::stop_container,
            commands::containers::restart_container,
            commands::containers::remove_container,
            commands::containers::inspect_container,
            commands::containers::open_container_window,
            commands::containers::container_shutdown,
            // 镜像管理
            commands::containers::images_list,
            commands::containers::image_pull,
            commands::containers::image_remove,
            commands::containers::images_used_by,
            // 环境（env）语义
            commands::containers::env_list,
            commands::containers::env_new,
            commands::containers::env_rebuild,
            commands::containers::env_rm,
            commands::containers::env_snapshot,
            commands::containers::env_start,
            commands::containers::env_stop,
            // 模板派生清单（conf 模板按 container.flavor 字段分组）
            commands::containers::template_lineage,
            // 配置管理器（单实例 GUI：get/apply/sync + 血缘）
            commands::config::get_container_config,
            commands::config::apply_container_config,
            commands::config::config_sync_from_template,
            // 配置编辑器 YAML 桥 + conf 模板管理（GUI 全面切 YAML 后取代 flavor）
            commands::config::conf_parse,
            commands::config::conf_load_dialog,
            commands::config::conf_save_dialog,
            commands::config::conf_examples,
            commands::config::mount_pick_host_dir,
            commands::config::list_host_path_suggestions,
            commands::config::conf_templates,
            commands::config::conf_template_get,
            commands::config::conf_template_expand,
            commands::config::passthrough_preview,
            commands::config::server_injected_env,
            commands::config::conf_save_template,
            commands::config::conf_rm_template,
            commands::config::conf_duplicate_template,
            // PTY 终端
            commands::pty::get_terminals,
            commands::pty::pty_open,
            commands::pty::pty_write,
            commands::pty::pty_resize,
            commands::pty::pty_close,
            commands::pty::pty_ping,
            commands::pty::pty_cwd,
            // root 终端（宿主 root 通道：共享 root shell）
            commands::root::root_channel_ensure,
            commands::root::root_terminal_status,
            commands::root::root_terminal_attach,
            commands::root::root_terminal_write,
            commands::root::root_terminal_resize,
            commands::root::root_terminal_close,
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
            // 图标（自定义应用：宿主选择 / 容器选择 → ~/.easytidy/icons）
            commands::passthrough::passthrough_pick_host_icon,
            commands::passthrough::passthrough_import_container_icon,
            commands::passthrough::passthrough_set_custom_icon,
            // 收藏自定义应用图标：宿主路径存在性探测（缺失 → 显示「图标异常」）
            commands::passthrough::fs_host_exists,
            // 收藏（pin 到工具栏）
            commands::passthrough::passthrough_set_pinned,
            commands::passthrough::passthrough_launch,
            // 立即拉起容器内任意应用（Passthrough 管理器列表，无需先收藏）
            commands::passthrough::passthrough_launch_app,
            // 容器自启动（systemd user unit）
            commands::passthrough::passthrough_set_boot_mode,
            // 配置（容器内 server 配置读写）
            commands::passthrough::config_get,
            commands::passthrough::config_set,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
