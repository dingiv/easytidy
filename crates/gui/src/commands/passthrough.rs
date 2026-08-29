//! Passthrough 命令：宿主 .desktop 导出/撤销/auto-start/自定义应用。

use tracing::{info, warn};

use easytidy_protocol::ops::{
    CfgGet, CfgGetResp, CfgSet, PassthroughList, PassthroughListResp, PassthroughSet,
    PtConfiguredApp,
};

use crate::commands::apps::AppInfoFrontend;
use crate::commands::fs::fetch_container_file;
use crate::commands::socket::send_json_request;
use crate::state::GuiSession;

// ============================================================================
// 容器内 passthrough 配置（每容器应用列表 + auto-start）读写 —— 经 server
// `passthrough.list` / `passthrough.set`（配置在容器内，容器自包含；server 启动
// 自读拉起 auto-start 应用）。宿主侧只留收藏(pinned)与导出元数据。
// ============================================================================

/// 读容器内配置的应用列表
async fn read_container_apps(sess: &GuiSession) -> Result<Vec<PtConfiguredApp>, String> {
    let resp = send_json_request(
        sess,
        "passthrough.list".to_string(),
        serde_json::to_value(PassthroughList).map_err(|e| e.to_string())?,
    )
    .await
    .map_err(|e| e.to_string())?;
    if let Some(err) = resp.err {
        return Err(format!("{} {}", err.code, err.message));
    }
    let list: PassthroughListResp =
        serde_json::from_value(resp.payload).map_err(|e| format!("解析 passthrough.list 失败：{e}"))?;
    Ok(list.apps)
}

/// 写容器内配置的应用列表（整份覆盖）
async fn save_container_apps(sess: &GuiSession, apps: Vec<PtConfiguredApp>) -> Result<(), String> {
    let resp = send_json_request(
        sess,
        "passthrough.set".to_string(),
        serde_json::to_value(PassthroughSet { apps }).map_err(|e| e.to_string())?,
    )
    .await
    .map_err(|e| e.to_string())?;
    if let Some(err) = resp.err {
        return Err(format!("{} {}", err.code, err.message));
    }
    Ok(())
}

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

/// 宿主 CLI 绝对路径（passthrough .desktop 与 GUI 入口 .desktop 的
/// Exec/TryExec 用——所有桌面快捷方式统一指向 CLI 垫片 `easytidy open`）。
///
/// 探测顺序：① GUI 同目录的 easytidy（开发布局 target/debug 共存）；
/// ② 安装目录 ~/.local/share/easytidy/bin/easytidy（部署布局）；
/// ③ PATH 中的 easytidy。宿主 PATH 未必有 easytidy（实测未安装），
/// 必须给出绝对路径，否则桌面入口无法启动。
pub(crate) fn cli_path() -> String {
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

/// 获取 passthrough 状态（已导出 .desktop 全文 + 配置的应用条目 + 收藏）。
///
/// 返回（前端契约）：
/// ```json
/// { "exported": [{"desktop_file","content"}],
///   "configured_apps": [{"id","name","cmd","desktop_file"?,"auto_start","icon"?}],
///   "pinned": [{"id","name","cmd","icon"?}] }
/// ```
/// `configured_apps` 是**容器内** passthrough 配置中有状态条目（auto_start=true 或
/// custom 应用）；扫描应用若未配置则不在此（前端按 id 匹配查 auto-start）。
#[tauri::command]
pub async fn passthrough_state(
    session: tauri::State<'_, Option<GuiSession>>,
) -> Result<serde_json::Value, String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    let container = &sess.container_name;

    let exported =
        easytidy_core::desktop::list_passthrough_detailed(container).map_err(|e| e.to_string())?;
    let exported_json: Vec<_> = exported
        .into_iter()
        .map(|e| serde_json::json!({ "desktop_file": e.desktop_file, "content": e.content }))
        .collect();

    // 容器内配置（经 server 读）
    let apps = read_container_apps(sess).await?;
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

    // 收藏（pin 到工具栏，宿主侧）：顺序 = pin 顺序
    let config_file = passthrough_config_file()?;
    let pinned = config_file.pinned(container).map_err(|e| e.to_string())?;
    let pinned_json: Vec<_> = pinned
        .into_iter()
        .map(|a| {
            serde_json::json!({
                "id": a.id,
                "name": a.name,
                "cmd": a.cmd,
                "icon": a.icon,
            })
        })
        .collect();

    // 容器自启动模式（systemd user unit 为准："off"|"silent"|"gui"）
    let boot_mode = easytidy_core::systemd::boot_mode(container).unwrap_or_else(|e| {
        warn!("查询自启动模式失败：{e}");
        "off".to_string()
    });

    Ok(serde_json::json!({
        "exported": exported_json,
        "configured_apps": apps_json,
        "pinned": pinned_json,
        "boot_mode": boot_mode,
    }))
}

