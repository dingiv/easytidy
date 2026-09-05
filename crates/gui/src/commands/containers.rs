//! Master GUI 命令：容器生命周期 + 环境（env）语义。
//!
//! 模板管理(fui 侧)由 conf YAML 取代 flavor TOML——见 `commands::config::*`。
//! CLI 仍使用 core::flavor；GUI 模板 tab 直接读写 `~/.easytidy/conf`。

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use tracing::{debug, error, info, warn};

use easytidy_core::configfile::ConfigFile;
use easytidy_core::desktop;
use easytidy_core::env::inject_passthrough;
use easytidy_core::models::{ContainerConfig, ContainerSummary};

use crate::commands::socket::send_json_request;
use crate::state::{GuiSession, PodmanState};
use easytidy_protocol::ops::LifecycleShutdown;

// ============================================================================
// Master GUI 命令（容器生命周期管理）
// ============================================================================

/// 容器生命周期命令的统一错误出口：完整信息返回前端（UI Modal 展示）
/// + error! 落盘日志（~/.easytidy/logs/easytidy-gui.log，排障唯一持久出口）。
fn ferr(ctx: &str, e: impl std::fmt::Display) -> String {
    let msg = format!("{ctx}失败：{e}");
    error!("{msg}");
    msg
}

/// 列出所有容器
#[tauri::command]
pub async fn list_containers(
    podman: tauri::State<'_, PodmanState>,
) -> Result<Vec<ContainerSummary>, String> {
    let p = podman.get().await.map_err(|e| e.to_string())?;
    let result = p.list_containers().await.map_err(|e| e.to_string())?;
    podman.return_podman(p).await;
    Ok(result)
}

/// 启动容器
#[tauri::command]
pub async fn start_container(
    podman: tauri::State<'_, PodmanState>,
    name: String,
) -> Result<(), String> {
    let p = podman.get().await.map_err(|e| ferr("连接 podman", e))?;
    p.start(&name)
        .await
        .map_err(|e| ferr(&format!("启动容器 {name}"), e))?;
    podman.return_podman(p).await;
    Ok(())
}

/// 停止容器
#[tauri::command]
pub async fn stop_container(
    podman: tauri::State<'_, PodmanState>,
    name: String,
) -> Result<(), String> {
    let p = podman.get().await.map_err(|e| ferr("连接 podman", e))?;
    p.stop(&name)
        .await
        .map_err(|e| ferr(&format!("停止容器 {name}"), e))?;
    podman.return_podman(p).await;
    Ok(())
}

/// 重启容器
#[tauri::command]
pub async fn restart_container(
    podman: tauri::State<'_, PodmanState>,
    name: String,
) -> Result<(), String> {
    let p = podman.get().await.map_err(|e| ferr("连接 podman", e))?;
    p.restart(&name)
        .await
        .map_err(|e| ferr(&format!("重启容器 {name}"), e))?;
    podman.return_podman(p).await;
    Ok(())
}

/// 删除容器
#[tauri::command]
pub async fn remove_container(
    podman: tauri::State<'_, PodmanState>,
    name: String,
    force: bool,
) -> Result<(), String> {
    let p = podman.get().await.map_err(|e| ferr("连接 podman", e))?;
    p.remove(&name, force)
        .await
        .map_err(|e| ferr(&format!("删除容器 {name}"), e))?;

    // 从配置文件注销
    let config_file =
        ConfigFile::default_instance().map_err(|e| ferr("解析容器配置目录", e))?;
    config_file
        .unregister_container(&name)
        .map_err(|e| ferr("注销容器配置", e))?;

    // 卸载桌面图标
    let _ = desktop::uninstall_desktop_entry(&name); // 忽略错误

    podman.return_podman(p).await;
    Ok(())
}

/// 检查容器详情
#[tauri::command]
pub async fn inspect_container(
    podman: tauri::State<'_, PodmanState>,
    name: String,
) -> Result<ContainerSummary, String> {
    let p = podman.get().await.map_err(|e| e.to_string())?;
    let containers = p.list_containers().await.map_err(|e| e.to_string())?;
    podman.return_podman(p).await;

    containers
        .into_iter()
        .find(|c| c.name == name)
        .ok_or_else(|| format!("容器不存在：{}", name))
}

