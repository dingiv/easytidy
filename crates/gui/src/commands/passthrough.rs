//! Passthrough 命令：宿主 .desktop 导出/撤销/auto-start/自定义应用。

use tracing::{info, warn};

use base64::Engine as _;

use easytidy_protocol::ops::{
    AppKill, AppLogs, AppLogsResp, AppsPs, AppsPsResp, CfgGet, CfgGetResp, CfgSet, FsMkdir, FsWrite,
    ManagedProcess, PassthroughList, PassthroughListResp, PassthroughSet, PtConfiguredApp,
};

use crate::commands::apps::AppInfoFrontend;
use crate::commands::fs::fetch_container_file;
use crate::commands::socket::send_json_request;
use crate::state::GuiSession;

// ============================================================================
// 容器内 passthrough 配置（应用列表 + 收藏）读写 —— 经 server
// `passthrough.list` / `passthrough.set`（配置在容器内，容器自包含；server 启动
// 自读拉起 auto-start 应用）。宿主侧不再持有 per-container 配置。
// ============================================================================

/// 容器内 passthrough 配置（应用列表 + 收藏），经 server 读写。
/// 两类条目同存一文件（容器自包含），改任一项须整份读-改-写（pinned 不可被
/// apps 的整份覆盖冲掉）。
#[derive(Default, Clone)]
struct ContainerPtConfig {
    /// 有状态条目（auto-start 应用 + 自定义应用）
    apps: Vec<PtConfiguredApp>,
    /// 收藏（pin 到 GUI 工具栏），顺序 = 显示顺序
    pinned: Vec<PtConfiguredApp>,
}

/// 读容器内配置（apps + pinned）
async fn read_container_config(sess: &GuiSession) -> Result<ContainerPtConfig, String> {
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
    Ok(ContainerPtConfig {
        apps: list.apps,
        pinned: list.pinned,
    })
}