// ============================================================================
// 容器自启动命令（systemd user unit：登录时触发）
// ============================================================================

/// 设置容器自启动模式。
///
/// - `"off"`：关闭（卸载 unit + disable）
/// - `"silent"`：静默（登录后仅后台启动容器）
/// - `"gui"`：非静默（登录后启动容器 + 拉起 Worker GUI 窗口）
///
/// 对应 systemd user unit `easytidy-<name>.service`（WantedBy=default.target）；
/// ExecStart = `<cli> boot --container <name> [--gui]`，cli 取 passthrough
/// 探测的绝对路径（systemd 用户单元 PATH 受限）。
#[tauri::command]
pub async fn passthrough_set_boot_mode(
    session: tauri::State<'_, Option<GuiSession>>,
    mode: String,
) -> Result<(), String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    let container = sess.container_name.clone();
    let cli = cli_path();

    // systemctl 调用（阻塞）放 spawn_blocking，避免卡 async runtime
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        match mode.as_str() {
            "off" => {
                // disable 后删除文件再 reload（顺序相反会残留失效单元）
                easytidy_core::systemd::disable_boot_unit(&container).map_err(|e| e.to_string())?;
                easytidy_core::systemd::remove_boot_unit(&container).map_err(|e| e.to_string())?;
                easytidy_core::systemd::daemon_reload().map_err(|e| e.to_string())?;
            }
            "silent" | "gui" => {
                let gui = mode == "gui";
                easytidy_core::systemd::install_boot_unit(&container, &cli, gui)
                    .map_err(|e| e.to_string())?;
                easytidy_core::systemd::daemon_reload().map_err(|e| e.to_string())?;
                easytidy_core::systemd::enable_boot_unit(&container).map_err(|e| e.to_string())?;
            }
            other => return Err(format!("未知自启动模式：{other}")),
        }
        info!("容器自启动模式已设置：{container} → {mode}（cli={cli}）");
        Ok(())
    })
    .await
    .map_err(|e| format!("自启动设置任务失败：{e}"))?
}

// ============================================================================
// 收藏（pin 到工具栏）命令
// ============================================================================

/// 收藏/取消收藏应用（pin 到 GUI 工具栏）。
///
/// pinned=true → upsert 到收藏列表（cmd/icon 一并保存，拉起与显示用最新值）；
/// pinned=false → 移除。icon_path：扫描应用 = 容器内图标路径（工具栏经
/// server 拉取显示），自定义应用 = 宿主 ~/.easytidy/icons 路径。
#[tauri::command]
pub async fn passthrough_set_pinned(
    session: tauri::State<'_, Option<GuiSession>>,
    id: String,
    name: String,
    cmd: String,
    icon_path: Option<String>,
    pinned: bool,
) -> Result<(), String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    let container = &sess.container_name;
    let config_file = passthrough_config_file()?;

    if pinned {
        let app = easytidy_core::passthrough::PassthroughApp {
            id: id.clone(),
            name,
            cmd: clean_exec(&cmd),
            desktop_file: (!id.starts_with("custom:")).then(|| id.clone()),
            auto_start: false,
            icon: icon_path,
        };
        config_file
            .pin_app(container, app)
            .map_err(|e| e.to_string())?;
    } else {
        config_file
            .unpin_app(container, &id)
            .map_err(|e| e.to_string())?;
    }
    info!("passthrough 收藏已更新：{container} {id} pinned={pinned}");
    Ok(())
}

/// 拉起收藏的应用（工具栏点击；经 server apps.launch，server 保活）。
/// 返回进程 pid。
#[tauri::command]
pub async fn passthrough_launch(
    session: tauri::State<'_, Option<GuiSession>>,
    id: String,
) -> Result<u32, String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    let container = &sess.container_name;

    let config_file = passthrough_config_file()?;
    let app = config_file
        .pinned(container)
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|a| a.id == id)
        .ok_or_else(|| format!("收藏的应用不存在：{id}"))?;

    let results = easytidy_core::passthrough::launch_apps(container, std::slice::from_ref(&app))
        .await
        .map_err(|e| e.to_string())?;
    match results.first() {
        Some(r) => match r.pid {
            Some(pid) => {
                info!("收藏应用已拉起：{id} (pid={pid})");
                // 拉起后检测即时退出：命令不存在（127）等情况下 spawn 成功但
                // 马上退出，「启动成功」是误导——轮询 server apps.ps，退出码
                // 非 0 时返回错误 + 捕获的 stdio（server 已捕获 stdout/stderr）
                detect_early_exit(container, pid).await?;
                Ok(pid)
            }
            None => Err(r.error.clone().unwrap_or_else(|| "拉起失败".to_string())),
        },
        None => Err("server 无响应".to_string()),
    }
}