/// 容器启动失败展示用信息：状态 + 退出码 + 错误 + 容器日志（podman logs 等价）。
///
/// 用于 Worker GUI 启动时检测到容器未连接时呈现给用户（替代当前静默吞错的
/// "从工具栏打开面板…" 误导文案）。`tail_lines = 0` → 全部日志（上限 10000 行）。
#[derive(Debug, Clone, Serialize)]
pub struct ContainerFailureInfo {
    /// 容器是否存在于 podman（false = 已被删除/从未创建）
    pub exists: bool,
    /// 是否运行中（与 status=="running" 一致；启动失败时为 false）
    pub running: bool,
    /// podman 原生状态字符串（"running" / "exited" / "created" / "configured"）
    pub status: String,
    /// 退出码（仅 exited 容器有值）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    /// podman 上报的失败原因（OCI hook / device 不可用 / 镜像损坏等）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// 容器日志（stdout + stderr 合并；有损 UTF-8；按 tail_lines 取尾）
    pub logs: String,
}

#[tauri::command]
pub async fn container_failure_info(
    podman: tauri::State<'_, PodmanState>,
    name: String,
    tail_lines: Option<usize>,
) -> Result<ContainerFailureInfo, String> {
    let p = podman.get().await.map_err(|e| e.to_string())?;
    let tail = tail_lines.unwrap_or(200);
    let result = fetch_failure_info_inner(&p, &name, tail).await;
    podman.return_podman(p).await;
    result
}

async fn fetch_failure_info_inner(
    p: &easytidy_core::podman::Podman,
    name: &str,
    tail_lines: usize,
) -> Result<ContainerFailureInfo, String> {
    let state = p.container_state(name).await.map_err(|e| e.to_string())?;
    let Some(state) = state else {
        // 容器在 podman 中已不存在（被外部清理或从未真正创建成功）
        return Ok(ContainerFailureInfo {
            exists: false,
            running: false,
            status: "missing".into(),
            exit_code: None,
            error: Some(format!("容器 {name} 在 podman 中不存在（可能已被清理或创建失败后未保留）")),
            logs: String::new(),
        });
    };
    // 仅在非运行状态下拉取日志：运行中的容器 Worker 不会进此分支，
    // 但保险起见（启动竞态）running 时也允许拉（不报错即可）。
    let logs = match p.container_logs(name, tail_lines).await {
        Ok(s) => s,
        Err(e) => {
            // 日志拉取失败不该阻断 status 展示（如容器从未启动过 → 无日志）
            tracing::debug!("拉取容器 {name} 日志失败（忽略）：{e}");
            String::new()
        }
    };
    Ok(ContainerFailureInfo {
        exists: true,
        running: state.running,
        status: state.status,
        exit_code: state.exit_code,
        error: state.error,
        logs,
    })
}

// ============================================================================
// 环境语义面板命令（docs/13-mutable-env-paradigm.md）
//
// 五种环境语义：新环境 / 删除环境 / 快照 / fork / 运行·关闭。
// 引擎语义已由 easytidy-core 实现，此处仅做 GUI 桥接（含 env_rm 的全清理）。
// ============================================================================

/// 环境视图（前端 env_list 契约）。
///
/// status 取值："running"（运行中）/ "exited"、"created"（已停止）/
/// "missing"（仅注册表配置存在，podman 中无容器）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvView {
    /// 环境名
    pub name: String,
    /// 基础镜像
    pub image: String,
    /// 运行状态
    pub status: String,
    /// 是否为 easytidy 管理
    pub managed: bool,
}

// ============================================================================
// 镜像管理命令（GUI 镜像板块）
// ============================================================================

/// 列出所有本地镜像。
#[tauri::command]
pub async fn images_list(
    podman: tauri::State<'_, PodmanState>,
) -> Result<Vec<easytidy_core::models::ImageSummary>, String> {
    let p = podman.get().await.map_err(|e| e.to_string())?;
    let result = p.list_images().await.map_err(|e| e.to_string())?;
    podman.return_podman(p).await;
    Ok(result)
}

/// 拉取镜像（显式动作；创建容器不自动拉取）。
#[tauri::command]
pub async fn image_pull(
    podman: tauri::State<'_, PodmanState>,
    image: String,
) -> Result<(), String> {
    let p = podman.get().await.map_err(|e| e.to_string())?;
    p.pull_image(&image).await.map_err(|e| e.to_string())?;
    podman.return_podman(p).await;
    Ok(())
}