/// 写容器内配置（apps + pinned 整份覆盖）
async fn save_container_config(sess: &GuiSession, cfg: &ContainerPtConfig) -> Result<(), String> {
    let resp = send_json_request(
        sess,
        "passthrough.set".to_string(),
        serde_json::to_value(PassthroughSet {
            apps: cfg.apps.clone(),
            pinned: cfg.pinned.clone(),
        })
        .map_err(|e| e.to_string())?,
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

/// 验证 CLI 二进制支持 `open` 垫片子命令（`--help` 输出含 open）。
///
/// 历史呑痕：`~/.local/share/easytidy/bin/easytidy` 可能是旧版安装残留
/// （无 `open` 子命令）——曾导致发布版桌面图标全废（点击报 unrecognized
/// subcommand，且用户看不到）。导出前校验，陈旧二进制跳过。
fn cli_supports_open(path: &str) -> bool {
    std::process::Command::new(path)
        .arg("--help")
        .output()
        .map(|out| {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            // 子命令列表里含独立的 open 词（避免误匹配路径/参数里的 open 字样）
            text.split_whitespace().any(|w| w == "open")
        })
        .unwrap_or(false)
}

/// 宿主 CLI 绝对路径（passthrough .desktop 与 GUI 入口 .desktop 的
/// Exec/TryExec 用——所有桌面快捷方式统一指向 CLI 垫片 `easytidy open`）。
///
/// 探测顺序：① GUI 同目录的 easytidy（开发布局 target/debug 共存）；
/// ② 安装目录 ~/.local/share/easytidy/bin/easytidy（部署布局）；
/// ③ PATH 中的 easytidy。宿主 PATH 未必有 easytidy（实测未安装），
/// 必须给出绝对路径，否则桌面入口无法启动。
/// 候选需通过 [`cli_supports_open`] 能力校验（旧版无 `open` 子命令的
/// 二进制直接跳过，不再导出指向它的死图标）。
pub(crate) fn cli_path() -> String {
    let mut candidates: Vec<String> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            candidates.push(parent.join("easytidy").to_string_lossy().into_owned());
        }
    }
    if let Some(data) = dirs::data_local_dir() {
        candidates.push(data.join("easytidy/bin/easytidy").to_string_lossy().into_owned());
    }
    candidates.push("easytidy".to_string());

    // 逐个校验支持 open；全不合格则回退第一个存在/PATH 兜底（维持旧行为，
    // 让错误在点击时暴露而不是导出时静默指向空）
    let mut first_existing: Option<&String> = None;
    for c in &candidates {
        if c == "easytidy" || std::path::Path::new(c).exists() {
            if first_existing.is_none() {
                first_existing = Some(c);
            }
            if c == "easytidy" || cli_supports_open(c) {
                return c.clone();
            }
            tracing::warn!("CLI 候选不支持 open 子命令（旧版安装残留？），跳过：{c}");
        }
    }
    first_existing.cloned().unwrap_or_else(|| "easytidy".to_string())
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

    // 容器内配置（经 server 读；apps + pinned 同存容器，容器自包含）
    let cfg = read_container_config(sess).await?;
    let apps_json: Vec<_> = cfg
        .apps
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

    // 收藏（pin 到工具栏）：顺序 = pin 顺序
    let pinned_json: Vec<_> = cfg
        .pinned
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
/// pinned=false → 移除。icon_path：扫描应用与自定义应用均为**容器内**图标
/// 路径（工具栏经 server 拉取显示）。
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

    // 收藏存**容器内**配置（容器自包含）：读-改-写整份（apps 不可被冲掉）
    let mut cfg = read_container_config(sess).await?;
    if pinned {
        let app = PtConfiguredApp {
            id: id.clone(),
            name,
            cmd: clean_exec(&cmd),
            desktop_file: (!id.starts_with("custom:")).then(|| id.clone()),
            auto_start: false,
            icon: icon_path,
        };
        // 按 id upsert（保持顺序）
        if let Some(existing) = cfg.pinned.iter_mut().find(|a| a.id == id) {
            *existing = app;
        } else {
            cfg.pinned.push(app);
        }
    } else {
        cfg.pinned.retain(|a| a.id != id);
    }
    save_container_config(sess, &cfg).await?;
    info!("passthrough 收藏已更新（容器内）：{container} {id} pinned={pinned}");
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

    // 收藏存容器内配置（容器自包含）
    let cfg = read_container_config(sess).await?;
    let app = cfg
        .pinned
        .into_iter()
        .find(|a| a.id == id)
        .ok_or_else(|| format!("收藏的应用不存在：{id}"))?;
    let launch_app = easytidy_core::passthrough::PassthroughApp {
        id: app.id.clone(),
        name: app.name.clone(),
        cmd: app.cmd.clone(),
        desktop_file: app.desktop_file.clone(),
        auto_start: false,
        icon: app.icon.clone(),
    };

    let results = easytidy_core::passthrough::launch_apps(container, std::slice::from_ref(&launch_app))
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

/// 立即拉起容器内应用（Passthrough 管理器列表里任意应用，无需先收藏）。
///
/// 与 [`passthrough_launch`]（拉起**收藏**应用）的区别：这里按调用方传入的
/// name/cmd 即时构造应用拉起，用于「列表里点一下就启动某个被扫描到的应用」。
/// 经 server apps.launch 拉起（server 保活、子进程独立于连接存活）；返回进程 pid。
#[tauri::command]
pub async fn passthrough_launch_app(
    session: tauri::State<'_, Option<GuiSession>>,
    id_or_name: String,
) -> Result<u32, String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    let container = &sess.container_name;

    if id_or_name.trim().is_empty() {
        return Err("应用 id 为空，无法启动".to_string());
    }

    // 按引用拉起：server 查登记表/自定义应用解析 exec 并自行 spawn
    // （调用方不传命令串）
    let resp = easytidy_core::passthrough::launch_app_by_ref(container, &id_or_name)
        .await
        .map_err(|e| e.to_string())?;
    match resp.pid {
        Some(pid) => {
            info!("应用已拉起：{} (id={}, pid={pid})", id_or_name, resp.id.unwrap_or_default());
            // 拉起后检测即时退出（命令不存在 → 127 等，避免误报「启动成功」）
            detect_early_exit(container, pid).await?;
            Ok(pid)
        }
        None => Err(match resp.available {
            Some(ref available) if !available.is_empty() => format!(
                "{}（可用：{}）",
                resp.error.unwrap_or_else(|| "启动失败".to_string()),
                available.join(", ")
            ),
            _ => resp.error.unwrap_or_else(|| "启动失败".to_string()),
        }),
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

    let mut cfg = read_container_config(sess).await?;
    // 保留已存应用的图标（upsert 会整体覆盖，不能丢）
    let existing_icon = cfg
        .apps
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
        if let Some(existing) = cfg.apps.iter_mut().find(|a| a.id == id) {
            *existing = app;
        } else {
            cfg.apps.push(app);
        }
    } else if let Some(existing) = cfg.apps.iter_mut().find(|a| a.id == id) {
        if id.starts_with("custom:") {
            existing.auto_start = false; // custom 保留（是资产，仅关 auto-start）
        } else {
            cfg.apps.retain(|a| a.id != id); // 扫描应用 absence = false
        }
    }
    save_container_config(sess, &cfg).await?;
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
    let mut cfg = read_container_config(sess).await?;
    if cfg.apps.iter().any(|a| a.id == id) {
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
    cfg.apps.push(app);
    save_container_config(sess, &cfg).await?;
    info!("自定义应用已添加（容器内）：{container} {name}");
    Ok(AppInfoFrontend {
        id: id.clone(),
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

/// 更新自定义应用（改名称/命令）。名称变化时同步 id（id = `custom:<name>`）
/// 与收藏引用（防孤儿）；图标字段随应用条目保留。
#[tauri::command]
pub async fn passthrough_update_custom(
    session: tauri::State<'_, Option<GuiSession>>,
    id: String,
    name: String,
    cmd: String,
) -> Result<(), String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    let container = &sess.container_name;

    if !id.starts_with("custom:") {
        return Err("只能编辑自定义应用".to_string());
    }
    let new_name = name.trim().to_string();
    let new_cmd = cmd.trim().to_string();
    if new_name.is_empty() || new_cmd.is_empty() {
        return Err("自定义应用名称与命令不能为空".to_string());
    }
    let new_id = format!("custom:{new_name}");

    let mut cfg = read_container_config(sess).await?;
    // 先校验（不可变借用，先于可变借用）：目标存在 + 新 id 不冲突
    if !cfg.apps.iter().any(|a| a.id == id) {
        return Err(format!("自定义应用 {id} 不存在"));
    }
    if new_id != id && cfg.apps.iter().any(|a| a.id == new_id) {
        return Err(format!("自定义应用 {new_name} 已存在"));
    }

    // 改 app（可变借用 cfg.apps，块结束后不再用 app）
    {
        if let Some(app) = cfg.apps.iter_mut().find(|a| a.id == id) {
            if new_id != id {
                app.id = new_id.clone();
                app.name = new_name.clone();
            }
            app.cmd = new_cmd.clone();
        }
    }
    // 同步收藏引用（可变借用 cfg.pinned，独立于 cfg.apps）
    if new_id != id {
        for p in cfg.pinned.iter_mut() {
            if p.id == id {
                p.id = new_id.clone();
                p.name = new_name.clone();
                p.cmd = new_cmd.clone();
            }
        }
    }

    // 保存 + 日志（此时所有可变借用已结束）
    let final_id = if new_id != id { new_id.clone() } else { id.clone() };
    save_container_config(sess, &cfg).await?;
    info!("自定义应用已更新（容器内）：{container} {id} → {final_id}");
    Ok(())
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

    let mut cfg = read_container_config(sess).await?;
    cfg.apps.retain(|a| a.id != id);
    // 移除应用时一并取消收藏（防孤儿——收藏引用已不存在的应用）
    cfg.pinned.retain(|a| a.id != id);
    save_container_config(sess, &cfg).await?;

    // 非 custom 且已导出 → 清理导出（防孤儿）
    if !id.starts_with("custom:") {
        let _ = easytidy_core::desktop::remove_passthrough(container, &id);
    }
    info!("passthrough 应用已移除（容器内）：{container} {id}");
    Ok(())
}

// ============================================================================
// 应用控制台（app 日志/进程状态查询）——经共享连接轮询，供 AppConsolePane 使用
// ============================================================================

/// 获取某受管子进程的已捕获 stdio（server `apps.logs`：stdout+stderr 合并的
/// 有界环形缓冲，超出上限丢最旧）。
///
/// 走 GuiSession 共享连接（不新建连接）——前端轮询用，每次调用开销小。
/// 进程不存在（已退出被 prune / 未知 pid）时 server 返回空串（非错误）。
#[tauri::command]
pub async fn app_logs(
    session: tauri::State<'_, Option<GuiSession>>,
    pid: u32,
) -> Result<String, String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    let resp = send_json_request(
        sess,
        "apps.logs".to_string(),
        serde_json::to_value(AppLogs { pid }).map_err(|e| e.to_string())?,
    )
    .await
    .map_err(|e| e.to_string())?;
    if let Some(err) = resp.err {
        return Err(format!("{} {}", err.code, err.message));
    }
    let logs: AppLogsResp =
        serde_json::from_value(resp.payload).map_err(|e| format!("解析 apps.logs 失败：{e}"))?;
    Ok(logs.stdio)
}

/// 终止托管进程（server `apps.kill`：SIGTERM）。进程不存在/已退出 → 报错。
#[tauri::command]
pub async fn app_kill(
    session: tauri::State<'_, Option<GuiSession>>,
    pid: u32,
) -> Result<(), String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    let resp = send_json_request(
        sess,
        "apps.kill".to_string(),
        serde_json::to_value(AppKill { pid }).map_err(|e| e.to_string())?,
    )
    .await
    .map_err(|e| e.to_string())?;
    if let Some(err) = resp.err {
        return Err(format!("{} {}", err.code, err.message));
    }
    Ok(())
}

/// 列出 server 托管的进程（`apps.ps`：运行中 + 最近退出，带退出码与 stdio 长度）。
///
/// 供前端判断某 pid 是否仍在运行/已退出/退出码——应用控制台据此显示状态
/// 并决定是否继续轮询。走共享连接（轮询用）。
#[tauri::command]
pub async fn app_ps(
    session: tauri::State<'_, Option<GuiSession>>,
) -> Result<Vec<ManagedProcess>, String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    let resp = send_json_request(
        sess,
        "apps.ps".to_string(),
        serde_json::to_value(AppsPs).map_err(|e| e.to_string())?,
    )
    .await
    .map_err(|e| e.to_string())?;
    if let Some(err) = resp.err {
        return Err(format!("{} {}", err.code, err.message));
    }
    let ps: AppsPsResp =
        serde_json::from_value(resp.payload).map_err(|e| format!("解析 apps.ps 失败：{e}"))?;
    Ok(ps.processes)
}

