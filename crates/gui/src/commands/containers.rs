//! Master GUI 命令：容器生命周期 + 环境（env）语义。
//!
//! 模板管理(fui 侧)由 conf YAML 取代 flavor TOML——见 `commands::config::*`。
//! CLI 仍使用 core::flavor；GUI 模板 tab 直接读写 `~/.easytidy/conf`。

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use tracing::{debug, error, info, warn};

use easytidy_core::configfile::ConfigFile;
use easytidy_core::desktop;
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
    let config_path = ConfigFile::default_path().map_err(|e| ferr("解析配置路径", e))?;
    let config_file = ConfigFile::with_path(config_path);
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
    let config_path = ConfigFile::default_path().map_err(|e| format!("解析配置路径失败：{}", e))?;
    let config_file = ConfigFile::with_path(config_path);
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
    let server_bin = try_log!(easytidy_core::server_binary_path(), "定位 server 二进制");
    try_log!(
        p.create_with_config(&config.name, &config.params.image, &server_bin, &config)
            .await,
        "创建容器"
    );
    try_log!(p.start(&config.name).await, "启动容器");

    // 容器内准备：fontconfig（恒执行）+ useradd（仅 user_name 配置时）。
    // 失败不阻断创建（装包等可后续重建补齐；错误落日志），但给出提示。
    if let Err(e) = p.prepare_container(&config.name, &config.params).await {
        error!("容器内准备失败（{name}）：{e}");
    }

    let config_path = try_log!(ConfigFile::default_path(), "解析配置路径");
    let config_file = ConfigFile::with_path(config_path);
    try_log!(config_file.register_container(config), "注册环境配置");

    // 生成桌面图标（辅助动作：失败不阻断创建，落日志即可——旧
    // create_container 路径的行为，统一入口后由此处承接）
    if let Err(e) = desktop::install_desktop_entry(&name, None, None) {
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

/// 模板派生清单：每个 conf 模板有哪些容器以其为血缘（FlavorsPanel 展示
/// 「派生容器」+ 批量同步入口）。自由创建的容器（无血缘）不出现。
///
/// 数据源：`configfile` 注册表里每个容器的 `flavor` 字段——容器从 conf 模板创建
/// 时该字段存模板名（与旧 TOML flavor 共用同一血缘语义）。
#[tauri::command]
pub fn template_lineage() -> Result<std::collections::HashMap<String, Vec<String>>, String> {
    let config_path = ConfigFile::default_path().map_err(|e| format!("解析配置路径失败：{e}"))?;
    let config_file = ConfigFile::with_path(config_path);
    let containers = config_file
        .list_containers()
        .map_err(|e| format!("读取容器配置失败：{e}"))?;
    let mut lineage = std::collections::HashMap::new();
    for cfg in containers {
        if let Some(flavor) = cfg.flavor {
            lineage.entry(flavor).or_insert_with(Vec::new).push(cfg.name);
        }
    }
    Ok(lineage)
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
    let config_path = ConfigFile::default_path().map_err(|e| format!("解析配置路径失败：{}", e))?;
    let config_file = ConfigFile::with_path(config_path);
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

/// 快照：commit 当前容器文件系统层为快照镜像 `easytidy/snapshot/<name>-<tag>`。
///
/// 默认标签 = 时间戳；返回快照镜像名（前端展示/复用）。
#[tauri::command]
pub async fn env_snapshot(
    podman: tauri::State<'_, PodmanState>,
    name: String,
    tag: Option<String>,
) -> Result<String, String> {
    let tag = tag.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs().to_string())
            .unwrap_or_else(|_| "now".to_string())
    });

    let p = podman.get().await.map_err(|e| e.to_string())?;
    let image_ref = p.snapshot(&name, &tag).await.map_err(|e| e.to_string())?;
    podman.return_podman(p).await;

    info!("环境 {} 快照完成：{}", name, image_ref);
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

/// 重建环境：按注册表（config.toml）当前配置 commit → 删旧 → 同名重建
/// → 启动。应用外部修改的配置文件 / 修正容器漂移状态用；配置编辑走
/// 配置管理器（apply），模板对齐走「从模板同步」。
#[tauri::command]
pub async fn env_rebuild(podman: tauri::State<'_, PodmanState>, name: String) -> Result<(), String> {
    let config_path = ConfigFile::default_path().map_err(|e| format!("解析配置路径失败：{e}"))?;
    let config_file = ConfigFile::with_path(config_path);
    let config = config_file
        .get_container(&name)
        .map_err(|e| format!("读取配置失败：{e}"))?
        .ok_or_else(|| format!("环境 {name} 不在注册表（先创建）"))?;

    let p = podman.get().await.map_err(|e| e.to_string())?;
    let server_bin = easytidy_core::server_binary_path().map_err(|e| e.to_string())?;
    p.rebuild(&name, &config, &server_bin)
        .await
        .map_err(|e| format!("重建失败：{e}"))?;
    // 容器内准备（fontconfig + 可选 useradd）：失败不阻断重建（落日志）
    if let Err(e) = p.prepare_container(&name, &config.params).await {
        tracing::error!("容器内准备失败（{name}）：{e}");
    }
    podman.return_podman(p).await;
    info!("环境 {name} 已按注册配置重建并启动");
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

    // 用当前可执行文件自身启动 Worker 实例（dev/prod 通用，不依赖 PATH）
    let exe = std::env::current_exe().map_err(|e| format!("解析当前可执行文件失败：{}", e))?;
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