/// 删除镜像（force = 强制删除被容器引用的镜像）。
#[tauri::command]
pub async fn image_remove(
    podman: tauri::State<'_, PodmanState>,
    image: String,
    force: bool,
) -> Result<(), String> {
    let p = podman.get().await.map_err(|e| e.to_string())?;
    p.remove_image(&image, force)
        .await
        .map_err(|e| e.to_string())?;
    podman.return_podman(p).await;
    Ok(())
}

/// 镜像占用查询（删除前防呆）：返回每个镜像被哪些容器使用
/// （`image -> 容器名列表`；空列表 = 未被占用，可删除）。
#[tauri::command]
pub async fn images_used_by(
    podman: tauri::State<'_, PodmanState>,
    images: Vec<String>,
) -> Result<std::collections::HashMap<String, Vec<String>>, String> {
    let p = podman.get().await.map_err(|e| e.to_string())?;
    let result = p
        .images_used_by(&images)
        .await
        .map_err(|e| e.to_string())?;
    podman.return_podman(p).await;
    Ok(result)
}

// ============================================================================
// 环境（env）语义 + conf 模板扩展（doc 镜像管理命令之后、env_list 之前）
// ============================================================================

/// 列出所有容器：easytidy 管理的（带 manager=easytidy 标签）+ 其他人创建的
/// （未接管，managed=false）+ configfile 注册表中 podman 已不存在的配置（missing）。
#[tauri::command]
pub async fn env_list(podman: tauri::State<'_, PodmanState>) -> Result<Vec<EnvView>, String> {
    let p = podman.get().await.map_err(|e| e.to_string())?;
    let containers = p.list_containers().await.map_err(|e| e.to_string())?;
    podman.return_podman(p).await;

    // configfile 注册表：podman 中不存在的配置 → "missing"（仅配置保留）
    let config_file =
        ConfigFile::default_instance().map_err(|e| format!("解析容器配置目录失败：{}", e))?;
    let registered = config_file
        .list_containers()
        .map_err(|e| format!("读取注册表失败：{}", e))?;

    let mut views: Vec<EnvView> = Vec::new();
    let mut covered: HashSet<String> = HashSet::new();
    // podman 里实际存在的容器全部展示：managed 按 manager=easytidy 标签区分；
    // 未接管容器保留其真实 status，前端标记「未接管」。
    for c in containers {
        covered.insert(c.name.clone());
        views.push(EnvView {
            name: c.name,
            image: c.image,
            status: c.status,
            managed: c.managed,
        });
    }
    // 注册表里 podman 已不存在的配置 → "missing"（仅配置保留）
    for cfg in registered {
        if covered.contains(&cfg.name) {
            continue;
        }
        views.push(EnvView {
            name: cfg.name,
            image: cfg.params.image,
            status: "missing".to_string(),
            managed: true,
        });
    }
    views.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(views)
}

/// 模板展开已迁移到 conf YAML（`commands::config::conf_template_get`）：
/// conf 模板 = 完整 ContainerConfig flatten + 可选 setup；创建表单预填直接
/// 读 conf/<name>.yaml 并解析为 ContainerConfig。flutter 已退役。

