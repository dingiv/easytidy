//! 桌面应用服务：.desktop 枚举/图标解析/launch + 托管进程（spawn/reaper）。
use crate::state::{ChildInfo, ProcessStatus, ServerState, APP_LOG_MAX};
use crate::setup::user_map;
use crate::services::desktop::parse_desktop_file;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use base64::Engine;
use easytidy_protocol::{
    AppGetIcon, AppGetIconResp, AppKill, AppLaunchApp, AppLaunchAppResp, AppLogs, AppLogsResp,
    AppsLaunch, AppsLaunchResp, AppsLaunchResult, AppsListResp, AppsPsResp, AppInfo,
    ChildExited, Frame, ManagedProcess, Message, MsgKind,
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

/// Handle apps.list（扫描 → 登记表刷新 → 返回列表）
pub(crate) async fn handle_apps_list(msg: Message) -> Result<Frame> {
    let apps = refresh_registry();

    Ok(Frame::Json(Message {
        id: msg.id,
        kind: MsgKind::Resp,
        op: "apps.list".to_string(),
        payload: serde_json::to_value(AppsListResp { apps })?,
        err: None,
    }))
}

/// 扫描容器内标准 .desktop 位置（系统目录 + 容器用户 home + server 自身
/// home；server 以 root 运行时 $HOME=/root，用户 home 是 /home/node——
/// 两者都要扫，实测）。
fn scan_apps() -> Vec<AppInfo> {
    let mut apps = Vec::new();
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
        if let Ok(iter) = fs::read_dir(&base_path) {
            for entry in iter.flatten() {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some("desktop") {
                    continue;
                }
                if let Ok(mut app) = parse_desktop_file(&path) {
                    app.id = compute_app_id(&app);
                    apps.push(app);
                }
            }
        }
    }
    apps
}

/// 稳定应用 id：`pt-<sha256 前 12 hex>`，哈希输入 = .desktop 读取的
/// 全部字段（Name/Icon/Exec/Comment/Categories/StartupNotify/StartupWMClass）。
///
/// 同一内容（同一图标）跨目录 / 跨扫描恒同；任一字段变化（如 exec 命令
/// 变更）= 新 id——id 是快捷方式的「内容身份」。字段间用 `\u{1f}` 分隔
/// + `key=` 前缀，消除边界歧义。
fn compute_app_id(app: &AppInfo) -> String {
    use sha2::{Digest, Sha256};
    let canon = format!(
        "name={}\u{1f}icon={}\u{1f}exec={}\u{1f}comment={}\u{1f}categories={}\u{1f}startup_notify={}\u{1f}startup_wm_class={}",
        app.name,
        app.icon_path.as_deref().unwrap_or(""),
        app.exec,
        app.comment.as_deref().unwrap_or(""),
        app.categories.as_deref().unwrap_or(""),
        app.startup_notify,
        app.startup_wm_class.as_deref().unwrap_or(""),
    );
    let mut hasher = Sha256::new();
    hasher.update(canon.as_bytes());
    let digest = hasher.finalize();
    format!("pt-{}", &hex::encode(digest)[..12])
}

/// 登记表磁盘格式（{user.home}/.easytidy/apps.toml；每次扫描全量重写，
/// 随容器层/快照持久）
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct AppRegistry {
    schema_version: u32,
    /// 扫描时间（unix 秒；排障用）
    scanned_at: u64,
    apps: Vec<AppInfo>,
}

/// 扫描 + 刷新登记表（全量重写；写失败仅告警，不阻断列表返回）。
///
/// 按 id 去重（保留先扫描者）：同一 .desktop 内容在多个 XDG 目录重复
/// （系统 + 用户自定义）时 id 相同，只留一条。
pub(crate) fn refresh_registry() -> Vec<AppInfo> {
    let mut apps = scan_apps();
    apps = dedup_by_id(apps);

    let scanned_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let registry = AppRegistry {
        schema_version: 1,
        scanned_at,
        apps: apps.clone(),
    };
    if let Err(e) = write_registry(&registry) {
        warn!("应用登记表写入失败（忽略）：{e}");
    }
    apps
}

/// 原子写登记表到固定路径（{user.home}/.easytidy/apps.toml）
fn write_registry(registry: &AppRegistry) -> Result<()> {
    write_registry_to(&crate::storage::apps_registry_path(), registry)
}