/// 容器 server 信息（容器默认用户 home）。
/// 宿主侧用 home_dir 定位容器内用户可写资源
/// （如自定义应用图标目录 ~/.easytidy/icons）。
#[tauri::command]
pub async fn server_info(
    session: tauri::State<'_, Option<GuiSession>>,
) -> Result<serde_json::Value, String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    server_info_inner(sess).await
}

/// server.info 请求（[`server_info`] tauri 命令与 [`container_icon_dir`] 共用）
async fn server_info_inner(sess: &GuiSession) -> Result<serde_json::Value, String> {
    let resp = send_json_request(
        sess,
        "server.info".to_string(),
        serde_json::to_value(serde_json::json!({})).map_err(|e| e.to_string())?,
    )
    .await
    .map_err(|e| e.to_string())?;
    if let Some(err) = resp.err {
        return Err(format!("{} {}", err.code, err.message));
    }
    Ok(resp.payload)
}

// ============================================================================
// 图标命令（自定义应用图标统一存**容器内** `{home}/.easytidy/icons/`，与宿主侧 `~/.easytidy` 约定一致；
// 容器自包含；宿主 .desktop 的 Icon= 导出时从容器拷出到宿主缓存）
// ============================================================================

/// 图标目录相对用户 home 的后缀（统一数据根 `.easytidy` 下的 icons 子目录）
const ICON_DIR_REL: &str = ".easytidy/icons";

