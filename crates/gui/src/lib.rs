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

    // 第二步：启动 Tauri 应用
    // 根据模式决定是否初始化 GuiSession
    let gui_session = match &mode {
        AppMode::Worker { name } => Some(state::GuiSession {
            container_name: name.clone(),
            socket: Arc::new(tokio::sync::Mutex::new(None)),
            conn_state: Arc::new(std::sync::Mutex::new(state::ConnectionState::Unconnected)),
            session_id: std::sync::Mutex::new(None),
            next_msg_id: Arc::new(AtomicU64::new(2)), // 握手已用 1
            pending: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            active_ptys: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            root_sink: Arc::new(tokio::sync::Mutex::new(None)),
            root_attach_lock: Arc::new(tokio::sync::Mutex::new(())),
        }),
        AppMode::Master => None,
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        // 注入全局 AppHandle（后台任务 emit 前端事件用）
        .setup(|app| {
            state::set_app_handle(app.handle().clone());
            Ok(())
        })
        .manage(mode)
        .manage(state::PodmanState::new())
        .manage(gui_session)
        // 图标中转协议：<img src="icon://<绝对路径>">。原本 Tauri 应用经 file:// 直读
        // 宿主机图片；现在图标统一存容器内（宿主路径不存在）→ 后端中转：先读宿主文件，
        // 宿主不存在再走容器 server socket 拉取；XPM/BMP/ICO/TIFF 后端转 PNG
        // （to_browser_renderable）。替代早期 file://（宿主）/ server HTTP 静态托管
        // （rootless 下端口宿主不可达）/ base64 方案。
        .register_asynchronous_uri_scheme_protocol("icon", |ctx, request, responder| {
            let app = ctx.app_handle().clone();
            let path = request.uri().path().to_string();
            tauri::async_runtime::spawn(async move {
                let response = match commands::fs::read_icon_bytes(&app, &path).await {
                    Ok(raw) => {
                        let (bytes, mime) = easytidy_core::icon::to_browser_renderable(&raw, &path);
                        tauri::http::Response::builder()
                            .header(tauri::http::header::CONTENT_TYPE, mime)
                            .body(bytes)
                            .unwrap()
                    }
                    Err(e) => {
                        tracing::debug!("icon:// 未找到或读取失败 {path}: {e}");
                        tauri::http::Response::builder()
                            .status(tauri::http::StatusCode::NOT_FOUND)
                            .body(Vec::new())
                            .unwrap()
                    }
                };
                responder.respond(response);
            });
        })
        .invoke_handler(tauri::generate_handler![
            // 通用
            commands::common::toggle_devtools,
            commands::common::get_app_mode,
            // 环境信息（下层引擎 / 存储驱动 只读快照）
            commands::common::engine_info,
            // Master GUI（容器管理 + 模板 + 镜像 + 配置）
            commands::containers::list_containers,
            commands::containers::start_container,
            commands::containers::stop_container,
            commands::containers::restart_container,
            commands::containers::remove_container,
            commands::containers::inspect_container,
            commands::containers::container_failure_info,
            commands::containers::open_container_window,
            commands::containers::open_container_vscode,
            commands::containers::open_container_terminal,
            commands::containers::container_shutdown,
            // 镜像管理
            commands::containers::images_list,
            commands::containers::image_pull,
            commands::containers::image_remove,
            commands::containers::images_used_by,
            commands::containers::rebuild_images_scan,
            commands::containers::rebuild_images_cleanup,
            // 环境（env）语义
            commands::containers::env_list,
            commands::containers::env_copy_config,
            commands::containers::env_new,
            commands::containers::env_rebuild,
            commands::containers::env_rm,
            commands::containers::env_snapshot,
            commands::containers::env_start,
            commands::containers::env_stop,
            // 容器入口图标选择（宿主文件选择 → icons 目录）
            commands::passthrough::container_pick_icon,
            commands::passthrough::container_entry_icon,
            // 容器入口图标源持久化（跨会话：导出时写 config.icon，挂载时预填输入框）
            commands::passthrough::container_entry_icon_source,
            commands::passthrough::container_entry_config,
            commands::passthrough::container_set_entry_name,
            commands::passthrough::container_set_entry_icon,
            // 桌面快捷方式管理（纯宿主侧：扫描/移除/图标重编/预览）
            commands::desktop_icons::desktop_icons_scan,
            commands::desktop_icons::desktop_icons_remove,
            commands::desktop_icons::desktop_icons_pick,
            commands::desktop_icons::desktop_icons_set_icon,
            commands::desktop_icons::host_file_b64,
            // 配置管理器（单实例 GUI：get/apply）
            commands::config::get_container_config,
            commands::config::apply_container_config,
            // 配置编辑器 YAML 桥 + conf 模板管理（GUI 全面切 YAML 后取代 flavor）
            commands::config::conf_parse,
            commands::config::conf_load_dialog,
            commands::config::conf_save_dialog,
            commands::config::conf_examples,
            commands::config::mount_pick_host_dir,
            commands::config::list_host_path_suggestions,
            commands::config::list_user_resource_dirs,
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
            commands::root::root_terminal_status,
            commands::root::root_terminal_attach,
            commands::root::root_terminal_write,
            commands::root::root_terminal_resize,
            commands::root::root_terminal_detach,
            commands::root::root_terminal_close,
            commands::root::root_session_list,
            commands::root::dock_logs,
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
            commands::passthrough::passthrough_update_custom,
            commands::passthrough::passthrough_remove_app,
            commands::passthrough::export_gui_shortcut,
            // 图标（自定义应用：宿主机选用 → 复制进容器；路径输入；存容器内）
            commands::passthrough::passthrough_pick_host_icon,
            commands::passthrough::passthrough_set_custom_icon,
            // 容器 server 信息（home_dir 定位容器内用户可写资源）
            commands::passthrough::server_info,
            // 收藏（pin 到工具栏）
            commands::passthrough::passthrough_set_pinned,
            commands::passthrough::passthrough_launch,
            // 立即拉起容器内任意应用（Passthrough 管理器列表，无需先收藏）
            commands::passthrough::passthrough_launch_app,
            // 应用控制台（受管子进程 stdio 日志 / 进程状态，前端轮询）
            commands::passthrough::app_logs,
            commands::passthrough::app_ps,
            commands::passthrough::app_kill,
            // 容器自启动（systemd user unit）
            commands::passthrough::passthrough_set_boot_mode,
            // 配置（容器内 server 配置读写）
            commands::passthrough::config_get,
            commands::passthrough::config_set,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
