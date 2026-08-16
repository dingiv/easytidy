//! 中心化模式命令：容器生命周期 + 环境（env）语义 + flavor。

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use tracing::{debug, error, info, warn};

use easytidy_core::configfile::ConfigFile;
use easytidy_core::desktop;
use easytidy_core::flavor::Flavor;
use easytidy_core::models::{ContainerConfig, ContainerSummary};

use crate::commands::socket::send_json_request;
use crate::state::{GuiSession, PodmanState};
use easytidy_protocol::ops::LifecycleShutdown;

// ============================================================================
// 中心化模式命令（容器生命周期管理）
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

/// 创建新容器
#[tauri::command]
pub async fn create_container(
    podman: tauri::State<'_, PodmanState>,
    image: String,
    name: String,
) -> Result<String, String> {
    let p = podman.get().await.map_err(|e| ferr("连接 podman", e))?;

    // 获取 server 二进制路径
    let server_bin =
        easytidy_core::server_binary_path().map_err(|e| ferr("定位 server 二进制", e))?;

    // 容器配置（网络默认 Host 模式，产品语义；distrobox 同款）
    let container_config = ContainerConfig {
        name: name.clone(),
        image: image.clone(),
        entry: None,
        silent_boot: false,
        persistent: true,
        ..Default::default()
    };

    // 创建容器（走新入口，应用完整配置）
    let id = p
        .create_with_config(&name, &image, &server_bin, &container_config)
        .await
        .map_err(|e| ferr(&format!("创建容器 {name}"), e))?;

    // 注册到配置文件
    let config_path = ConfigFile::default_path().map_err(|e| ferr("解析配置路径", e))?;
    let config_file = ConfigFile::with_path(config_path);

    config_file
        .register_container(container_config)
        .map_err(|e| ferr("注册容器配置", e))?;

    // 生成桌面图标
    desktop::install_desktop_entry(&name, None, None).map_err(|e| ferr("生成桌面图标", e))?;

    podman.return_podman(p).await;
    Ok(id)
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

/// 列出可用 flavor 模板（$XDG_CONFIG_HOME/easytidy/flavors/*.toml）。
#[tauri::command]
pub fn flavor_list() -> Result<Vec<String>, String> {
    Flavor::list().map_err(|e| e.to_string())
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
// flavor 管理命令（GUI flavor 板块：启动配置模板）
// ============================================================================

/// 列出全部 flavor（含完整配置：镜像/gui/setup/entry/mounts/网络）。
/// 首次调用补齐内置预设（chrome/firefox/code 快速 GUI 拉起模板）。
#[tauri::command]
pub fn flavor_list_detailed() -> Result<Vec<Flavor>, String> {
    easytidy_core::flavor::ensure_presets();
    Flavor::list_detailed().map_err(|e| e.to_string())
}

/// 保存 flavor（新建或覆盖；原子写 ~/.easytidy/flavors/<name>.toml）。
#[tauri::command]
pub fn flavor_save(flavor: Flavor) -> Result<(), String> {
    flavor.save().map_err(|e| e.to_string())
}

/// 删除 flavor。
#[tauri::command]
pub fn flavor_delete(name: String) -> Result<(), String> {
    Flavor::delete(&name).map_err(|e| e.to_string())
}

/// 列出所有环境（managed 容器 + configfile 注册表合并）。
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
    for c in containers {
        if !c.managed {
            continue; // 仅展示 easytidy 管理的环境
        }
        covered.insert(c.name.clone());
        views.push(EnvView {
            name: c.name,
            image: c.image,
            status: c.status,
            managed: true,
        });
    }
    for cfg in registered {
        if covered.contains(&cfg.name) {
            continue;
        }
        views.push(EnvView {
            name: cfg.name,
            image: cfg.image,
            status: "missing".to_string(),
            managed: true,
        });
    }
    views.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(views)
}

/// 新建环境：flavor 模板展开创建，或指定镜像创建；创建后启动并注册。
///
/// 镜像需已拉取（`create_with_config` 对缺失镜像报错，错误信息直接透传）。
#[tauri::command]
pub async fn env_new(
    podman: tauri::State<'_, PodmanState>,
    name: String,
    flavor: Option<String>,
    image: Option<String>,
) -> Result<(), String> {
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

    // 配置来源：flavor 模板展开（继承挂载/网络/用户映射/GUI 透传）或直接镜像
    let config = match flavor {
        Some(f) => {
            let flavor = try_log!(Flavor::load(&f), format!("加载 flavor {f}"));
            try_log!(flavor.build_config(&name), "展开 flavor 配置")
        }
        None => {
            let Some(image) = image else {
                return Err("新建环境需要提供模板（flavor）或镜像（image）".to_string());
            };
            ContainerConfig {
                name: name.clone(),
                image: image.clone(),
                ..Default::default()
            }
        }
    };

    let server_bin = try_log!(easytidy_core::server_binary_path(), "定位 server 二进制");
    try_log!(
        p.create_with_config(&name, &config.image, &server_bin, &config)
            .await,
        "创建容器"
    );
    try_log!(p.start(&name).await, "启动容器");

    let config_path = try_log!(ConfigFile::default_path(), "解析配置路径");
    let config_file = ConfigFile::with_path(config_path);
    try_log!(config_file.register_container(config), "注册环境配置");

    podman.return_podman(p).await;
    info!("新环境 {name} 已创建并运行");
    Ok(())
}

/// 删除环境：容器 + 注册配置 + 桌面图标 + socket 目录全清理。
///
/// 快照镜像为独立资产，删除时保留（可被 fork 复用）。
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
    // 清理 socket 目录（$XDG_RUNTIME_DIR/easytidy/<name>）
    if let Ok(sock) = easytidy_core::host_socket_path(&name) {
        if let Some(dir) = sock.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

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

/// fork：从快照镜像派生新环境，继承源环境的全部配置（仅镜像换成快照）。
#[tauri::command]
pub async fn env_fork(
    podman: tauri::State<'_, PodmanState>,
    name: String,
    snapshot: String,
    new_name: String,
) -> Result<(), String> {
    // 读源环境配置
    let config_path = ConfigFile::default_path().map_err(|e| format!("解析配置路径失败：{}", e))?;
    let config_file = ConfigFile::with_path(config_path);
    let mut config = config_file
        .get_container(&name)
        .map_err(|e| format!("读取源环境配置失败：{}", e))?
        .ok_or_else(|| format!("源环境 {} 不在注册表中（请先创建该环境）", name))?;

    // 快照镜像：easytidy/snapshot/<name>-<snapshot>
    let image_ref = format!("easytidy/snapshot/{name}-{snapshot}");
    config.name = new_name.clone();
    config.image = image_ref.clone();

    let p = podman.get().await.map_err(|e| e.to_string())?;
    let server_bin = easytidy_core::server_binary_path().map_err(|e| e.to_string())?;
    p.create_with_config(&new_name, &image_ref, &server_bin, &config)
        .await
        .map_err(|e| e.to_string())?;
    p.start(&new_name).await.map_err(|e| e.to_string())?;
    podman.return_podman(p).await;

    config_file
        .register_container(config)
        .map_err(|e| format!("注册新环境配置失败：{}", e))?;

    info!("新环境 {} 已从快照 {} 派生并运行", new_name, snapshot);
    Ok(())
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

/// 构建 server 二进制
#[tauri::command]
pub async fn build_server() -> Result<String, String> {
    use std::process::Command;

    // 调用构建脚本
    let output = Command::new("/bin/bash")
        .arg("/home/jiugui5209/Documents/codes/easy-tidy/easytidy/scripts/build-server.sh")
        .output()
        .map_err(|e| format!("执行构建脚本失败：{}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("构建失败：{}", stderr));
    }

    Ok("构建成功".to_string())
}

/// 打开容器专属窗口（从中心化 GUI 双击容器）
#[tauri::command]
pub fn open_container_window(name: String) -> Result<(), String> {
    use std::process::Command;

    // 用当前可执行文件自身启动 per-container 实例（dev/prod 通用，不依赖 PATH）
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