/// 容器内自定义应用图标目录：`{home}/.easytidy/icons`（与 server `storage::icons_dir` 一致）。
///
/// server 以容器默认用户运行（非 root，/usr/local 不可写），故用该用户 home
/// 下的统一数据目录；home 经 `server.info` 的 home_dir 字段获取（server 即该用户，
/// 零探测）。旧 server 无 home_dir 字段 → 明确报错。
async fn container_icon_dir(sess: &GuiSession) -> Result<String, String> {
    let info = server_info_inner(sess).await?;
    let home = info
        .get("home_dir")
        .and_then(|v| v.as_str())
        .filter(|h| !h.is_empty())
        .ok_or_else(|| "无法获取容器 home 目录（容器内 server 版本过旧，无 home_dir 字段）".to_string())?;
    Ok(format!("{home}/{ICON_DIR_REL}"))
}

/// 从宿主机选择图标（rfd 原生文件对话框）→ 复制进容器
/// `{home}/.easytidy/icons/<原文件名>` → 返回**容器内路径**。
///
/// 自定义应用图标统一存容器内（容器自包含）：用户从宿主选一个图片文件，
/// 后端读取后经 server fs.mkdir + fs.write 写入容器，前端把容器内路径填回
/// 输入框。导出 .desktop 时再经 [`passthrough_export`] 从容器拷出。
#[tauri::command]
pub async fn passthrough_pick_host_icon(
    session: tauri::State<'_, Option<GuiSession>>,
) -> Result<String, String> {
    use std::io::Read;

    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;

    let icon_dir = container_icon_dir(sess).await?;

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

    let fname = std::path::Path::new(&picked)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("icon.png")
        .to_string();
    let container_path = format!("{icon_dir}/{fname}");

    // fs.write 不建父目录：先 mkdir -p 再全量写入
    let mkdir_req = FsMkdir { path: icon_dir.clone() };
    send_json_request(
        sess,
        "fs.mkdir".to_string(),
        serde_json::to_value(mkdir_req).map_err(|e| e.to_string())?,
    )
    .await
    .map_err(|e| e.to_string())?;

    let write_req = FsWrite {
        path: container_path.clone(),
        data_b64: base64::engine::general_purpose::STANDARD.encode(&bytes),
        offset: None,
    };
    send_json_request(
        sess,
        "fs.write".to_string(),
        serde_json::to_value(write_req).map_err(|e| e.to_string())?,
    )
    .await
    .map_err(|e| e.to_string())?;

    info!("图标已从宿主复制进容器：{picked} → {container_path}");
    Ok(container_path)
}

