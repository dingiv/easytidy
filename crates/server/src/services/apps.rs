//! 桌面应用服务：.desktop 枚举/图标解析/launch + 托管进程（spawn/reaper）。
use crate::state::{ChildInfo, ProcessStatus, ServerState, APP_LOG_MAX};
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
    AppGetIcon, AppGetIconResp, AppInfo, AppKill, AppLogs, AppLogsResp, AppsLaunch, AppsLaunchResp,
    AppsLaunchResult, AppsListResp, AppsPsResp, ChildExited, Frame, ManagedProcess, Message,
    MsgKind,
};
use tokio::process::Command as TokioCommand;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

/// 当前 unix millis（进程时间戳用；跨平台不可靠但容器内仅 Linux，够用）
fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 已退出进程在注册表中的保留时长（ms）：apps.ps 展示「已退出」状态的窗口，
/// 超过即由 prune 清理（防内存膨胀）
const EXITED_TTL_MS: u64 = 120_000;

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
/// env 继承 server 进程 env（create 时已注入 DISPLAY 等显示透传变量）。
///
/// **server 全权负责生命周期**：
/// - stdio：stdout/stderr 合并捕获进有界环形缓冲（[`APP_LOG_MAX`]，`apps.logs`
///   查询）
/// - 生命周期：后台 wait，退出后记录退出码（[`ProcessStatus::Exited`]），
///   并经 `event_tx` 发 `child.exited` 事件（无订阅方则静默丢弃）
/// - 注册表：`state.children` 保留（含已退出，供 `apps.ps` 查询），由
///   [`child_prune_task`] 周期清理超时条目
pub(crate) async fn spawn_managed_process(
    state: &Arc<ServerState>,
    cmd: &str,
    kind: &str,
    name: String,
    event_tx: Option<mpsc::UnboundedSender<Frame>>,
) -> Result<u32> {
    if cmd.trim().is_empty() {
        return Err(anyhow!("Empty command"));
    }

    // 新模型：server 即容器默认用户，直接继承身份 spawn——不经 su
    // （su 要求调用方 root，新模型下不存在）。命令串经 shell 执行
    // （entry/passthrough 命令含 `;`/引号/重定向，裸 split_whitespace
    // 直 exec 本来就是错的）。
    let mut child = TokioCommand::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Failed to spawn command (sh -c)")?;

    let pid = child.id().unwrap();

    // 捕获 stdio：stdout/stderr 合并进有界环形缓冲（调试/排障用）
    let stdio_buf = Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
    let streams: [Option<Box<dyn tokio::io::AsyncRead + Unpin + Send>>; 2] = [
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
    ];
    for stream in streams.into_iter().flatten() {
        let buf = stdio_buf.clone();
        tokio::spawn(read_stream_into_buffer(stream, buf));
    }

    let started_at = unix_millis();
    {
        let mut children = state.children.write().await;
        children.insert(
            pid,
            ChildInfo {
                kind: kind.to_string(),
                name: name.clone(),
                cmd: cmd.to_string(),
                started_at,
                status: ProcessStatus::Running,
                stdio: stdio_buf,
            },
        );
    }

    // 后台 wait：退出后记录状态 + 发 child.exited 事件（条目保留供 apps.ps
    // 查询，由 child_prune_task 过期清理）
    let state = state.clone();
    let kind_owned = kind.to_string();
    tokio::spawn(async move {
        let code = match child.wait().await {
            Ok(status) => status.code().unwrap_or(-1),
            Err(e) => {
                error!("Failed to wait for managed process {pid}: {e}");
                -1
            }
        };
        let at = unix_millis();
        {
            let mut children = state.children.write().await;
            if let Some(info) = children.get_mut(&pid) {
                info.status = ProcessStatus::Exited { code, at };
            }
        }
        info!("Managed process exited: pid={pid}, kind={kind_owned}, code={code}");
        if let Some(tx) = &event_tx {
            // 无订阅方（启动期拉起 entry，尚未有连接）时 send 静默失败
            let _ = tx.send(Frame::Json(Message {
                id: 0,
                kind: MsgKind::Evt,
                op: "child.exited".to_string(),
                payload: serde_json::to_value(ChildExited {
                    pid,
                    code,
                    kind: kind_owned,
                })
                .unwrap_or(serde_json::json!(null)),
                err: None,
            }));
        }
    });

    Ok(pid)
}

/// 把子进程 stdout/stderr 读入有界环形缓冲（超出 [`APP_LOG_MAX`] 丢最旧）。
async fn read_stream_into_buffer(
    mut stream: impl tokio::io::AsyncRead + Unpin + Send + 'static,
    buf: Arc<std::sync::Mutex<std::collections::VecDeque<u8>>>,
) {
    use tokio::io::AsyncReadExt;
    let mut chunk = vec![0u8; 4096];
    loop {
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let mut b = buf.lock().unwrap();
                b.extend(chunk[..n].iter().copied());
                while b.len() > APP_LOG_MAX {
                    b.pop_front();
                }
            }
        }
    }
}

