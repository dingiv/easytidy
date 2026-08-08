//! Passthrough 命令：宿主 .desktop 导出/撤销/auto-start/自定义应用。

use tracing::info;

use easytidy_protocol::ops::{
        CfgGet, CfgGetResp, CfgSet,
    };

use crate::state::GuiSession;
use crate::commands::socket::send_json_request;
use crate::commands::apps::AppInfoFrontend;
use crate::commands::fs::fetch_container_file;

/// easytidy 品牌图标（导出图标水印；内嵌 PNG）
const EASYTIDY_BRAND_ICON: &[u8] = include_bytes!("../../icons/easytidy256x256.png");

// ============================================================================
// Passthrough 命令（宿主本地实现）
//
// passthrough 语义：把容器内应用导出为宿主 .desktop
// （Exec = easytidy --container <name> run -- <exec>，经 server socket 拉起
// 应用）。⚠️ 必须在宿主本地生成——server 在容器内无法写宿主 .desktop；
// M2 曾把这三个操作走 server socket（server 无 passthrough.* 操作），
// GUI 调用必报 unknown_op（2026-08-07 实测）。导出文件标记
// X-easytidy-pt=1 + X-easytidy-container=<name> + X-easytidy-app=<容器内路径>，
// 供 state 枚举与 revoke 定位。
// ============================================================================

/// 宿主 CLI 绝对路径（passthrough .desktop 的 Exec/TryExec 用）。
///
/// 探测顺序：① GUI 同目录的 easytidy（开发布局 target/debug 共存）；
/// ② 安装目录 ~/.local/share/easytidy/bin/easytidy（部署布局）；
/// ③ PATH 中的 easytidy。宿主 PATH 未必有 easytidy（实测未安装），
/// 必须给出绝对路径，否则桌面入口无法启动。
fn cli_path() -> String {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let sibling = parent.join("easytidy");
            if sibling.exists() {
                return sibling.to_string_lossy().into_owned();
            }
        }
    }
    if let Some(data) = dirs::data_local_dir() {
        let installed = data.join("easytidy/bin/easytidy");
        if installed.exists() {
            return installed.to_string_lossy().into_owned();
        }
    }
    "easytidy".to_string()
}

/// 获取 passthrough 状态（已导出 .desktop 全文 + 配置的应用条目）。
///
/// 返回（前端契约）：
/// ```json
/// { "exported": [{"desktop_file","content"}],
///   "configured_apps": [{"id","name","cmd","desktop_file"?,"auto_start","icon"?}] }
/// ```
/// `configured_apps` 是 passthrough.toml 中有状态条目（auto_start=true 或
/// custom 应用）；扫描应用若未配置则不在此（前端按 id 匹配查 auto-start）。
#[tauri::command]
pub async fn passthrough_state(
    session: tauri::State<'_, Option<GuiSession>>,
) -> Result<serde_json::Value, String> {
    let sess = session.inner().as_ref().ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    let container = &sess.container_name;

    let exported = easytidy_core::desktop::list_passthrough_detailed(container)
        .map_err(|e| e.to_string())?;
    let exported_json: Vec<_> = exported
        .into_iter()
        .map(|e| serde_json::json!({ "desktop_file": e.desktop_file, "content": e.content }))
        .collect();

    let config_file = passthrough_config_file()?;
    let apps = config_file.apps(container).map_err(|e| e.to_string())?;
    let apps_json: Vec<_> = apps
        .into_iter()
        .map(|a| {
            serde_json::json!({
                "id": a.id,
                "name": a.name,
                "cmd": a.cmd,
                "desktop_file": a.desktop_file,
                "auto_start": a.auto_start,
                "icon": a.icon,
            })
        })
        .collect();

    Ok(serde_json::json!({
        "exported": exported_json,
        "configured_apps": apps_json,
    }))
}

/// passthrough 配置文件（默认路径）
fn passthrough_config_file() -> Result<easytidy_core::passthrough::PassthroughConfigFile, String> {
    let path = easytidy_core::passthrough::PassthroughConfigFile::default_path()
        .map_err(|e| e.to_string())?;
    Ok(easytidy_core::passthrough::PassthroughConfigFile::with_path(path))
}

/// 清理 Exec 的 %U/%f 等占位符（宿主侧不展开容器内文件参数；export/toggle 共用）
fn clean_exec(exec: &str) -> String {
    exec.split_whitespace()
        .filter(|w| !w.starts_with('%'))
        .collect::<Vec<_>>()
        .join(" ")
}