/// 选择宿主图片作为容器入口图标源（容器管理「容器」页用，无 session 依赖）：
/// 原生文件选择 → 拷入宿主 icons 目录（`easytidy-container-<name>.<ext>`，
/// 保留原图源图——导出时经 [`easytidy_core::desktop::process_container_icon`]
/// 品牌加工）→ 返回宿主路径（前端写入配置 icon 字段）。
#[tauri::command]
pub async fn container_pick_icon(name: String) -> Result<String, String> {
    use std::io::Read;

    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("容器名为空（请先填写名称）".to_string());
    }

    // 原生对话框阻塞主线程：spawn_blocking 避免卡 async runtime
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
        .map_err(|e| format!("读取图片失败：{e}"))?;

    let icons_dir = easytidy_core::appdata::icons_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&icons_dir).map_err(|e| e.to_string())?;
    let ext = std::path::Path::new(&picked)
        .extension()
        .and_then(|e| e.to_str())
        .filter(|e| !e.is_empty())
        .unwrap_or("png");
    let dest = icons_dir.join(format!("easytidy-container-{name}.{ext}"));
    std::fs::write(&dest, &bytes).map_err(|e| format!("写入图标失败：{e}"))?;
    info!("容器入口图标已选择：{name} ← {picked} → {}", dest.display());
    Ok(dest.to_string_lossy().into_owned())
}

/// 设置自定义应用图标（icon = **容器内**图片路径；None = 清除）。
/// 写入**容器内**配置；导出时 Icon= 用该容器内路径拷出的宿主缓存。
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

    let mut cfg = read_container_config(sess).await?;
    let Some(app) = cfg.apps.iter_mut().find(|a| a.id == id) else {
        return Err(format!("应用不存在：{id}"));
    };
    app.icon = icon.clone();
    save_container_config(sess, &cfg).await?;
    info!("自定义应用图标已设置（容器内）：{container} {id} → {icon:?}");
    Ok(())
}