/// 新建环境：统一创建入口，收完整 [`ContainerConfig`]（创建后启动并注册）。
///
/// 主 GUI 的容器启动（flavor 模板展开预填 / 裸镜像默认值）与单实例 GUI 的
/// 配置管理（`get_container_config`/`apply_container_config`）自此依赖同一
/// 结构体——core::models::ContainerConfig 是容器启动参数的唯一事实来源。
///
/// 镜像需已拉取（`create_with_config` 对缺失镜像报错，错误信息直接透传）。
#[tauri::command]
pub async fn env_new(
    podman: tauri::State<'_, PodmanState>,
    config: ContainerConfig,
) -> Result<(), String> {
    // 基本校验（表单兜底；名称 trim 防空白注册）
    if config.name.trim().is_empty() {
        return Err("环境名称不能为空".to_string());
    }
    if config.params.image.trim().is_empty() {
        return Err("镜像不能为空（需已拉取）".to_string());
    }
    let mut config = config;
    config.name = config.name.trim().to_string();
    let name = config.name.clone();

    // 按 gui/gpu 意图注入宿主透传（幂等；模板展开已注入过则跳过）——
    // 让创建表单里直接切换「GUI 透传 / GPU 透传」开关后创建即生效（不仅限于模板）。
    inject_passthrough(&mut config);

    // 失败即落盘：容器创建/启动链路多步易错，前端展示之外同时写
    // ~/.easytidy/logs/easytidy-gui.log（排障唯一持久出口）
    macro_rules! try_log {
        ($expr:expr, $ctx:expr) => {
            match $expr {
                Ok(v) => v,
                Err(e) => {
                    let msg = e.to_string();
                    let ctx: String = $ctx.to_string();
                    error!("env_new（{name}）{ctx}失败：{msg}");
                    return Err(format!("{ctx}失败：{msg}"));
                }
            }
        };
    }

    let p = try_log!(podman.get().await, "连接 podman");
    let bins = try_log!(easytidy_core::ContainerBins::resolve(), "定位容器内二进制");
    try_log!(
        p.create_with_config(&config.name, &config.params.image, &bins, &config)
            .await,
        "创建容器"
    );
    try_log!(p.start(&config.name).await, "启动容器");

    // 容器内准备：fontconfig（恒执行）+ useradd（仅 user_name 配置时）。
    // 失败不阻断创建（装包等可后续重建补齐；错误落日志），但给出提示。
    if let Err(e) = p.prepare_container(&config.name, &config.params).await {
        error!("容器内准备失败（{name}）：{e}");
    }

    let config_file = try_log!(ConfigFile::default_instance(), "解析容器配置目录");
    try_log!(config_file.register_container(config), "注册环境配置");

    // 生成桌面图标（辅助动作：失败不阻断创建，落日志即可——旧
    // create_container 路径的行为，统一入口后由此处承接）。
    // Exec 指向 CLI 垫片（`easytidy open --container <name>`），点击时由
    // CLI 决定保活/弹 GUI/友好报错。
    if let Err(e) = desktop::install_desktop_entry(
        &name,
        None,
        None,
        &crate::commands::passthrough::cli_path(),
    ) {
        warn!("生成桌面图标失败（忽略）：{e}");
    }

    podman.return_podman(p).await;
    info!("新环境 {name} 已创建并运行");

    // 便捷：新建容器后自动打开其 Worker GUI（进入即连；失败不阻断创建，落日志即可）
    if let Err(e) = open_container_window(name) {
        warn!("自动打开容器窗口失败（忽略）：{e}");
    }

    Ok(())
}

/// 删除环境：容器 + 注册配置 + 桌面图标 + socket 目录全清理。
///
/// 快照镜像为独立资产，删除时保留（可手动基于该镜像恢复/重建）。
#[tauri::command]
pub async fn env_rm(podman: tauri::State<'_, PodmanState>, name: String) -> Result<(), String> {
    let p = podman.get().await.map_err(|e| e.to_string())?;
    p.remove(&name, true).await.map_err(|e| e.to_string())?;
    podman.return_podman(p).await;

    // 清理注册配置（失败仅告警，不阻断删除）
    let config_file =
        ConfigFile::default_instance().map_err(|e| format!("解析容器配置目录失败：{}", e))?;
    if let Err(e) = config_file.unregister_container(&name) {
        warn!("注销环境 {} 配置失败（忽略）：{}", name, e);
    }
    // 清理桌面图标
    if let Err(e) = desktop::uninstall_desktop_entry(&name) {
        debug!("清理环境 {} 桌面图标失败（忽略）：{}", name, e);
    }
    // 清理 socket 目录（$XDG_RUNTIME_DIR/easytidy/<name>-<hash>，全代；尽力而为）
    let _ = easytidy_core::remove_socket_dirs(&name);

    info!("环境 {} 已删除（快照镜像保留为独立资产）", name);
    Ok(())
}

/// 快照：commit 当前容器文件系统层为快照镜像 `easytidy/snapshot/<snapshot_name>`。
///
/// `snapshot_name` = 用户输入的快照名（最终镜像全名 = `easytidy/snapshot/<snapshot_name>`）；
/// 缺省 = 可读默认名 `<容器名>-<YYYYmmdd-HHMM>`（core 统一兜底）。
/// `squash` = 镜像形态：`true`（默认）= `commit --squash` 单层；`false` = 普通
/// commit 保留分层历史。未传（None）时按默认 `true`。
/// 返回快照镜像名（前端展示/复用）。
#[tauri::command]
pub async fn env_snapshot(
    podman: tauri::State<'_, PodmanState>,
    name: String,
    snapshot_name: Option<String>,
    squash: Option<bool>,
) -> Result<String, String> {
    let p = podman.get().await.map_err(|e| e.to_string())?;
    let squash = squash.unwrap_or(true);
    let image_ref = p
        .snapshot(&name, snapshot_name.as_deref(), squash)
        .await
        .map_err(|e| e.to_string())?;
    podman.return_podman(p).await;

    info!("环境 {} 快照完成（squash={}）：{}", name, squash, image_ref);
    Ok(image_ref)
}