/// Handle apps.launch（passthrough auto-start：批量拉起，逐条独立成败）
pub(crate) async fn handle_apps_launch(
    msg: Message,
    state: &Arc<ServerState>,
    event_tx: mpsc::UnboundedSender<Frame>,
) -> Result<Frame> {
    let req: AppsLaunch = serde_json::from_value(msg.payload)
        .context("Failed to parse AppsLaunch")?;

    info!("apps.launch：{} 个应用", req.apps.len());
    let mut results = Vec::with_capacity(req.apps.len());
    for app in &req.apps {
        match spawn_managed_process(
            state,
            &app.cmd,
            "passthrough",
            app.name.clone(),
            Some(event_tx.clone()),
        )
        .await
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

/// 列出托管进程（含已退出，供生命周期监控；先 prune 过期条目）
pub(crate) async fn handle_apps_ps(msg: Message, state: &Arc<ServerState>) -> Result<Frame> {
    prune_children(state).await;
    let children = state.children.read().await;
    let mut processes: Vec<ManagedProcess> = children
        .iter()
        .map(|(pid, info)| ManagedProcess {
            pid: *pid,
            name: info.name.clone(),
            kind: info.kind.clone(),
            cmd: info.cmd.clone(),
            started_at: info.started_at,
            status: match info.status {
                ProcessStatus::Running => "running".to_string(),
                ProcessStatus::Exited { .. } => "exited".to_string(),
            },
            exit_code: info.status.exit_code(),
            stdio_len: info.stdio.lock().unwrap().len(),
        })
        .collect();
    processes.sort_by_key(|p| p.started_at);
    Ok(Frame::Json(Message {
        id: msg.id,
        kind: MsgKind::Resp,
        op: "apps.ps".to_string(),
        payload: serde_json::to_value(AppsPsResp { processes })?,
        err: None,
    }))
}

/// 获取某托管进程捕获的 stdio（有损 UTF-8；进程不存在返回空）
pub(crate) async fn handle_apps_logs(msg: Message, state: &Arc<ServerState>) -> Result<Frame> {
    let req: AppLogs = serde_json::from_value(msg.payload)
        .context("Failed to parse AppLogs")?;
    let children = state.children.read().await;
    let stdio = children
        .get(&req.pid)
        .map(|info| {
            let mut buf = info.stdio.lock().unwrap();
            String::from_utf8_lossy(buf.make_contiguous()).to_string()
        })
        .unwrap_or_default();
    Ok(Frame::Json(Message {
        id: msg.id,
        kind: MsgKind::Resp,
        op: "apps.logs".to_string(),
        payload: serde_json::to_value(AppLogsResp { pid: req.pid, stdio })?,
        err: None,
    }))
}

/// 终止托管进程（SIGTERM；退出由 wait 任务记录）。
pub(crate) async fn handle_apps_kill(msg: Message, state: &Arc<ServerState>) -> Result<Frame> {
    let req: AppKill = serde_json::from_value(msg.payload)
        .context("Failed to parse AppKill")?;
    let running = {
        let children = state.children.read().await;
        matches!(
            children.get(&req.pid).map(|i| i.status),
            Some(ProcessStatus::Running)
        )
    };
    if !running {
        return Err(anyhow!("进程 {} 不存在或已退出", req.pid));
    }
    let status = TokioCommand::new("kill")
        .arg("-TERM")
        .arg(req.pid.to_string())
        .status()
        .await
        .context("Failed to send SIGTERM")?;
    if !status.success() {
        return Err(anyhow!("SIGTERM 失败（exit={}）", status));
    }
    info!("Managed process terminated: pid={}", req.pid);
    Ok(Frame::Json(Message {
        id: msg.id,
        kind: MsgKind::Resp,
        op: "apps.kill".to_string(),
        payload: serde_json::json!(null),
        err: None,
    }))
}

/// 清理已退出且超过保留时长的托管进程条目（防注册表无限增长）。
async fn prune_children(state: &Arc<ServerState>) {
    let now = unix_millis();
    let mut children = state.children.write().await;
    children.retain(|_, info| match info.status {
        ProcessStatus::Running => true,
        ProcessStatus::Exited { at, .. } => now.saturating_sub(at) < EXITED_TTL_MS,
    });
}

/// 周期清理任务：过期退出条目（替代旧的 kill -0 轮询 reaper——wait 任务
/// 已负责精确退出记录，轮询既冗余又可能误判复用的 PID）。
pub(crate) async fn child_prune_task(state: Arc<ServerState>) {
    info!("Child prune task started");
    loop {
        tokio::time::sleep(Duration::from_secs(30)).await;
        prune_children(&state).await;
        if state.shutting_down.load(Ordering::SeqCst) {
            break;
        }
    }
    info!("Child prune task ended");
}