/// 导出 passthrough 应用（宿主生成**薄指针** .desktop：应用菜单 + 桌面图标。
/// Exec 只引用应用 id（`easytidy launch <id> --container <n>`），启动决策
/// 归容器内 server 登记表；宿主仅负责图标搬运+存在性验证。桌面图标
/// 经 chmod +x + `gio metadata::trusted` 信任标记（GNOME 双击必需）。
/// 生成逻辑在 core::desktop::write_passthrough，CLI unexport 与其共用）
#[tauri::command]
pub async fn passthrough_export(
    session: tauri::State<'_, Option<GuiSession>>,
    app: AppInfoFrontend,
    // 同时创建桌面图标（GNOME 桌面默认不显示应用菜单，入口在桌面路径）
    desktop_icon: Option<bool>,
    // 桌面显示名（.desktop Name=）；空/None = 应用名
    display_name: Option<String>,
) -> Result<String, String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    let container = sess.container_name.clone();

    // 图标：两类应用均为**容器内**路径 → 导出时经 server 拷出到宿主缓存 →
    // Icon= 用宿主路径。custom 保留原始图标；扫描应用合成品牌化（边框+水印）；
    // 拷出失败（容器未运行/文件缺失）时 custom 回退按 app id 生成的 identicon
    let mut icon_attr = None;
    if app.desktop_file.starts_with("custom:") {
        if let Some(icon_path) = app.icon_path.as_ref() {
            if let Ok(icon_data) = fetch_container_file(sess, icon_path).await {
                if let Ok(icons_dir) = easytidy_core::desktop::passthrough_icon_dir() {
                    if std::fs::create_dir_all(&icons_dir).is_ok() {
                        // 与扫描应用统一：经内置图标加工工具处理（品牌渐变边框 +
                        // 圆角内容 + 水印，输出 256×256 PNG）；加工失败（如 SVG
                        // 等 image 不支持的格式）保留原图（原扩展名）
                        let (bytes, file_name) =
                            match easytidy_core::icon::compose_app_icon(
                                &icon_data,
                                EASYTIDY_BRAND_ICON,
                            ) {
                                Ok(composed) => (
                                    composed,
                                    format!("easytidy-custom-{container}-icon.png"),
                                ),
                                Err(e) => {
                                    tracing::warn!("自定义应用图标加工失败，保留原图：{e}");
                                    let ext = std::path::Path::new(icon_path)
                                        .extension()
                                        .and_then(|e| e.to_str())
                                        .filter(|e| !e.is_empty())
                                        .unwrap_or("png");
                                    (
                                        icon_data,
                                        format!("easytidy-custom-{container}-icon.{ext}"),
                                    )
                                }
                            };
                        let icon_file = icons_dir.join(&file_name);
                        if std::fs::write(&icon_file, &bytes).is_ok() {
                            icon_attr = Some(icon_file.to_string_lossy().into_owned());
                        }
                    }
                }
            }
        }
        if icon_attr.is_none() {
            icon_attr = easytidy_core::desktop::ensure_app_icon(&app.id);
        }
    } else if let Some(icon_path) = app.icon_path.as_ref() {
        if icon_path.starts_with('/') {
            if let Ok(icon_data) = fetch_container_file(sess, icon_path).await {
                if let Ok(icons_dir) = easytidy_core::desktop::passthrough_icon_dir() {
                    if std::fs::create_dir_all(&icons_dir).is_ok() {
                        // 图标缓存按应用 id 命名（与 .desktop 文件命名一致；id 稳定）
                        let safe: String = app
                            .id
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
        } else {
            // 主题图标名（org.gnome.Screenshot / printer / …）：无文件可拉。
            // 宿主与容器共用同一图标主题（容器挂载宿主 icons），直接写名字，
            // 桌面环境按主题解析（避免导出后图标缺失）。
            icon_attr = Some(icon_path.clone());
        }
    }

    let spec = easytidy_core::desktop::PassthroughSpec {
        container: container.clone(),
        app_name: app.name,
        display_name,
        comment: app.comment,
        categories: app.categories,
        app_id: app.id.clone(),
        icon: icon_attr,
        desktop_file: app.desktop_file,
        cli_path: cli_path(),
        startup_notify: app.startup_notify,
        startup_wm_class: app.startup_wm_class,
    };
    let menu_path = easytidy_core::desktop::write_passthrough(&spec, desktop_icon.unwrap_or(true))
        .map_err(|e| e.to_string())?;
    info!("passthrough 导出：{} → {:?}", spec.app_id, menu_path);

    Ok(menu_path.to_string_lossy().into_owned())
}

/// 撤销 passthrough 导出（按应用 id 删除宿主 .desktop）
#[tauri::command]
pub async fn passthrough_revoke(
    session: tauri::State<'_, Option<GuiSession>>,
    app_id: String,
) -> Result<String, String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    let removed = easytidy_core::desktop::remove_passthrough(&sess.container_name, &app_id)
        .map_err(|e| e.to_string())?;
    Ok(removed.to_string_lossy().into_owned())
}

/// 读取宿主 ConfigFile 的入口图标源（**主来源**，宿主或容器路径）。
fn host_entry_icon(container: &str) -> Option<String> {
    easytidy_core::configfile::ConfigFile::default_instance()
        .ok()
        .and_then(|cf| cf.get_container(container).ok())
        .flatten()
        .and_then(|c| c.icon)
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
}

/// 写入宿主 ConfigFile 的入口图标源（**主来源**；None = 清除）。
fn set_host_entry_icon(container: &str, path: Option<&str>) -> Result<(), String> {
    let config_file = easytidy_core::configfile::ConfigFile::default_instance()
        .map_err(|e| e.to_string())?;
    let mut cfg = config_file
        .get_container(container)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "容器配置不存在（请先创建容器）".to_string())?;
    cfg.icon = path
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| p.to_string());
    config_file.register_container(cfg).map_err(|e| e.to_string())?;
    Ok(())
}