/// 拉起后短暂轮询 server 进程表，检测命令是否立即退出（非 0 码）。
///
/// 命令不存在（如 google-chrome-stable 未安装）时 spawn 成功但 `su -c` 立即
/// 127 退出——若只返回 pid，前端显示「启动成功」而用户看不到窗口。此处轮询
/// `apps.ps`（server 已记录退出码），非 0 即拉 `apps.logs` 的 stdio 一并返回，
/// 让「启动成功」变成诚实的「启动失败：命令不存在」。
///
/// **best-effort**：检测本身不能阻塞拉起——`apps.ps` 不可用（server 二进制
/// 过旧未含该 op / 容器 server 未就绪）时跳过检测，视为启动成功，绝不让
/// 探测失败反过来破坏一次本来成功的拉起。
async fn detect_early_exit(container: &str, pid: u32) -> Result<(), String> {
    // 给命令失败留出时间（su -c 找不到命令 → 立即 127）
    tokio::time::sleep(std::time::Duration::from_millis(700)).await;
    for _ in 0..3 {
        // server 过旧（无 apps.ps）/未就绪时无法核实 → 优雅降级，不阻断拉起
        let procs = match easytidy_core::passthrough::list_managed_processes(container).await {
            Ok(p) => p,
            Err(e) => {
                warn!("apps.ps 查询失败（server 可能过旧/未就绪），跳过即时退出检测：{e}");
                return Ok(());
            }
        };
        match procs.iter().find(|p| p.pid == pid) {
            // 仍在运行或正常退出（0）→ 视为启动成功
            Some(p) if p.status == "running" => return Ok(()),
            Some(p) if p.status == "exited" && p.exit_code == Some(0) => return Ok(()),
            Some(p) => {
                // 退出码非 0 → 拉捕获的 stdio 展示原因
                let logs = easytidy_core::passthrough::fetch_process_logs(container, pid)
                    .await
                    .unwrap_or_default();
                let code = p.exit_code.unwrap_or(-1);
                let detail = logs.trim();
                return Err(if detail.is_empty() {
                    format!("应用启动后立即退出（退出码 {code}）")
                } else {
                    format!("应用启动后立即退出（退出码 {code}）：{detail}")
                });
            }
            // 进程已从注册表消失（退出后待 prune）——等一轮再查
            None => tokio::time::sleep(std::time::Duration::from_millis(300)).await,
        }
    }
    // 三轮未发现明确失败，视为启动成功（避免误报长启动应用）
    Ok(())
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

/// 设置应用 auto-start（写入**容器内**配置；server 启动自读拉起）
#[tauri::command]
pub async fn passthrough_set_auto_start(
    session: tauri::State<'_, Option<GuiSession>>,
    id: String,
    name: String,
    cmd: String,
    enabled: bool,
) -> Result<(), String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    let container = &sess.container_name;

    let mut apps = read_container_apps(sess).await?;
    // 保留已存应用的图标（upsert 会整体覆盖，不能丢）
    let existing_icon = apps
        .iter()
        .find(|a| a.id == id)
        .and_then(|a| a.icon.clone());
    let app = PtConfiguredApp {
        id: id.clone(),
        name,
        cmd: clean_exec(&cmd),
        desktop_file: (!id.starts_with("custom:")).then(|| id.clone()),
        auto_start: false,
        icon: existing_icon,
    };
    if enabled {
        let mut app = app;
        app.auto_start = true;
        if let Some(existing) = apps.iter_mut().find(|a| a.id == id) {
            *existing = app;
        } else {
            apps.push(app);
        }
    } else if let Some(existing) = apps.iter_mut().find(|a| a.id == id) {
        if id.starts_with("custom:") {
            existing.auto_start = false; // custom 保留（是资产，仅关 auto-start）
        } else {
            apps.retain(|a| a.id != id); // 扫描应用 absence = false
        }
    }
    save_container_apps(sess, apps).await?;
    info!("passthrough auto-start 已设置（容器内）：{container} enabled={enabled}");
    Ok(())
}