/// 原子写登记表（tmp + rename；可注入路径，单测用）
fn write_registry_to(path: &Path, registry: &AppRegistry) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建登记表目录失败：{}", parent.display()))?;
    }
    let content = toml::to_string_pretty(registry).context("序列化登记表失败")?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, &content).with_context(|| format!("写登记表失败：{}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| "登记表原子替换失败".to_string())?;
    Ok(())
}

/// 读登记表（文件缺失/解析失败 → None，仅告警）
pub(crate) fn read_registry_from(path: &Path) -> Option<AppRegistry> {
    let content = std::fs::read_to_string(path).ok()?;
    match toml::from_str::<AppRegistry>(&content) {
        Ok(r) => Some(r),
        Err(e) => {
            warn!("解析应用登记表失败（忽略）：{e}");
            None
        }
    }
}

/// 按 id 去重（保留先扫描者）：同一内容在多个 XDG 目录重复时 id 相同
fn dedup_by_id(mut apps: Vec<AppInfo>) -> Vec<AppInfo> {
    let mut seen = std::collections::HashSet::new();
    apps.retain(|a| seen.insert(a.id.clone()));
    apps
}

/// 解析应用引用（id 或名称）→ (id, name, exec)。server 全权决定「启动谁、
/// 如何启动」：调用方只传引用。
///
/// 解析顺序：扫描登记表 id 精确 → 名称精确（忽略大小写）→ 自定义应用
/// id 精确（`custom:<name>`）→ 自定义应用名称精确。
///
/// 登记表读取优先（快、不重复扫盘）；文件缺失/为空（新容器、尚未扫描）
/// 回退一次新鲜扫描（顺带建登记表）。
fn resolve_app(ref_: &str) -> Option<(String, String, String)> {
    let mut apps = read_registry_from(&crate::storage::apps_registry_path())
        .map(|r| r.apps)
        .unwrap_or_default();
    if apps.is_empty() {
        apps = refresh_registry();
    }
    if let Some(a) = apps.iter().find(|a| a.id == ref_) {
        return Some((a.id.clone(), a.name.clone(), a.exec.clone()));
    }
    if let Some(a) = apps.iter().find(|a| a.name.eq_ignore_ascii_case(ref_)) {
        return Some((a.id.clone(), a.name.clone(), a.exec.clone()));
    }
    // 2) 自定义应用（passthrough.toml 用户态配置）
    if let Some(app) = crate::services::passthrough::custom_apps()
        .into_iter()
        .find(|a| a.id == ref_ || a.name.eq_ignore_ascii_case(ref_))
    {
        return Some((app.id, app.name, app.cmd));
    }
    None
}

/// 可用引用列表（未找到时的报错提示：扫描 id + 自定义 id）
fn available_refs() -> Vec<String> {
    let mut out: Vec<String> = refresh_registry().into_iter().map(|a| a.id).collect();
    for app in crate::services::passthrough::custom_apps() {
        if !out.contains(&app.id) {
            out.push(app.id);
        }
    }
    out
}

/// Handle apps.launch_app（按 id/name 拉起：server 查登记表决定 exec）
pub(crate) async fn handle_apps_launch_app(
    msg: Message,
    state: &Arc<ServerState>,
    event_tx: mpsc::UnboundedSender<Frame>,
) -> Result<Frame> {
    let req: AppLaunchApp = serde_json::from_value(msg.payload)
        .context("Failed to parse AppLaunchApp")?;

    info!("apps.launch_app：{}", req.id_or_name);
    let resp = match resolve_app(&req.id_or_name) {
        Some((id, name, exec)) => match spawn_managed_process(
            state,
            &exec,
            "passthrough",
            name.clone(),
            Some(event_tx.clone()),
        )
        .await
        {
            Ok(pid) => {
                info!("应用已拉起：{} (id={}, pid={pid})", name, id);
                AppLaunchAppResp {
                    id: Some(id),
                    name: Some(name),
                    pid: Some(pid),
                    error: None,
                    available: None,
                }
            }
            Err(e) => {
                warn!("应用拉起失败：{} (id={id})：{e}", name);
                AppLaunchAppResp {
                    id: Some(id),
                    name: Some(name),
                    pid: None,
                    error: Some(e.to_string()),
                    available: None,
                }
            }
        },
        None => AppLaunchAppResp {
            id: None,
            name: None,
            pid: None,
            error: Some(format!("未找到应用：{}", req.id_or_name)),
            available: Some(available_refs()),
        },
    };

    Ok(Frame::Json(Message {
        id: msg.id,
        kind: MsgKind::Resp,
        op: "apps.launch_app".to_string(),
        payload: serde_json::to_value(resp)?,
        err: None,
    }))
}

// .desktop 解析（parse_desktop_file / resolve_icon_path）已抽到 crate::services::desktop；
// apps 仅经上方 use 导入 parse_desktop_file（枚举时调用）。

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
    //
    // **登录 shell**（bash -l）：托管进程的环境须与终端一致——dev 工具链
    // （rustup/nvm/conda…）的 PATH 都在 /etc/profile、~/.profile 里，裸
    // sh -c 的 PATH 只有系统基础目录，cargo/node 等直接找不到（实测
    // desk-pilot dev-up.sh 起不来）。bash 不存在时回退 sh -c（不加载登录
    // env；嵌入式容器场景）。
    // 注意：~/.bashrc 的非交互守卫仍会跳过 bashrc 内的 PATH 初始化——
    // 工具链装在 .bashrc 里的场景，用户应在脚本里自行 source 或改用 .profile。
    let use_bash = std::path::Path::new("/bin/bash").exists();
    let mut spawn_cmd = if use_bash {
        let mut c = TokioCommand::new("bash");
        c.arg("-l").arg("-c");
        c
    } else {
        let mut c = TokioCommand::new("sh");
        c.arg("-c");
        c
    };
    let shell_desc = if use_bash { "bash -lc" } else { "sh -c" };
    let mut child = spawn_cmd
        .arg(cmd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Failed to spawn command ({shell_desc})"))?;

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
            stdio_len: info.stdio.lock().unwrap().len() as u64,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_app(name: &str, exec: &str, desktop_file: &str) -> AppInfo {
        AppInfo {
            id: String::new(),
            desktop_file: desktop_file.to_string(),
            name: name.to_string(),
            icon_path: Some(format!("/usr/share/icons/hicolor/48x48/apps/{name}.png")),
            exec: exec.to_string(),
            comment: None,
            categories: Some("Utility;".to_string()),
            startup_notify: false,
            startup_wm_class: None,
        }
    }

    #[test]
    fn test_compute_app_id_stable_and_format() {
        let a = fixture_app("Foo", "foo --flag", "/usr/share/applications/foo.desktop");
        let b = fixture_app("Foo", "foo --flag", "/usr/local/share/applications/foo.desktop");
        let id_a = compute_app_id(&a);
        // 同一内容跨目录：id 恒同（不含路径参与哈希）
        assert_eq!(id_a, compute_app_id(&b));
        assert_eq!(id_a, compute_app_id(&a), "同字段重复计算应稳定");
        // 格式：pt-<12 hex>
        assert!(id_a.starts_with("pt-"));
        assert_eq!(id_a.len(), 3 + 12);
        assert!(id_a[3..].chars().all(|c| c.is_ascii_digit() || matches!(c, 'a'..='f')));
    }

    #[test]
    fn test_compute_app_id_content_change_different_id() {
        let a = fixture_app("Foo", "foo", "/usr/share/applications/foo.desktop");
        let b = fixture_app("Foo", "foo --new-flag", "/usr/share/applications/foo.desktop");
        assert_ne!(compute_app_id(&a), compute_app_id(&b), "exec 变化 = 新内容身份");
        let c = fixture_app("Bar", "foo", "/usr/share/applications/foo.desktop");
        assert_ne!(compute_app_id(&a), compute_app_id(&c), "name 变化 = 新内容身份");
    }

    #[test]
    fn test_registry_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("apps.toml");
        let registry = AppRegistry {
            schema_version: 1,
            scanned_at: 1234567890,
            apps: vec![fixture_app("Foo", "foo", "/usr/share/applications/foo.desktop")],
        };
        write_registry_to(&path, &registry).unwrap();
        let loaded = read_registry_from(&path).expect("登记表应可解析");
        assert_eq!(loaded.schema_version, 1);
        assert_eq!(loaded.scanned_at, 1234567890);
        assert_eq!(loaded.apps.len(), 1);
        assert_eq!(loaded.apps[0].name, "Foo");
    }

    #[test]
    fn test_registry_missing_file_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_registry_from(&dir.path().join("nope.toml")).is_none());
    }

    #[test]
    fn test_dedup_by_id_keeps_first() {
        let mut a = fixture_app("Foo", "foo", "/usr/share/applications/foo.desktop");
        let b = fixture_app("Foo", "foo", "/usr/local/share/applications/foo.desktop");
        let c = fixture_app("Bar", "bar", "/usr/share/applications/bar.desktop");
        a.id = compute_app_id(&a);
        let mut b2 = b.clone();
        b2.id = compute_app_id(&b2);
        let mut c2 = c.clone();
        c2.id = compute_app_id(&c2);
        // 同 id（Foo 两份）+ 不同 id（Bar）→ 去重后 2 条，Foo 保留先扫描者
        let out = dedup_by_id(vec![a, b2, c2]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].desktop_file, "/usr/share/applications/foo.desktop");
        assert_eq!(out[1].name, "Bar");
    }
}