/// 从容器（server）或宿主读取图标字节（容器优先，宿主回退）。
async fn fetch_or_host_read(sess: &GuiSession, path: &str) -> Result<Vec<u8>, String> {
    match fetch_container_file(sess, path).await {
        Ok(d) => Ok(d),
        Err(_) => std::fs::read(path)
            .map_err(|_| format!("图标文件不存在（容器内与宿主均未找到 {path}）")),
    }
}

/// 容器内 config.json 键写入（server 维护；None/空串 → 写 Null 清除）。
async fn container_config_set(
    sess: &GuiSession,
    key: &str,
    value: Option<&str>,
) -> Result<(), String> {
    let v = value
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|s| serde_json::Value::String(s.to_string()))
        .unwrap_or(serde_json::Value::Null);
    let set_req = CfgSet {
        key: key.to_string(),
        value: v,
    };
    let resp = send_json_request(
        sess,
        "config.set".to_string(),
        serde_json::to_value(set_req).map_err(|e| e.to_string())?,
    )
    .await
    .map_err(|e| e.to_string())?;
    if let Some(err) = resp.err {
        return Err(format!("{} {}", err.code, err.message));
    }
    Ok(())
}

/// 登记容器内入口图标路径到 `config.json` 的 `entry_icon`（None = 清除）。
async fn register_entry_icon_in_container(
    sess: &GuiSession,
    path: Option<&str>,
) -> Result<(), String> {
    container_config_set(sess, "entry_icon", path).await
}

/// 登记容器导出显示名到 `config.json` 的 `entry_name`（None = 清除）。
/// 与 entry_icon 同源：由 easytidy-server 维护，UI 进入时拉取预填。
async fn register_entry_name_in_container(
    sess: &GuiSession,
    name: Option<&str>,
) -> Result<(), String> {
    container_config_set(sess, "entry_name", name).await
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
    // 桌面显示名（.desktop Name=）；空/None = `easytidy <容器名>`
    display_name: Option<String>,
    // 导出时指定的图标源（宿主或容器路径，经品牌加工）；空/None = 用主来源（宿主 ConfigFile.icon）
    icon_source: Option<String>,
) -> Result<String, String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;

    // 容器入口图标（源 = 用户设置的图标路径，宿主或容器路径）：优先导出时指定的图标源，
    // 否则用主来源（宿主 ConfigFile.icon）。均经 server/宿主读取后品牌加工（未设置回退
    // 品牌图标），并把图标源写入主来源 + 镜像到容器 config.json（entry_icon，供 worker GUI）。
    // 每次导出都强制重写宿主图标 + .desktop（process_container_icon_bytes / write_gui_entry 均覆盖写）。
    let icon = match icon_source.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(src) if src.starts_with("data:") || src.starts_with("file://") => {
            return Err(format!(
                "无效的图标路径：{src}（请拖入预选图标或手动填写路径，不要拖拽图片文件本身）"
            ));
        }
        Some(src) => {
            let data = fetch_or_host_read(sess, src).await?;
            // 持久化：主来源（宿主 ConfigFile.icon）+ 镜像（容器 config.json entry_icon）
            set_host_entry_icon(&sess.container_name, Some(src)).map_err(|e| e.to_string())?;
            register_entry_icon_in_container(sess, Some(src)).await?;
            easytidy_core::desktop::process_container_icon_bytes(&sess.container_name, &data)
        }
        None => {
            // 无显式图标源：用主来源（宿主 ConfigFile.icon）；未设置 = 内置品牌图标
            match host_entry_icon(&sess.container_name) {
                Some(src) => {
                    let data = fetch_or_host_read(sess, &src).await?;
                    // 镜像到容器 config.json（entry_icon，供 worker GUI 经 server 查询）
                    register_entry_icon_in_container(sess, Some(&src)).await?;
                    easytidy_core::desktop::process_container_icon_bytes(&sess.container_name, &data)
                }
                None => {
                    // 兒底种子 = 导出显示名（与 .desktop Name= 同源）：改名重导出
                    // → 图标与桌面名字一起变；未填显示名用容器名
                    let seed = display_name
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .unwrap_or(&sess.container_name);
                    easytidy_core::desktop::ensure_container_entry_icon(&sess.container_name, seed)
                }
            }
        }
    };

    let menu_path = easytidy_core::desktop::write_gui_entry(
        &sess.container_name,
        &cli_path(),
        desktop_icon.unwrap_or(true),
        icon.as_deref(),
        display_name.as_deref(),
    )
    .map_err(|e| e.to_string())?;
    // 导出显示名持久化到容器 config.json（entry_name，server 维护；
    // 与 entry_icon 同源，UI 下次进入拉取预填）。种子与 .desktop Name= 同源。
    let effective_name = display_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("easytidy {}", sess.container_name));
    register_entry_name_in_container(sess, Some(&effective_name))
        .await
        .map_err(|e| e.to_string())?;
    info!("GUI 入口导出：{} → {:?}", sess.container_name, menu_path);
    Ok(menu_path.to_string_lossy().into_owned())
}