/// 设置应用 auto-start（容器启动时自动拉起；随容器启动链路触发）
#[tauri::command]
pub async fn passthrough_set_auto_start(
    session: tauri::State<'_, Option<GuiSession>>,
    id: String,
    name: String,
    cmd: String,
    enabled: bool,
) -> Result<(), String> {
    let sess = session.inner().as_ref().ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    let container = &sess.container_name;

    // 保留已存应用的图标（upsert 会整体覆盖，不能丢）
    let config_file = passthrough_config_file()?;
    let existing_icon = config_file
        .apps(container)
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|a| a.id == id)
        .and_then(|a| a.icon);
    let app = easytidy_core::passthrough::PassthroughApp {
        id: id.clone(),
        name,
        cmd: clean_exec(&cmd),
        desktop_file: (!id.starts_with("custom:")).then(|| id.clone()),
        auto_start: false,
        icon: existing_icon,
    };
    config_file
        .set_auto_start(container, app, enabled)
        .map_err(|e| e.to_string())?;
    info!("passthrough auto-start 已设置：{container} enabled={enabled}");
    Ok(())
}

/// 添加自定义应用（固定目录扫描之外，如 `google-chrome-stable --disable-dev-shm-usage`）。
/// 返回 AppInfoFrontend（desktop_file=`custom:<name>`），前端直接进现有导出流。
#[tauri::command]
pub async fn passthrough_add_custom(
    session: tauri::State<'_, Option<GuiSession>>,
    name: String,
    cmd: String,
) -> Result<AppInfoFrontend, String> {
    let sess = session.inner().as_ref().ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    let config_file = passthrough_config_file()?;
    let app = config_file
        .add_custom(&sess.container_name, &name, &cmd)
        .map_err(|e| e.to_string())?;
    Ok(AppInfoFrontend {
        name: app.name,
        icon_path: app.icon,
        exec: app.cmd,
        comment: None,
        desktop_file: app.id,
        categories: None,
        startup_notify: false,
        startup_wm_class: None,
    })
}

/// 移除应用（配置条目 + 清理可能存在的导出，防孤儿）
#[tauri::command]
pub async fn passthrough_remove_app(
    session: tauri::State<'_, Option<GuiSession>>,
    id: String,
) -> Result<(), String> {
    let sess = session.inner().as_ref().ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    let container = &sess.container_name;

    let config_file = passthrough_config_file()?;
    config_file.remove_app(container, &id).map_err(|e| e.to_string())?;

    // 非 custom 且已导出 → 清理导出（防孤儿）
    if !id.starts_with("custom:") {
        let _ = easytidy_core::desktop::remove_passthrough(container, &id);
    }
    info!("passthrough 应用已移除：{container} {id}");
    Ok(())
}

// ============================================================================
// 图标命令（自定义应用图标：宿主选择 / 容器选择 → 统一落盘 ~/.easytidy/icons）
// ============================================================================

