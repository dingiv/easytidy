//! 桌面应用服务：.desktop 枚举/图标解析/launch + 托管进程（spawn/reaper）。
use crate::state::{ChildInfo, ServerState};
use crate::setup::user_map;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use base64::Engine;
use easytidy_protocol::{
    AppGetIcon, AppGetIconResp, AppInfo, AppsLaunch, AppsLaunchResp,
    AppsLaunchResult, AppsListResp, Frame, Message, MsgKind,
};
use tokio::process::Command as TokioCommand;
use tracing::{error, info, warn};

/// Handle apps.list
pub(crate) async fn handle_apps_list(msg: Message) -> Result<Frame> {
    let mut apps = Vec::new();

    // 扫描标准 .desktop 位置：系统目录 + 容器用户 home + server 自身 home
    // （server 以 root 运行，$HOME=/root；用户 home 是 /home/node——之前
    // 只扫 ~/ 漏掉用户 home 的 .desktop，实测）
    let mut paths: Vec<String> = vec![
        "/usr/share/applications".to_string(),
        "/usr/local/share/applications".to_string(),
    ];
    if let Some(user) = user_map() {
        paths.push(format!("{}/.local/share/applications", user.home));
    }
    if let Ok(home) = std::env::var("HOME") {
        paths.push(format!("{home}/.local/share/applications"));
    }

    for base in &paths {
        let base_path = PathBuf::from(base);

        if let Ok(iter) = fs::read_dir(base_path) {
            for entry in iter.flatten() {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some("desktop") {
                    continue;
                }

                if let Ok(app) = parse_desktop_file(&path) {
                    apps.push(app);
                }
            }
        }
    }

    Ok(Frame::Json(Message {
        id: msg.id,
        kind: MsgKind::Resp,
        op: "apps.list".to_string(),
        payload: serde_json::to_value(AppsListResp { apps })?,
        err: None,
    }))
}

/// Parse a .desktop file
pub(crate) fn parse_desktop_file(path: &Path) -> Result<AppInfo> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read .desktop file: {}", path.display()))?;

    let mut name = None;
    let mut icon_path = None;
    let mut exec = None;
    let mut comment = None;
    let mut categories = None;
    let mut startup_notify = false;
    let mut startup_wm_class = None;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
            continue;
        }

        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            let value = value.trim();

            match key {
                "Name" => name = Some(value.to_string()),
                // Icon 常为主题名（如 "google-chrome"）而非路径——解析成实际
                // 图标文件，宿主 passthrough 才能搬运；解析失败保留原值
                "Icon" => icon_path = resolve_icon_path(value).or(Some(value.to_string())),
                "Exec" => exec = Some(value.to_string()),
                "Comment" => comment = Some(value.to_string()),
                // NoDisplay 不再过滤（扫全：passthrough 场景用户要看到所有
                // .desktop，如 python3.12.desktop 的 NoDisplay=true，实测遗漏）
                "Categories" => categories = Some(value.to_string()),
                "StartupNotify" => startup_notify = value == "true",
                "StartupWMClass" => startup_wm_class = Some(value.to_string()),
                _ => {}
            }
        }
    }

    let name = name.ok_or_else(|| anyhow!("Missing Name"))?;
    let exec = exec.ok_or_else(|| anyhow!("Missing Exec"))?;

    Ok(AppInfo {
        desktop_file: path.display().to_string(),
        name,
        icon_path,
        exec,
        comment,
        categories,
        startup_notify,
        startup_wm_class,
    })
}

/// 把 .desktop 的 Icon 值解析为容器内实际图标文件路径。
///
/// Icon= 常为主题名（如 "google-chrome"）而非路径，按图标主题标准位置
/// 依次探测（hicolor 多尺寸 + Adwaita + pixmaps，svg/png 均试）；已是
/// 绝对路径或相对路径则原样返回。解析失败返回 None（保留原值显示）。
pub(crate) fn resolve_icon_path(icon: &str) -> Option<String> {
    if icon.starts_with('/') || icon.contains('/') {
        return Some(icon.to_string());
    }
    let (base, exts): (&str, &[&str]) = if icon.ends_with(".svg") || icon.ends_with(".png") {
        (icon.trim_end_matches(".svg").trim_end_matches(".png"), &["svg", "png"])
    } else {
        (icon, &["svg", "png"])
    };
    let sizes = ["256x256", "128x128", "64x64", "48x48", "32x32", "24x24", "16x16"];
    for size in sizes {
        for ext in exts {
            for theme_root in ["/usr/share/icons/hicolor", "/usr/share/icons/Adwaita"] {
                let p = format!("{theme_root}/{size}/apps/{base}.{ext}");
                if Path::new(&p).exists() {
                    return Some(p);
                }
            }
        }
    }
    for ext in exts {
        let p = format!("/usr/share/pixmaps/{base}.{ext}");
        if Path::new(&p).exists() {
            return Some(p);
        }
    }
    None
}

/// Handle apps.getIcon
pub(crate) async fn handle_apps_get_icon(msg: Message) -> Result<Frame> {
    let req: AppGetIcon = serde_json::from_value(msg.payload)
        .context("Failed to parse AppGetIcon")?;

    let path = Path::new(&req.path);

    let data = fs::read(path)
        .with_context(|| format!("Failed to read icon: {}", path.display()))?;

    let format = path.extension()
        .and_then(|s| s.to_str())
        .unwrap_or("png")
        .to_string();

    let data_b64 = base64::engine::general_purpose::STANDARD.encode(&data);

    Ok(Frame::Json(Message {
        id: msg.id,
        kind: MsgKind::Resp,
        op: "apps.getIcon".to_string(),
        payload: serde_json::to_value(AppGetIconResp { format, data_b64 })?,
        err: None,
    }))
}