/// 添加自定义应用（固定目录扫描之外，如 `google-chrome-stable --disable-dev-shm-usage`），
/// 写入**容器内**配置。返回 AppInfoFrontend（desktop_file=`custom:<name>`），前端直接进现有导出流。
#[tauri::command]
pub async fn passthrough_add_custom(
    session: tauri::State<'_, Option<GuiSession>>,
    name: String,
    cmd: String,
) -> Result<AppInfoFrontend, String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    let container = &sess.container_name;

    if name.trim().is_empty() || cmd.trim().is_empty() {
        return Err("自定义应用名称与命令不能为空".to_string());
    }
    let id = format!("custom:{name}");
    let mut apps = read_container_apps(sess).await?;
    if apps.iter().any(|a| a.id == id) {
        return Err(format!("自定义应用 {name} 已存在"));
    }
    let app = PtConfiguredApp {
        id: id.clone(),
        name: name.clone(),
        cmd: cmd.trim().to_string(),
        desktop_file: None,
        auto_start: false,
        icon: None,
    };
    apps.push(app);
    save_container_apps(sess, apps).await?;
    info!("自定义应用已添加（容器内）：{container} {name}");
    Ok(AppInfoFrontend {
        name,
        icon_path: None,
        exec: cmd,
        comment: None,
        desktop_file: id,
        categories: None,
        startup_notify: false,
        startup_wm_class: None,
    })
}

/// 移除应用（容器内配置条目 + 清理可能存在的导出，防孤儿）
#[tauri::command]
pub async fn passthrough_remove_app(
    session: tauri::State<'_, Option<GuiSession>>,
    id: String,
) -> Result<(), String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    let container = &sess.container_name;

    let mut apps = read_container_apps(sess).await?;
    apps.retain(|a| a.id != id);
    save_container_apps(sess, apps).await?;

    // 非 custom 且已导出 → 清理导出（防孤儿）
    if !id.starts_with("custom:") {
        let _ = easytidy_core::desktop::remove_passthrough(container, &id);
    }
    info!("passthrough 应用已移除（容器内）：{container} {id}");
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
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    let bytes = fetch_container_file(sess, &container_path).await?;
    save_icon_to_appdata(&bytes, &container_path)
}

/// 设置自定义应用图标（icon = 宿主 ~/.easytidy/icons 路径；None = 清除）。
/// 写入**容器内**配置；导出时 Icon= 直接用该路径。
#[tauri::command]
pub async fn passthrough_set_custom_icon(
    session: tauri::State<'_, Option<GuiSession>>,
    id: String,
    icon: Option<String>,
) -> Result<(), String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    let container = &sess.container_name;

    let mut apps = read_container_apps(sess).await?;
    let Some(app) = apps.iter_mut().find(|a| a.id == id) else {
        return Err(format!("应用不存在：{id}"));
    };
    app.icon = icon.clone();
    save_container_apps(sess, apps).await?;
    info!("自定义应用图标已设置（容器内）：{container} {id} → {icon:?}");
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
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;
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
                        .map(|c| {
                            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                                c
                            } else {
                                '-'
                            }
                        })
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
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    let removed = easytidy_core::desktop::remove_passthrough(&sess.container_name, &desktop_file)
        .map_err(|e| e.to_string())?;
    Ok(removed.to_string_lossy().into_owned())
}

/// 导出本容器的 GUI 管理界面桌面快捷方式（菜单 + 桌面图标）。
///
/// Exec = 宿主 CLI 垫片 `easytidy open --container <name>`（点击时由 CLI
/// 决定保活/弹 GUI/友好报错；不再冷启动 380MB 的 GUI 二进制）。
/// TryExec 同；内置品牌 SVG 图标；桌面副本 chmod +x + gio trusted。
/// 返回应用菜单路径。
#[tauri::command]
pub async fn export_gui_shortcut(
    session: tauri::State<'_, Option<GuiSession>>,
    desktop_icon: Option<bool>,
) -> Result<String, String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;

    let menu_path = easytidy_core::desktop::write_gui_entry(
        &sess.container_name,
        &cli_path(),
        desktop_icon.unwrap_or(true),
    )
    .map_err(|e| e.to_string())?;
    info!("GUI 入口导出：{} → {:?}", sess.container_name, menu_path);
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
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;

    let resp = send_json_request(
        sess,
        "config.get".to_string(),
        serde_json::to_value(CfgGet).map_err(|e| e.to_string())?,
    )
    .await
    .map_err(|e| e.to_string())?;

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
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;

    let set_req = CfgSet { key, value };
    send_json_request(
        sess,
        "config.set".to_string(),
        serde_json::to_value(set_req).map_err(|e| e.to_string())?,
    )
    .await
    .map_err(|e| e.to_string())?;

    Ok(())
}