/// 读取容器入口导出配置（显示名 + 图标源，均存容器内 config.json、由
/// server 维护）：UI 进入时拉取预填。server 不可达（容器未运行）→
/// name = None、icon 回退宿主 ConfigFile.icon。
#[tauri::command]
pub async fn container_entry_config(
    session: tauri::State<'_, Option<GuiSession>>,
) -> Result<serde_json::Value, String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    let mut name: Option<String> = None;
    let mut icon: Option<String> = host_entry_icon(&sess.container_name);
    // server 侧优先（容器内 config.json 是主维护源）
    if let Ok(resp) = send_json_request(
        sess,
        "config.get".to_string(),
        serde_json::to_value(CfgGet).map_err(|e| e.to_string())?,
    )
    .await
    {
        if resp.err.is_none() {
            let cfg: CfgGetResp = serde_json::from_value(resp.payload)
                .map_err(|e| format!("解析 config.get 响应失败：{e}"))?;
            name = cfg
                .config
                .get("entry_name")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            if let Some(p) =
                cfg.config.get("entry_icon").and_then(|v| v.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
            {
                icon = Some(p);
            }
        }
    }
    Ok(serde_json::json!({ "name": name, "icon": icon }))
}

/// 设置容器导出显示名（同步到容器 config.json 的 entry_name，server 维护；
/// None/空 = 清除）。UI 输入框变更时同步。
#[tauri::command]
pub async fn container_set_entry_name(
    session: tauri::State<'_, Option<GuiSession>>,
    name: Option<String>,
) -> Result<(), String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    register_entry_name_in_container(sess, name.as_deref()).await
}

/// 容器入口当前图标源路径（UI 预览用）：主来源（宿主 ConfigFile.icon，宿主或容器
/// 路径）；未设置 = 内置品牌图标。前端用 `icon://<路径>` 显示（server 容器优先、
/// 宿主回退取字节；XPM 等后端转 PNG）。
#[tauri::command]
pub async fn container_entry_icon(
    session: tauri::State<'_, Option<GuiSession>>,
) -> Result<String, String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    match host_entry_icon(&sess.container_name) {
        Some(p) => Ok(p),
        None => easytidy_core::desktop::ensure_container_entry_icon(
            &sess.container_name,
            &sess.container_name,
        )
        .ok_or_else(|| "生成品牌图标失败".to_string()),
    }
}

/// 读取容器入口图标源（主来源：宿主 ConfigFile.icon；None = 未设置）。供 GUI 预填
/// 图标输入框（跨会话持久化）。
#[tauri::command]
pub async fn container_entry_icon_source(
    session: tauri::State<'_, Option<GuiSession>>,
) -> Result<Option<String>, String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    Ok(host_entry_icon(&sess.container_name))
}

/// 设置容器入口图标源：把图标源路径（宿主或容器路径）写入**主来源**（宿主
/// `ConfigFile.icon`）+ **镜像**到容器 `config.json` 的 `entry_icon`（None = 清除）。
/// 导出/设置成功后由 GUI 调用，使入口图标跨会话保留。
#[tauri::command]
pub async fn container_set_entry_icon(
    session: tauri::State<'_, Option<GuiSession>>,
    path: Option<String>,
) -> Result<Option<String>, String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    let path = path
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty() && !p.starts_with("data:") && !p.starts_with("file://"));
    // 主来源：宿主 ConfigFile.icon
    set_host_entry_icon(&sess.container_name, path.as_deref()).map_err(|e| e.to_string())?;
    // 镜像：容器 config.json entry_icon（供 worker GUI 经 server 查询）
    register_entry_icon_in_container(sess, path.as_deref()).await?;
    tracing::info!(
        "容器入口图标源已设置：{} ← {}",
        sess.container_name,
        path.as_deref().unwrap_or("(清除)")
    );
    Ok(path)
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