/// 拉起一个受管子进程（entry / passthrough auto-start 共用）。
///
/// 用户映射生效时经 su 以容器内宿主用户拉起（cmd 为整条 shell 命令串，
/// 直接作 su -c 参数交给目标用户 shell 解析——**不做单引号转义**：util-linux
/// su 经 argv 传参，不经宿主 shell 二次解析；转义会破坏含引号/重定向的
/// 复杂命令（实测 auto-start 命令 127 失败））；否则 root 直接 spawn。
/// stdout/stderr 丢弃（GUI 应用自管窗口）；注册 state.children 并后台
/// wait（退出后移除 + 日志）。env 继承 server 进程 env（create 时已注入
/// DISPLAY 等显示透传变量）。
pub(crate) async fn spawn_managed_process(
    state: &Arc<ServerState>,
    cmd: &str,
    kind: &str,
    entry_id: Option<String>,
) -> Result<u32> {
    if cmd.trim().is_empty() {
        return Err(anyhow!("Empty command"));
    }

    let mut child = if let Some(user) = user_map() {
        TokioCommand::new("su")
            .arg("-c")
            .arg(cmd)
            .arg(&user.name)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("Failed to spawn command (su)")?
    } else {
        let parts: Vec<&str> = cmd.split_whitespace().collect();
        let cmd0 = parts[0];
        let args = &parts[1..];
        TokioCommand::new(cmd0)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("Failed to spawn command")?
    };

    let pid = child.id().unwrap();

    // Track child
    {
        let mut children = state.children.write().await;
        children.insert(pid, ChildInfo {
            pid,
            kind: kind.to_string(),
            entry_id: entry_id.clone(),
        });
    }

    // Wait for child in background（退出后移除跟踪）
    let state = state.clone();
    let kind_owned = kind.to_string();
    tokio::spawn(async move {
        match child.wait().await {
            Ok(status) => {
                info!("Managed process exited: pid={pid}, kind={kind_owned}, status={status}");
                let mut children = state.children.write().await;
                children.remove(&pid);
            }
            Err(e) => {
                error!("Failed to wait for managed process {pid}: {e}");
                let mut children = state.children.write().await;
                children.remove(&pid);
            }
        }
    });

    Ok(pid)
}

/// Launch entry command on startup
pub(crate) async fn launch_entry_command(state: &Arc<ServerState>, entry_cmd: String) -> Result<()> {
    info!("Launching entry command: {}", entry_cmd);
    let pid = spawn_managed_process(state, &entry_cmd, "entry", Some("default".to_string())).await?;
    info!("Entry command launched: pid={pid}");
    Ok(())
}

/// Handle apps.launch（passthrough auto-start：批量拉起，逐条独立成败）
pub(crate) async fn handle_apps_launch(msg: Message, state: &Arc<ServerState>) -> Result<Frame> {
    let req: AppsLaunch = serde_json::from_value(msg.payload)
        .context("Failed to parse AppsLaunch")?;

    info!("apps.launch：{} 个应用", req.apps.len());
    let mut results = Vec::with_capacity(req.apps.len());
    for app in &req.apps {
        match spawn_managed_process(state, &app.cmd, "passthrough", Some(app.name.clone())).await
        {
            Ok(pid) => {
                info!("passthrough 应用已拉起：{} (pid={pid})", app.name);
                results.push(AppsLaunchResult {
                    name: app.name.clone(),
                    pid: Some(pid),
                    error: None,
                });
            }
            Err(e) => {
                // 单条失败不阻断其余（如 cmd 不存在/为空）
                warn!("passthrough 应用拉起失败：{}：{e}", app.name);
                results.push(AppsLaunchResult {
                    name: app.name.clone(),
                    pid: None,
                    error: Some(e.to_string()),
                });
            }
        }
    }

    Ok(Frame::Json(Message {
        id: msg.id,
        kind: MsgKind::Resp,
        op: "apps.launch".to_string(),
        payload: serde_json::to_value(AppsLaunchResp { results })?,
        err: None,
    }))
}

/// Child reaper task
pub(crate) async fn child_reaper_task(state: Arc<ServerState>) {
    info!("Child reaper task started");

    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;

        let children_to_remove = {
            let children = state.children.read().await;
            let mut to_remove = Vec::new();

            for pid in children.keys() {
                // Check if process is still alive
                if let Ok(output) = TokioCommand::new("kill")
                    .arg("-0")
                    .arg(pid.to_string())
                    .output()
                    .await
                {
                    if !output.status.success() {
                        to_remove.push(*pid);
                    }
                }
            }

            to_remove
        };

        for pid in children_to_remove {
            let mut children = state.children.write().await;
            if let Some(child_info) = children.remove(&pid) {
                info!("Child reaped: pid={}, kind={}", pid, child_info.kind);
                // TODO: Emit child.exited event
            }
        }

        if state.shutting_down.load(Ordering::SeqCst) {
            break;
        }
    }

    info!("Child reaper task ended");
}