/// 把图标字节写入 `~/.easytidy/icons/`（时间戳防冲突，保留扩展名），返回宿主路径。
fn save_icon_to_appdata(bytes: &[u8], source_name: &str) -> Result<String, String> {
    let ext = std::path::Path::new(source_name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .filter(|e| !e.is_empty())
        .unwrap_or_else(|| "png".to_string());
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dir = easytidy_core::appdata::icons_dir().map_err(|e| e.to_string())?;
    let dst = dir.join(format!("icon-{secs}.{ext}"));
    std::fs::write(&dst, bytes).map_err(|e| format!("复制图标到 ~/.easytidy/icons 失败：{e}"))?;
    info!("图标已落盘：{}", dst.display());
    Ok(dst.to_string_lossy().into_owned())
}

/// 从宿主机选择图标（rfd 原生文件对话框）→ 复制到 ~/.easytidy/icons/。
/// 自定义应用图标入口之一；返回宿主本地路径（写入 passthrough.toml 的 icon）。
#[tauri::command]
pub async fn passthrough_pick_host_icon() -> Result<String, String> {
    use std::io::Read;

    // 原生对话框会阻塞主线程：spawn_blocking 避免卡 async runtime
    let picked = tokio::task::spawn_blocking(|| {
        rfd::FileDialog::new()
            .add_filter("图片", &["png", "jpg", "jpeg", "svg", "ico", "webp", "gif"])
            .pick_file()
            .map(|p| p.to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| format!("文件选择对话框失败：{e}"))?
    .ok_or_else(|| "已取消".to_string())?;

    let mut bytes = Vec::new();
    std::fs::File::open(&picked)
        .and_then(|mut f| f.read_to_end(&mut bytes))
        .map_err(|e| format!("读取宿主图标失败：{e}"))?;
    save_icon_to_appdata(&bytes, &picked)
}

/// 从容器内选择图标（前端容器文件浏览器选定路径）→ 经 server fs.read
/// 拉取 → 复制到 ~/.easytidy/icons/。自定义应用图标入口之二。
#[tauri::command]
pub async fn passthrough_import_container_icon(
    session: tauri::State<'_, Option<GuiSession>>,
    container_path: String,
) -> Result<String, String> {
    let sess = session.inner().as_ref().ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    let bytes = fetch_container_file(sess, &container_path).await?;
    save_icon_to_appdata(&bytes, &container_path)
}

/// 设置自定义应用图标（icon = 宿主 ~/.easytidy/icons 路径；None = 清除）。
/// 持久化到 passthrough.toml；导出时 Icon= 直接用该路径。
#[tauri::command]
pub async fn passthrough_set_custom_icon(
    session: tauri::State<'_, Option<GuiSession>>,
    id: String,
    icon: Option<String>,
) -> Result<(), String> {
    let sess = session.inner().as_ref().ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    let container = &sess.container_name;

    let config_file = passthrough_config_file()?;
    let mut apps = config_file.apps(container).map_err(|e| e.to_string())?;
    let Some(app) = apps.iter_mut().find(|a| a.id == id) else {
        return Err(format!("应用不存在：{id}"));
    };
    app.icon = icon.clone();
    config_file
        .upsert_app(container, app.clone())
        .map_err(|e| e.to_string())?;
    info!("自定义应用图标已设置：{container} {id} → {icon:?}");
    Ok(())
}

/// 导出 passthrough 应用（宿主生成 .desktop：应用菜单 + 桌面图标。
/// distrobox 风格 TryExec/GenericName/Keywords/Actions=Remove；桌面图标
/// 经 chmod +x + `gio metadata::trusted` 信任标记（GNOME 双击必需）。
/// 生成逻辑在 core::desktop::write_passthrough，CLI unexport 与其共用）
#[tauri::command]
pub async fn passthrough_export(
    session: tauri::State<'_, Option<GuiSession>>,
    app: AppInfoFrontend,
    // 同时创建桌面图标（GNOME 桌面默认不显示应用菜单，入口在桌面路径）
    desktop_icon: Option<bool>,
) -> Result<String, String> {
    let sess = session.inner().as_ref().ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    let container = sess.container_name.clone();

    // Exec 清理 %U/%f 等占位符（宿主侧不展开容器内文件参数）
    let exec = clean_exec(&app.exec);

    // 图标：custom 应用优先用用户选定的宿主本地图标（~/.easytidy/icons/，
    // Icon= 绝对路径直接可用），未选定回退内置品牌图标；扫描应用经
    // server fs.read 搬运容器内图标 → 合成品牌化 → ~/.easytidy/icons/ 缓存
    let mut icon_attr = None;
    if app.desktop_file.starts_with("custom:") {
        if let Some(icon) = app.icon_path.as_ref() {
            if std::path::Path::new(icon).is_file() {
                icon_attr = Some(icon.clone());
            }
        }
        if icon_attr.is_none() {
            icon_attr = easytidy_core::desktop::ensure_gui_icon();
        }
    } else if let Some(icon_path) = app.icon_path.as_ref() {
        if let Ok(icon_data) = fetch_container_file(sess, icon_path).await {
            if let Ok(icons_dir) = easytidy_core::desktop::passthrough_icon_dir() {
                if std::fs::create_dir_all(&icons_dir).is_ok() {
                    let base = app
                        .desktop_file
                        .rsplit('/')
                        .next()
                        .unwrap_or("app")
                        .trim_end_matches(".desktop");
                    let safe: String = base
                        .chars()
                        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
                        .collect();
                    let icon_file = icons_dir.join(format!("easytidy-pt-{container}-{safe}.png"));
                    // 品牌化合成:208px 内容 + 天蓝→深蓝 45° 渐变圆角边框 +
                    // 右下角 easytidy 水印(96px);合成失败回退原始图标
                    let composed =
                        easytidy_core::icon::compose_app_icon(&icon_data, EASYTIDY_BRAND_ICON);
                    let bytes = composed.unwrap_or(icon_data);
                    if std::fs::write(&icon_file, &bytes).is_ok() {
                        icon_attr = Some(icon_file.to_string_lossy().into_owned());
                    }
                }
            }
        }
    }

    let spec = easytidy_core::desktop::PassthroughSpec {
        container: container.clone(),
        app_name: app.name,
        comment: app.comment,
        categories: app.categories,
        exec,
        icon: icon_attr,
        desktop_file: app.desktop_file,
        cli_path: cli_path(),
        startup_notify: app.startup_notify,
        startup_wm_class: app.startup_wm_class,
    };
    let menu_path = easytidy_core::desktop::write_passthrough(&spec, desktop_icon.unwrap_or(true))
        .map_err(|e| e.to_string())?;
    info!("passthrough 导出：{} → {:?}", spec.desktop_file, menu_path);

    Ok(menu_path.to_string_lossy().into_owned())
}

/// 撤销 passthrough 导出（宿主删除对应 .desktop）
#[tauri::command]
pub async fn passthrough_revoke(
    session: tauri::State<'_, Option<GuiSession>>,
    desktop_file: String,
) -> Result<String, String> {
    let sess = session.inner().as_ref().ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    let removed = easytidy_core::desktop::remove_passthrough(&sess.container_name, &desktop_file)
        .map_err(|e| e.to_string())?;
    Ok(removed.to_string_lossy().into_owned())
}

/// 导出本容器的 GUI 管理界面桌面快捷方式（菜单 + 桌面图标）。
///
/// Exec = 当前 GUI 二进制 --container <name>（per-container 模式），
/// TryExec 同；内置品牌 SVG 图标；桌面副本 chmod +x + gio trusted。
/// 返回应用菜单路径。
#[tauri::command]
pub async fn export_gui_shortcut(
    session: tauri::State<'_, Option<GuiSession>>,
    desktop_icon: Option<bool>,
) -> Result<String, String> {
    let sess = session.inner().as_ref().ok_or_else(|| "当前模式不是单容器模式".to_string())?;

    // 当前进程即 GUI 二进制（per-container 模式入口）；current_exe 失败
    // 回退命令行 argv[0]，再不行报错（Exec 必须绝对路径）
    let gui_path = std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
        .or_else(|| std::env::args().next())
        .ok_or_else(|| "无法确定 GUI 可执行路径".to_string())?;

    let menu_path = easytidy_core::desktop::write_gui_entry(
        &sess.container_name,
        &gui_path,
        desktop_icon.unwrap_or(true),
    )
    .map_err(|e| e.to_string())?;
    info!(
        "GUI 入口导出：{} → {:?}",
        sess.container_name, menu_path
    );
    Ok(menu_path.to_string_lossy().into_owned())
}

// ============================================================================
// 配置管理命令
// ============================================================================

/// 获取容器配置
#[tauri::command]
pub async fn config_get(
    session: tauri::State<'_, Option<GuiSession>>,
) -> Result<serde_json::Value, String> {
    let sess = session.inner().as_ref().ok_or_else(|| "当前模式不是单容器模式".to_string())?;

    let resp = send_json_request(sess, "config.get".to_string(),
        serde_json::to_value(CfgGet).map_err(|e| e.to_string())?)
        .await.map_err(|e| e.to_string())?;

    let get_resp: CfgGetResp = serde_json::from_value(resp.payload)
        .map_err(|e| format!("解析 config.get 响应失败：{}", e))?;

    Ok(get_resp.config)
}

/// 设置容器配置项
#[tauri::command]
pub async fn config_set(
    session: tauri::State<'_, Option<GuiSession>>,
    key: String,
    value: serde_json::Value,
) -> Result<(), String> {
    let sess = session.inner().as_ref().ok_or_else(|| "当前模式不是单容器模式".to_string())?;

    let set_req = CfgSet { key, value };
    send_json_request(sess, "config.set".to_string(),
        serde_json::to_value(set_req).map_err(|e| e.to_string())?)
        .await.map_err(|e| e.to_string())?;

    Ok(())
}