/// 运行环境（start）。
#[tauri::command]
pub async fn env_start(podman: tauri::State<'_, PodmanState>, name: String) -> Result<(), String> {
    let p = podman.get().await.map_err(|e| ferr("连接 podman", e))?;
    p.start(&name)
        .await
        .map_err(|e| ferr(&format!("启动容器 {name}"), e))?;
    podman.return_podman(p).await;
    Ok(())
}

/// 重建环境：两种形态
/// - 安全（默认，`quick=false`）：commit → 保留旧容器 → 同名重建并启动 →
///   确认新容器就绪后才删旧（失败自动回滚，环境不中断）。应用外部修改的
///   配置文件用；配置编辑走配置管理器（apply）。
/// - 快速（`quick=true`）：commit 数据 → 删旧 → 同名重建 → 启动（无回滚、
///   不确认就绪；失败时数据仍有 commit 镜像兜底）。
#[tauri::command]
pub async fn env_rebuild(
    podman: tauri::State<'_, PodmanState>,
    name: String,
    quick: Option<bool>,
) -> Result<(), String> {
    let config_file =
        ConfigFile::default_instance().map_err(|e| format!("解析容器配置目录失败：{e}"))?;
    let config = config_file
        .get_container(&name)
        .map_err(|e| format!("读取配置失败：{e}"))?
        .ok_or_else(|| format!("环境 {name} 不在注册表（先创建）"))?;

    let p = podman.get().await.map_err(|e| e.to_string())?;
    let bins = easytidy_core::ContainerBins::resolve().map_err(|e| e.to_string())?;
    if quick.unwrap_or(false) {
        p.rebuild_quick(&name, &config, &bins)
            .await
            .map_err(|e| format!("快速重建失败：{e}"))?;
    } else {
        p.rebuild(&name, &config, &bins)
            .await
            .map_err(|e| format!("重建失败：{e}"))?;
    }
    // 容器内准备（fontconfig + 可选 useradd）：失败不阻断重建（落日志）
    if let Err(e) = p.prepare_container(&name, &config.params).await {
        tracing::error!("容器内准备失败（{name}）：{e}");
    }
    podman.return_podman(p).await;
    info!("环境 {name} 已按注册配置重建并启动（quick={}）", quick.unwrap_or(false));
    Ok(())
}

/// 关闭环境（stop；环境保留，可随时恢复运行）。
#[tauri::command]
pub async fn env_stop(podman: tauri::State<'_, PodmanState>, name: String) -> Result<(), String> {
    let p = podman.get().await.map_err(|e| ferr("连接 podman", e))?;
    p.stop(&name)
        .await
        .map_err(|e| ferr(&format!("停止容器 {name}"), e))?;
    podman.return_podman(p).await;
    Ok(())
}

/// 打开容器专属窗口（从 Master GUI 双击容器）
#[tauri::command]
pub fn open_container_window(name: String) -> Result<(), String> {
    use std::process::Command;

    // 用当前可执行文件自身启动 Worker 实例（dev/prod 通用，不依赖 PATH）。
    // dev 坑：本进程 exe 被 cargo 重建替换后 current_exe() 返回带
    // " (deleted)" 后缀的路径（文件已不存在）→ 剥离后缀回落到新构建
    // （target/debug 同路径已重新落盘）
    let exe = std::env::current_exe().map_err(|e| format!("解析当前可执行文件失败：{}", e))?;
    let exe = if let Some(stripped) = exe.to_string_lossy().strip_suffix(" (deleted)") {
        std::path::PathBuf::from(stripped)
    } else {
        exe
    };
    let _child = Command::new(exe)
        .arg("--container")
        .arg(&name)
        .spawn()
        .map_err(|e| format!("启动容器窗口失败：{}", e))?;

    info!("已启动容器窗口：{}", name);
    Ok(())
}

/// 关闭容器
#[tauri::command]
pub async fn container_shutdown(
    session: tauri::State<'_, Option<GuiSession>>,
) -> Result<(), String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;

    send_json_request(
        sess,
        "lifecycle.shutdown".to_string(),
        serde_json::to_value(LifecycleShutdown).map_err(|e| e.to_string())?,
    )
    .await
    .map_err(|e| e.to_string())?;

    Ok(())
}
