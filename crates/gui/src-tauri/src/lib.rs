//! easytidy GUI Tauri 命令实现。
//!
//! 实现两种模式：
//! - 中心化模式：容器生命周期管理（调用 easytidy-core）
//! - 单容器模式：与容器内 server 通信（经 socket 协议）
//!
//! 托管状态：
//! - AppMode（启动时解析的命令行参数）
//! - Podman（中心化模式，延迟连接）
//! - GuiSession（单容器模式，socket 会话）

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use anyhow::{Context, Result};
use futures::stream::SplitSink;
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::net::UnixStream;
use tokio_util::codec::Framed;
use tracing::{debug, error, info, warn};

use easytidy_core::configfile::ConfigFile;
use easytidy_core::desktop;
use easytidy_core::flavor::Flavor;
use easytidy_core::models::{ContainerConfig, ContainerSummary};
use easytidy_core::podman::Podman;
use easytidy_protocol::{
    Frame, FrameCodec, Message, MsgKind, Handshake, HandshakeAck, PROTOCOL_VERSION,
};
use easytidy_protocol::ops::{
    PtyOpen, PtyOpenResp, PtyResize, PtyClose, PtyExited,
    FsList, FsListResp, FsRead, FsReadResp, FsWrite,
    AppsList, AppsListResp,
    CfgGet, CfgGetResp, CfgSet,
    LifecycleShutdown,
};

// ============================================================================
// 应用模式与托管状态
// ============================================================================

/// GUI 应用模式（main.rs 传入）
#[derive(Debug, Clone, PartialEq)]
pub enum AppMode {
    /// 中心化管理模式（管理所有容器）
    Centralized,
    /// 单容器管理模式
    Container { name: String },
}

/// 单条 PTY 会话的写侧（该 PTY 专用连接的 sink）
type PtySink = Arc<tokio::sync::Mutex<SplitSink<Framed<UnixStream, FrameCodec>, Frame>>>;

/// 单容器 GUI 会话（托管在 Tauri State 中）
pub struct GuiSession {
    /// 容器名
    pub container_name: String,
    /// Socket 会话（使用 tokio Mutex 因为需要 async）
    pub socket: tokio::sync::Mutex<Option<Framed<UnixStream, FrameCodec>>>,
    /// 下一个消息 ID
    pub next_msg_id: AtomicU64,
    /// 活动 PTY 流（stream_id -> 该 PTY 专用连接的写侧）
    pub active_ptys: Arc<tokio::sync::Mutex<HashMap<u32, PtySink>>>,
}

/// PTY 事件（通过 Channel 发送给前端）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PtyEvent {
    /// 事件类型："data" / "exited" / "cwdChanged"
    pub kind: String,
    /// 数据（kind="data" 时）
    pub data: Option<Vec<u8>>,
    /// 退出码（kind="exited" 时）
    pub code: Option<i32>,
    /// 工作目录（kind="cwdChanged" 时；server 主动推送的 TTY 事件）
    pub cwd: Option<String>,
}

/// Podman 客户端（延迟连接）
pub struct PodmanState(Arc<tokio::sync::Mutex<Option<Podman>>>);

impl Default for PodmanState {
    fn default() -> Self {
        Self::new()
    }
}

impl PodmanState {
    pub fn new() -> Self {
        Self(Arc::new(tokio::sync::Mutex::new(None)))
    }

    /// 获取或创建 Podman 连接
    async fn get(&self) -> Result<Podman> {
        let mut guard = self.0.lock().await;
        if let Some(podman) = guard.take() {
            drop(guard); // 立即释放锁
            Ok(podman)
        } else {
            drop(guard); // 在 await 前释放锁
            let podman = Podman::connect().await.context("连接 podman 失败")?;
            Ok(podman)
        }
    }

    /// 归还 Podman 连接（保持复用）
    async fn return_podman(&self, podman: Podman) {
        let mut guard = self.0.lock().await;
        *guard = Some(podman);
    }
}

// ============================================================================
// NVIDIA GPU 规避措施
// ============================================================================

/// 应用 NVIDIA DMABUF 渲染器规避措施
///
/// 根据 docs/10-m0-spike-report.md 的结论：
/// - NVIDIA 宿主需设置 WEBKIT_DISABLE_DMABUF_RENDERER=1
/// - 参考 clash-verge-rev 的 workarounds.rs 实现
fn apply_nvidia_workaround() {
    // 如果环境变量已设置，跳过检测
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_some() {
        return;
    }

    if has_nvidia_gpu() {
        unsafe {
            std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
        }
        println!("Detected NVIDIA GPU, applied WEBKIT_DISABLE_DMABUF_RENDERER=1 workaround");
    }
}

/// 检测是否存在 NVIDIA GPU
///
/// 检测路径（参考 clash-verge-rev）：
/// - /proc/driver/nvidia/version
/// - /sys/module/nvidia*
/// - /sys/class/drm/card*/device/vendor == 0x10de
fn has_nvidia_gpu() -> bool {
    // 检查 NVIDIA 驱动文件
    if Path::new("/proc/driver/nvidia/version").exists()
        || Path::new("/sys/module/nvidia").exists()
        || Path::new("/sys/module/nvidia_drm").exists()
    {
        return true;
    }

    // 检查 DRM 设备供应商
    let Ok(entries) = fs::read_dir("/sys/class/drm") else {
        return false;
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("card") || name.contains('-') {
            continue;
        }

        let vendor_path = entry.path().join("device/vendor");
        let Ok(vendor) = fs::read_to_string(vendor_path) else {
            continue;
        };
        if vendor.trim().eq_ignore_ascii_case("0x10de") {
            return true;
        }
    }

    false
}

// ============================================================================
// 通用命令
// ============================================================================

/// 切换 webview devtools（调试用；前端按钮触发）
#[tauri::command]
fn toggle_devtools(webview: tauri::WebviewWindow) {
    if webview.is_devtools_open() {
        webview.close_devtools();
    } else {
        webview.open_devtools();
    }
}

/// 应用模式响应（结构体，避开 serde 单元变体→裸字符串的歧义）。
/// 前端按 result.mode 判断："centralized" | "per_container"。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppModeResponse {
    pub mode: String,
    pub name: Option<String>,
}

/// 获取应用模式（前端调用）
#[tauri::command]
fn get_app_mode(mode: tauri::State<'_, AppMode>) -> Result<AppModeResponse, String> {
    Ok(match mode.inner() {
        AppMode::Centralized => AppModeResponse {
            mode: "centralized".to_string(),
            name: None,
        },
        AppMode::Container { name } => AppModeResponse {
            mode: "per_container".to_string(),
            name: Some(name.clone()),
        },
    })
}

// ============================================================================
// 中心化模式命令（容器生命周期管理）
// ============================================================================

/// 列出所有容器
#[tauri::command]
async fn list_containers(
    podman: tauri::State<'_, PodmanState>,
) -> Result<Vec<ContainerSummary>, String> {
    let p = podman.get().await.map_err(|e| e.to_string())?;
    let result = p.list_containers().await.map_err(|e| e.to_string())?;
    podman.return_podman(p).await;
    Ok(result)
}

/// 创建新容器
#[tauri::command]
async fn create_container(
    podman: tauri::State<'_, PodmanState>,
    image: String,
    name: String,
) -> Result<String, String> {
    let p = podman.get().await.map_err(|e| e.to_string())?;

    // 获取 server 二进制路径
    let server_bin = easytidy_core::server_binary_path()
        .map_err(|e| e.to_string())?;

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
    let id = p.create_with_config(&name, &image, &server_bin, &container_config)
        .await
        .map_err(|e| e.to_string())?;

    // 注册到配置文件
    let config_path = ConfigFile::default_path()
        .map_err(|e| format!("解析配置路径失败：{}", e))?;
    let config_file = ConfigFile::with_path(config_path);

    config_file.register_container(container_config)
        .map_err(|e| format!("注册容器配置失败：{}", e))?;

    // 生成桌面图标
    desktop::install_desktop_entry(&name, None, None)
        .map_err(|e| format!("生成桌面图标失败：{}", e))?;

    podman.return_podman(p).await;
    Ok(id)
}

/// 启动容器
#[tauri::command]
async fn start_container(
    podman: tauri::State<'_, PodmanState>,
    name: String,
) -> Result<(), String> {
    let p = podman.get().await.map_err(|e| e.to_string())?;
    p.start(&name).await.map_err(|e| e.to_string())?;
    podman.return_podman(p).await;
    Ok(())
}

/// 停止容器
#[tauri::command]
async fn stop_container(
    podman: tauri::State<'_, PodmanState>,
    name: String,
) -> Result<(), String> {
    let p = podman.get().await.map_err(|e| e.to_string())?;
    p.stop(&name).await.map_err(|e| e.to_string())?;
    podman.return_podman(p).await;
    Ok(())
}

/// 重启容器
#[tauri::command]
async fn restart_container(
    podman: tauri::State<'_, PodmanState>,
    name: String,
) -> Result<(), String> {
    let p = podman.get().await.map_err(|e| e.to_string())?;
    p.restart(&name).await.map_err(|e| e.to_string())?;
    podman.return_podman(p).await;
    Ok(())
}

/// 删除容器
#[tauri::command]
async fn remove_container(
    podman: tauri::State<'_, PodmanState>,
    name: String,
    force: bool,
) -> Result<(), String> {
    let p = podman.get().await.map_err(|e| e.to_string())?;
    p.remove(&name, force).await.map_err(|e| e.to_string())?;

    // 从配置文件注销
    let config_path = ConfigFile::default_path()
        .map_err(|e| format!("解析配置路径失败：{}", e))?;
    let config_file = ConfigFile::with_path(config_path);
    config_file.unregister_container(&name)
        .map_err(|e| format!("注销容器配置失败：{}", e))?;

    // 卸载桌面图标
    let _ = desktop::uninstall_desktop_entry(&name); // 忽略错误

    podman.return_podman(p).await;
    Ok(())
}

/// 检查容器详情
#[tauri::command]
async fn inspect_container(
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
fn flavor_list() -> Result<Vec<String>, String> {
    Flavor::list().map_err(|e| e.to_string())
}

/// 列出所有环境（managed 容器 + configfile 注册表合并）。
#[tauri::command]
async fn env_list(
    podman: tauri::State<'_, PodmanState>,
) -> Result<Vec<EnvView>, String> {
    let p = podman.get().await.map_err(|e| e.to_string())?;
    let containers = p.list_containers().await.map_err(|e| e.to_string())?;
    podman.return_podman(p).await;

    // configfile 注册表：podman 中不存在的配置 → "missing"（仅配置保留）
    let config_path = ConfigFile::default_path()
        .map_err(|e| format!("解析配置路径失败：{}", e))?;
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
async fn env_new(
    podman: tauri::State<'_, PodmanState>,
    name: String,
    flavor: Option<String>,
    image: Option<String>,
) -> Result<(), String> {
    let p = podman.get().await.map_err(|e| e.to_string())?;

    // 配置来源：flavor 模板展开（继承挂载/网络/用户映射/GUI 透传）或直接镜像
    let config = match flavor {
        Some(f) => {
            let flavor = Flavor::load(&f).map_err(|e| e.to_string())?;
            flavor.build_config(&name).map_err(|e| e.to_string())?
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

    let server_bin = easytidy_core::server_binary_path().map_err(|e| e.to_string())?;
    p.create_with_config(&name, &config.image, &server_bin, &config)
        .await
        .map_err(|e| e.to_string())?;
    p.start(&name).await.map_err(|e| e.to_string())?;

    let config_path = ConfigFile::default_path()
        .map_err(|e| format!("解析配置路径失败：{}", e))?;
    let config_file = ConfigFile::with_path(config_path);
    config_file
        .register_container(config)
        .map_err(|e| format!("注册环境配置失败：{}", e))?;

    podman.return_podman(p).await;
    info!("新环境 {} 已创建并运行", name);
    Ok(())
}

/// 删除环境：容器 + 注册配置 + 桌面图标 + socket 目录全清理。
///
/// 快照镜像为独立资产，删除时保留（可被 fork 复用）。
#[tauri::command]
async fn env_rm(
    podman: tauri::State<'_, PodmanState>,
    name: String,
) -> Result<(), String> {
    let p = podman.get().await.map_err(|e| e.to_string())?;
    p.remove(&name, true).await.map_err(|e| e.to_string())?;
    podman.return_podman(p).await;

    // 清理注册配置（失败仅告警，不阻断删除）
    let config_path = ConfigFile::default_path()
        .map_err(|e| format!("解析配置路径失败：{}", e))?;
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
async fn env_snapshot(
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
async fn env_fork(
    podman: tauri::State<'_, PodmanState>,
    name: String,
    snapshot: String,
    new_name: String,
) -> Result<(), String> {
    // 读源环境配置
    let config_path = ConfigFile::default_path()
        .map_err(|e| format!("解析配置路径失败：{}", e))?;
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
async fn env_start(
    podman: tauri::State<'_, PodmanState>,
    name: String,
) -> Result<(), String> {
    let p = podman.get().await.map_err(|e| e.to_string())?;
    p.start(&name).await.map_err(|e| e.to_string())?;
    podman.return_podman(p).await;
    Ok(())
}

/// 关闭环境（stop；环境保留，可随时恢复运行）。
#[tauri::command]
async fn env_stop(
    podman: tauri::State<'_, PodmanState>,
    name: String,
) -> Result<(), String> {
    let p = podman.get().await.map_err(|e| e.to_string())?;
    p.stop(&name).await.map_err(|e| e.to_string())?;
    podman.return_podman(p).await;
    Ok(())
}

// ============================================================================
// 配置管理器命令（M4 前置：mount 管理 + 网络映射管理；改配置 = 重建容器）
// ============================================================================

/// 获取容器配置（configfile 期望配置 + podman inspect 当前生效状态 + 宿主用户）。
///
/// 返回（前端契约）：
/// ```json
/// { "config": { "name","image","entry","silent_boot","persistent",
///                "mounts":[{"host_path","container_path","read_only"}],
///                "network":{"mode":"host"|"mapped","ports":[...]},
///                "env":["K=V"], "user_home":true },
///   "effective": { "mounts":[...], "network":{...}, "env":["K=V"],
///                  "user":"0:0"|null, "userns_mode":"keep-id"|null },
///   "host_user": { "name","uid","gid","home" } | null }
/// ```
/// `effective` 为 `null` 表示容器尚未创建（仅 configfile 有记录）或 podman 不可达；
/// `host_user` 为 `null` 表示宿主用户探测失败（容器降级 root 运行，UI 需展示）。
#[tauri::command]
async fn get_container_config(name: String) -> Result<serde_json::Value, String> {
    // configfile 期望配置
    let config_path = ConfigFile::default_path()
        .map_err(|e| format!("解析配置路径失败：{}", e))?;
    let config_file = ConfigFile::with_path(config_path);
    let config = config_file
        .get_container(&name)
        .map_err(|e| format!("读取容器配置失败：{}", e))?
        .ok_or_else(|| format!("容器配置不存在：{}", name))?;

    // podman inspect 当前生效状态（容器未创建 / 连接失败时为 null）
    let effective = match Podman::connect().await {
        Ok(p) => match p.inspect_config(&name).await {
            Ok(view) => serde_json::to_value(view).map_err(|e| e.to_string())?,
            Err(e) => {
                warn!("获取容器 {} 生效配置失败：{}", name, e);
                serde_json::Value::Null
            }
        },
        Err(e) => {
            warn!("连接 podman 失败：{}", e);
            serde_json::Value::Null
        }
    };

    Ok(serde_json::json!({
        "config": serde_json::to_value(config).map_err(|e| e.to_string())?,
        "effective": effective,
        // 宿主用户（uid 映射语义对照表数据源；null = 探测失败，容器降级 root）
        "host_user": serde_json::to_value(easytidy_core::userenv::host_user()).map_err(|e| e.to_string())?,
    }))
}

/// 应用容器配置（mounts / 网络映射变更 → 重建容器，重启后生效）。
///
/// `config` 即 `get_container_config` 返回的 `config` 对象（含 mounts/network）。
/// 返回新容器 ID。
#[tauri::command]
async fn apply_container_config(
    name: String,
    config: serde_json::Value,
) -> Result<String, String> {
    let mut container_config: ContainerConfig = serde_json::from_value(config)
        .map_err(|e| format!("解析容器配置失败：{}", e))?;
    container_config.name = name.clone();

    // 直接调用 core（不依赖 GuiSession）：commit → 删旧 → 同名重建（新配置）→ 启动
    let podman = Podman::connect().await.map_err(|e| e.to_string())?;
    let new_id = podman
        .rebuild(&name, &container_config)
        .await
        .map_err(|e| e.to_string())?;

    // 更新 configfile（与重建后的容器保持一致）
    let config_path = ConfigFile::default_path()
        .map_err(|e| format!("解析配置路径失败：{}", e))?;
    let config_file = ConfigFile::with_path(config_path);
    config_file
        .register_container(container_config)
        .map_err(|e| format!("更新容器配置失败：{}", e))?;

    Ok(new_id)
}

/// 构建 server 二进制
#[tauri::command]
async fn build_server() -> Result<String, String> {
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
fn open_container_window(name: String) -> Result<(), String> {
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

// ============================================================================
// 单容器模式命令（socket 通信）
// ============================================================================

/// 连接到容器 server socket（延迟连接，仅在首次单容器命令时调用）
async fn connect_to_container(container_name: &str) -> Result<Framed<UnixStream, FrameCodec>> {
    // 确保容器运行中
    let podman = Podman::connect().await.context("连接 podman 失败")?;
    let containers = podman.list_containers().await.context("获取容器列表失败")?;
    let container_info = containers
        .iter()
        .find(|c| c.name == container_name)
        .context(format!("容器不存在：{}", container_name))?;

    if container_info.status != "running" {
        return Err(anyhow::anyhow!("容器未运行：{}", container_name));
    }

    // 连接到 socket
    let socket_path = easytidy_core::host_socket_path(container_name)?;
    info!("连接到 socket：{}", socket_path.display());

    let stream = UnixStream::connect(&socket_path)
        .await
        .context(format!("连接 socket 失败（容器可能未就绪）：{}",
            socket_path.display()))?;

    // 握手
    let codec = FrameCodec::new();
    let mut framed = Framed::new(stream, codec);

    let handshake = Handshake {
        v: PROTOCOL_VERSION,
        client: "easytidy-gui".to_string(),
        wants: vec!["pty".to_string(), "fs".to_string(), "apps".to_string(),
                    "passthrough".to_string(), "config".to_string(), "lifecycle".to_string()],
    };
    let handshake_msg = Message {
        id: 1,
        kind: MsgKind::Req,
        op: "hello".to_string(),
        payload: serde_json::to_value(handshake)?,
        err: None,
    };
    framed.send(Frame::Json(handshake_msg)).await
        .context("发送握手失败")?;

    let ack_frame = framed.next().await
        .context("接收握手确认失败")?
        .context("握手确认帧为空")?;

    let ack_msg = match ack_frame {
        Frame::Json(msg) => msg,
        Frame::Raw { .. } => return Err(anyhow::anyhow!("握手响应应为 JSON 帧")),
    };

    if ack_msg.kind != MsgKind::Resp || ack_msg.op != "hello" {
        return Err(anyhow::anyhow!("握手响应格式错误"));
    }

    let ack: HandshakeAck = serde_json::from_value(ack_msg.payload)
        .context("解析握手确认失败")?;

    debug!("握手成功：server={}, v={}", ack.server, ack.v);

    if ack.v != PROTOCOL_VERSION {
        return Err(anyhow::anyhow!("协议版本不匹配：客户端={}，服务端={}",
            PROTOCOL_VERSION, ack.v));
    }

    Ok(framed)
}

/// 确保会话已连接（延迟连接）
async fn ensure_session_connected(session: &GuiSession) -> Result<()> {
    let mut socket_guard = session.socket.lock().await;
    if socket_guard.is_some() {
        return Ok(()); // 已连接
    }

    // 首次连接
    let container_name = session.container_name.clone();
    let framed = connect_to_container(&container_name).await?;
    *socket_guard = Some(framed);

    Ok(())
}

/// 发送 JSON 请求并接收响应
/// 在共享 session socket 上发一次请求并等待响应。
async fn try_send_json_request(
    session: &GuiSession,
    msg: &Message,
) -> Result<Message> {
    let mut socket_guard = session.socket.lock().await;
    let socket = socket_guard.as_mut().context("Socket 未初始化")?;

    socket.send(Frame::Json(msg.clone())).await
        .context("发送请求失败")?;

    // 接收响应
    let response = socket.next().await
        .context("接收响应失败")?
        .context("响应帧为空")?;

    match response {
        Frame::Json(resp_msg) => {
            if let Some(ref err) = resp_msg.err {
                Err(anyhow::anyhow!("服务器错误：{} - {}", err.code, err.message))
            } else {
                Ok(resp_msg)
            }
        }
        Frame::Raw { .. } => Err(anyhow::anyhow!("响应应为 JSON 帧")),
    }
}

/// 发送 JSON 请求并接收响应。
///
/// server 对无 PTY 的连接有 30s 空闲超时；共享 socket 空闲超时后可能已被
/// server 断开 —— 失败时丢弃旧连接、重连一次重试（幂等请求场景足够）。
async fn send_json_request(
    session: &GuiSession,
    op: String,
    payload: serde_json::Value,
) -> Result<Message> {
    ensure_session_connected(session).await?;

    let msg = Message {
        id: session.next_msg_id.fetch_add(1, Ordering::SeqCst),
        kind: MsgKind::Req,
        op,
        payload,
        err: None,
    };

    match try_send_json_request(session, &msg).await {
        Ok(resp) => Ok(resp),
        Err(e) => {
            warn!("共享 socket 请求失败，重连重试：{}", e);
            // 丢弃旧连接（可能已被 server 空闲超时断开）
            session.socket.lock().await.take();
            ensure_session_connected(session).await?;
            try_send_json_request(session, &msg).await
        }
    }
}

/// 打开 PTY 会话
///
/// 每条 PTY 会话使用**一条专用连接**（open + resp + 读写全走它，与 cli cmd_run 同构）。
/// server 的 PTY 输出只流向打开它的那条连接；共享 session socket 仅用于 fs/apps/config。
#[tauri::command]
async fn pty_open(
    session: tauri::State<'_, Option<GuiSession>>,
    on_event: tauri::ipc::Channel<PtyEvent>,
    cmd: Option<String>,
    cols: u16,
    rows: u16,
    // 以 root 运行（root 终端；Tauri 参数名 camelCase → 前端传 asRoot）
    as_root: bool,
) -> Result<u32, String> {
    let sess = session.inner().as_ref().ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    let container_name = sess.container_name.clone();

    // 专用连接：连接 + 握手（与 cli cmd_run 同构）
    let mut framed = connect_to_container(&container_name)
        .await
        .map_err(|e| e.to_string())?;

    let (cmd, argv) = if let Some(c) = cmd {
        let parts: Vec<String> = c.split_whitespace().map(String::from).collect();
        (parts[0].clone(), parts)
    } else {
        (String::new(), Vec::new())
    };

    let pty_open = PtyOpen {
        cmd,
        argv,
        env: HashMap::new(),
        cwd: "/".to_string(),
        cols,
        rows,
        // ⚠️ 曾硬编码 false 且命令缺 as_root 参数——前端 asRoot 被静默忽略,
        // root 终端 attach 到 node 会话（2026-08-08 实测）
        as_root,
        // 接线常驻终端：GUI 终端复用以容器为单位的常驻会话
        // （server 持有句柄，连接断开不清理；重开窗口回放当前屏幕）
        attach: true,
    };

    // 在此连接上发送 pty.open 并接收响应
    framed
        .send(Frame::Json(Message {
            id: 1,
            kind: MsgKind::Req,
            op: "pty.open".to_string(),
            payload: serde_json::to_value(pty_open).map_err(|e| e.to_string())?,
            err: None,
        }))
        .await
        .map_err(|e| format!("发送 pty.open 失败：{}", e))?;

    let resp = framed
        .next()
        .await
        .ok_or_else(|| "pty.open 响应为空".to_string())?
        .map_err(|e| format!("接收 pty.open 响应失败：{}", e))?;

    let Frame::Json(resp_msg) = resp else {
        return Err("pty.open 响应应为 JSON 帧".to_string());
    };
    if let Some(err) = resp_msg.err {
        return Err(format!("pty.open 失败：{} {}", err.code, err.message));
    }

    let open_resp: PtyOpenResp = serde_json::from_value(resp_msg.payload)
        .map_err(|e| format!("解析 pty.open 响应失败：{}", e))?;
    let stream_id = open_resp.stream_id;

    // 拆分读写侧：写侧共享给 pty_write/resize/close，读侧归本任务的 reader
    let (sink, stream) = framed.split();
    let sink = Arc::new(tokio::sync::Mutex::new(sink));

    {
        let mut active = sess.active_ptys.lock().await;
        active.insert(stream_id, sink.clone());
    }

    // reader：专用连接读侧 → Channel（data / exited），退出时清理注册
    let active_ptys = Arc::clone(&sess.active_ptys);
    tokio::spawn(async move {
        let mut stream = stream;
        loop {
            match stream.next().await {
                Some(Ok(frame)) => match frame {
                    Frame::Raw { stream_id: sid, data } => {
                        if sid == stream_id {
                            let event = PtyEvent {
                                kind: "data".to_string(),
                                data: Some(data),
                                code: None,
                                cwd: None,
                            };
                            if on_event.send(event).is_err() {
                                break; // 通道关闭
                            }
                        }
                    }
                    Frame::Json(msg) => {
                        if msg.op == "pty.exited" {
                            match serde_json::from_value::<PtyExited>(msg.payload) {
                                Ok(exited) if exited.stream_id == stream_id => {
                                    let event = PtyEvent {
                                        kind: "exited".to_string(),
                                        data: None,
                                        code: Some(exited.code),
                                        cwd: None,
                                    };
                                    let _ = on_event.send(event);
                                    break;
                                }
                                _ => {}
                            }
                        } else if msg.op == "pty.cwdChanged" {
                            // server 主动推送（TTY 事件驱动：输入回车时检测 cwd 变化）
                            if let Some(cwd) = msg
                                .payload
                                .get("cwd")
                                .and_then(|c| c.as_str())
                            {
                                let event = PtyEvent {
                                    kind: "cwdChanged".to_string(),
                                    data: None,
                                    code: None,
                                    cwd: Some(cwd.to_string()),
                                };
                                let _ = on_event.send(event);
                            }
                        }
                    }
                },
                Some(Err(e)) => {
                    error!("PTY 流读取错误：{}", e);
                    break;
                }
                None => break,
            }
        }

        // 清理
        let mut active = active_ptys.lock().await;
        active.remove(&stream_id);
    });

    Ok(stream_id)
}

/// 向 PTY 写入数据（走该 PTY 专用连接的写侧）
#[tauri::command]
async fn pty_write(
    session: tauri::State<'_, Option<GuiSession>>,
    stream_id: u32,
    data: Vec<u8>,
) -> Result<(), String> {
    let sess = session.inner().as_ref().ok_or_else(|| "当前模式不是单容器模式".to_string())?;

    let sink = {
        let active = sess.active_ptys.lock().await;
        active.get(&stream_id).cloned()
    }
    .ok_or_else(|| format!("PTY 流 {} 不存在", stream_id))?;

    sink.lock()
        .await
        .send(Frame::Raw { stream_id, data })
        .await
        .map_err(|e| format!("PTY 写入失败：{}", e))?;
    Ok(())
}

/// 调整 PTY 大小（走该 PTY 专用连接的写侧）
#[tauri::command]
async fn pty_resize(
    session: tauri::State<'_, Option<GuiSession>>,
    stream_id: u32,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let sess = session.inner().as_ref().ok_or_else(|| "当前模式不是单容器模式".to_string())?;

    // 流已关闭时静默忽略（resize 可能在退出竞态中触发）
    let maybe_sink = {
        let active = sess.active_ptys.lock().await;
        active.get(&stream_id).cloned()
    };
    let Some(sink) = maybe_sink else {
        return Ok(());
    };

    let resize = PtyResize {
        stream_id,
        cols,
        rows,
    };
    sink.lock()
        .await
        .send(Frame::Json(Message {
            id: 2,
            kind: MsgKind::Req,
            op: "pty.resize".to_string(),
            payload: serde_json::to_value(resize).map_err(|e| e.to_string())?,
            err: None,
        }))
        .await
        .map_err(|e| format!("PTY resize 失败：{}", e))?;
    Ok(())
}

/// 关闭 PTY 会话（走该 PTY 专用连接的写侧）
#[tauri::command]
async fn pty_close(
    session: tauri::State<'_, Option<GuiSession>>,
    stream_id: u32,
) -> Result<(), String> {
    let sess = session.inner().as_ref().ok_or_else(|| "当前模式不是单容器模式".to_string())?;

    // 流已关闭时静默忽略
    let maybe_sink = {
        let active = sess.active_ptys.lock().await;
        active.get(&stream_id).cloned()
    };
    let Some(sink) = maybe_sink else {
        return Ok(());
    };

    let close = PtyClose {
        stream_id,
    };
    sink.lock()
        .await
        .send(Frame::Json(Message {
            id: 3,
            kind: MsgKind::Req,
            op: "pty.close".to_string(),
            payload: serde_json::to_value(close).map_err(|e| e.to_string())?,
            err: None,
        }))
        .await
        .map_err(|e| format!("PTY close 失败：{}", e))?;

    // 清理活动 PTY
    {
        let mut active = sess.active_ptys.lock().await;
        active.remove(&stream_id);
    }

    Ok(())
}

/// 心跳保活（走该 PTY 专用连接发 ping 帧）。
///
/// server 对无帧连接有 idle 超时（有 PTY 的连接 1h）——终端长时间无输出
/// 时靠心跳维持连接，否则连接被回收后输入永久失效（前端无法感知）。
/// ping 响应由 pty_open 的 reader 任务消费（非 pty.exited 的 JSON 忽略）。
#[tauri::command]
async fn pty_ping(
    session: tauri::State<'_, Option<GuiSession>>,
    stream_id: u32,
) -> Result<(), String> {
    let sess = session.inner().as_ref().ok_or_else(|| "当前模式不是单容器模式".to_string())?;

    // 流已关闭时静默忽略（ping 可能在退出竞态中触发）
    let maybe_sink = {
        let active = sess.active_ptys.lock().await;
        active.get(&stream_id).cloned()
    };
    let Some(sink) = maybe_sink else {
        return Ok(());
    };

    sink.lock()
        .await
        .send(Frame::Json(Message {
            id: 0,
            kind: MsgKind::Req,
            op: "ping".to_string(),
            payload: serde_json::Value::Null,
            err: None,
        }))
        .await
        .map_err(|e| format!("ping 发送失败：{}", e))?;
    Ok(())
}

/// 查询 PTY 会话主进程的实时工作目录（文件浏览器"跟随终端"）。
/// 经共享 socket 发 pty.cwd（不占 PTY 专用连接）。
#[tauri::command]
async fn pty_cwd(
    session: tauri::State<'_, Option<GuiSession>>,
    stream_id: u32,
) -> Result<String, String> {
    let sess = session.inner().as_ref().ok_or_else(|| "当前模式不是单容器模式".to_string())?;

    let req = easytidy_protocol::ops::PtyCwd { stream_id };
    let resp = send_json_request(
        sess,
        "pty.cwd".to_string(),
        serde_json::to_value(req).map_err(|e| e.to_string())?,
    )
    .await
    .map_err(|e| e.to_string())?;
    if let Some(err) = resp.err {
        return Err(format!("{} {}", err.code, err.message));
    }
    let cwd_resp: easytidy_protocol::ops::PtyCwdResp = serde_json::from_value(resp.payload)
        .map_err(|e| format!("解析 pty.cwd 响应失败：{e}"))?;
    Ok(cwd_resp.cwd)
}

// ============================================================================
// 文件系统命令
// ============================================================================

/// 文件系统条目（前端）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FsEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: Option<u64>,
    pub mtime: i64,
}

/// 列出目录内容
#[tauri::command]
async fn fs_list(
    session: tauri::State<'_, Option<GuiSession>>,
    path: String,
) -> Result<Vec<FsEntry>, String> {
    let sess = session.inner().as_ref().ok_or_else(|| "当前模式不是单容器模式".to_string())?;

    let list_req = FsList { path };
    let resp = send_json_request(sess, "fs.list".to_string(),
        serde_json::to_value(list_req).map_err(|e| e.to_string())?)
        .await.map_err(|e| e.to_string())?;

    let list_resp: FsListResp = serde_json::from_value(resp.payload)
        .map_err(|e| format!("解析 fs.list 响应失败：{}", e))?;

    let entries: Vec<FsEntry> = list_resp.entries.into_iter().map(|e| FsEntry {
        name: e.name,
        is_dir: matches!(e.entry_type, easytidy_protocol::ops::FsEntryType::Dir),
        size: e.size,
        mtime: 0, // TODO: 从 FsStatResp 获取
    }).collect();

    Ok(entries)
}

/// 读取文件内容（返回 base64）
#[tauri::command]
async fn fs_read(
    session: tauri::State<'_, Option<GuiSession>>,
    path: String,
) -> Result<String, String> {
    let sess = session.inner().as_ref().ok_or_else(|| "当前模式不是单容器模式".to_string())?;

    let read_req = FsRead { path, offset: None, len: None };
    let resp = send_json_request(sess, "fs.read".to_string(),
        serde_json::to_value(read_req).map_err(|e| e.to_string())?)
        .await.map_err(|e| e.to_string())?;

    let read_resp: FsReadResp = serde_json::from_value(resp.payload)
        .map_err(|e| format!("解析 fs.read 响应失败：{}", e))?;

    Ok(read_resp.data_b64)
}

/// 写入文件内容（接收 base64）
#[tauri::command]
async fn fs_write(
    session: tauri::State<'_, Option<GuiSession>>,
    path: String,
    data_b64: String,
) -> Result<(), String> {
    let sess = session.inner().as_ref().ok_or_else(|| "当前模式不是单容器模式".to_string())?;

    let write_req = FsWrite { path, data_b64 };
    send_json_request(sess, "fs.write".to_string(),
        serde_json::to_value(write_req).map_err(|e| e.to_string())?)
        .await.map_err(|e| e.to_string())?;

    Ok(())
}

// ============================================================================
// 桌面应用命令
// ============================================================================

/// 应用信息（前端）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppInfoFrontend {
    pub name: String,
    pub icon_path: Option<String>,
    pub exec: String,
    pub comment: Option<String>,
    pub desktop_file: String,
    pub categories: Option<String>,
    pub startup_notify: bool,
    pub startup_wm_class: Option<String>,
}

/// 列出桌面应用
#[tauri::command]
async fn apps_list(
    session: tauri::State<'_, Option<GuiSession>>,
) -> Result<Vec<AppInfoFrontend>, String> {
    let sess = session.inner().as_ref().ok_or_else(|| "当前模式不是单容器模式".to_string())?;

    let resp = send_json_request(sess, "apps.list".to_string(),
        serde_json::to_value(AppsList).map_err(|e| e.to_string())?)
        .await.map_err(|e| e.to_string())?;

    let list_resp: AppsListResp = serde_json::from_value(resp.payload)
        .map_err(|e| format!("解析 apps.list 响应失败：{}", e))?;

    let apps: Vec<AppInfoFrontend> = list_resp.apps.into_iter().map(|a| AppInfoFrontend {
        name: a.name,
        icon_path: a.icon_path,
        exec: a.exec,
        comment: a.comment,
        desktop_file: a.desktop_file,
        categories: a.categories,
        startup_notify: a.startup_notify,
        startup_wm_class: a.startup_wm_class,
    }).collect();

    Ok(apps)
}

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

/// 经 server fs.read 拉取容器内文件（passthrough 图标搬运；失败返回 Err，
/// 调用方应忽略——图标缺失仅影响显示，不影响导出）
async fn fetch_container_file(
    sess: &GuiSession,
    path: &str,
) -> Result<Vec<u8>, String> {
    let read_req = FsRead {
        path: path.to_string(),
        offset: None,
        len: None,
    };
    let resp = send_json_request(
        sess,
        "fs.read".to_string(),
        serde_json::to_value(read_req).map_err(|e| e.to_string())?,
    )
    .await
    .map_err(|e| e.to_string())?;
    if let Some(err) = resp.err {
        return Err(format!("{} {}", err.code, err.message));
    }
    let read_resp: FsReadResp = serde_json::from_value(resp.payload)
        .map_err(|e| format!("解析 fs.read 响应失败：{e}"))?;
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(read_resp.data_b64)
        .map_err(|e| format!("图标 base64 解码失败：{e}"))
}

/// 获取 passthrough 状态（已导出 .desktop 全文 + 配置的应用条目）。
///
/// 返回（前端契约）：
/// ```json
/// { "exported": [{"desktop_file","content"}],
///   "configured_apps": [{"id","name","cmd","desktop_file"?,"auto_start"}] }
/// ```
/// `configured_apps` 是 passthrough.toml 中有状态条目（auto_start=true 或
/// custom 应用）；扫描应用若未配置则不在此（前端按 id 匹配查 auto-start）。
#[tauri::command]
async fn passthrough_state(
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
async fn passthrough_set_auto_start(
    session: tauri::State<'_, Option<GuiSession>>,
    id: String,
    name: String,
    cmd: String,
    enabled: bool,
) -> Result<(), String> {
    let sess = session.inner().as_ref().ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    let container = &sess.container_name;

    let app = easytidy_core::passthrough::PassthroughApp {
        id: id.clone(),
        name,
        cmd: clean_exec(&cmd),
        desktop_file: (!id.starts_with("custom:")).then(|| id.clone()),
        auto_start: false,
    };
    let config_file = passthrough_config_file()?;
    config_file
        .set_auto_start(container, app, enabled)
        .map_err(|e| e.to_string())?;
    info!("passthrough auto-start 已设置：{container} enabled={enabled}");
    Ok(())
}

/// 添加自定义应用（固定目录扫描之外，如 `google-chrome-stable --disable-dev-shm-usage`）。
/// 返回 AppInfoFrontend（desktop_file=`custom:<name>`），前端直接进现有导出流。
#[tauri::command]
async fn passthrough_add_custom(
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
        icon_path: None,
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
async fn passthrough_remove_app(
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

/// 导出 passthrough 应用（宿主生成 .desktop：应用菜单 + 桌面图标。
/// distrobox 风格 TryExec/GenericName/Keywords/Actions=Remove；桌面图标
/// 经 chmod +x + `gio metadata::trusted` 信任标记（GNOME 双击必需）。
/// 生成逻辑在 core::desktop::write_passthrough，CLI unexport 与其共用）
#[tauri::command]
async fn passthrough_export(
    session: tauri::State<'_, Option<GuiSession>>,
    app: AppInfoFrontend,
    // 同时创建桌面图标（GNOME 桌面默认不显示应用菜单，入口在桌面路径）
    desktop_icon: Option<bool>,
) -> Result<String, String> {
    let sess = session.inner().as_ref().ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    let container = sess.container_name.clone();

    // Exec 清理 %U/%f 等占位符（宿主侧不展开容器内文件参数）
    let exec = clean_exec(&app.exec);

    // 图标：custom 应用（无容器内图标）用内置品牌图标；扫描应用经
    // server fs.read 搬运容器内图标 → 宿主 hicolor 缓存
    let mut icon_attr = None;
    if app.desktop_file.starts_with("custom:") {
        icon_attr = easytidy_core::desktop::ensure_gui_icon();
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
                    let ext = icon_path.rsplit('.').next().unwrap_or("png");
                    let icon_file =
                        icons_dir.join(format!("easytidy-pt-{container}-{safe}.{ext}"));
                    if std::fs::write(&icon_file, &icon_data).is_ok() {
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
async fn passthrough_revoke(
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
async fn export_gui_shortcut(
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
async fn config_get(
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
async fn config_set(
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

// ============================================================================
// 生命周期命令
// ============================================================================

/// 关闭容器
#[tauri::command]
async fn container_shutdown(
    session: tauri::State<'_, Option<GuiSession>>,
) -> Result<(), String> {
    let sess = session.inner().as_ref().ok_or_else(|| "当前模式不是单容器模式".to_string())?;

    send_json_request(sess, "lifecycle.shutdown".to_string(),
        serde_json::to_value(LifecycleShutdown).map_err(|e| e.to_string())?)
        .await.map_err(|e| e.to_string())?;

    Ok(())
}

// ============================================================================
// Tauri 启动
// ============================================================================

pub fn run(mode: AppMode, _config_file: Option<String>) {
    // 第一步：应用 NVIDIA 规避措施（在 Tauri 初始化之前）
    apply_nvidia_workaround();

    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env()
            .add_directive("easytidy_gui=info".parse().unwrap()))
        .init();

    // 第二步：启动 Tauri 应用
    // 根据模式决定是否初始化 GuiSession
    let gui_session = match &mode {
        AppMode::Container { name } => Some(GuiSession {
            container_name: name.clone(),
            socket: tokio::sync::Mutex::new(None),
            next_msg_id: AtomicU64::new(2), // 握手已用 1
            active_ptys: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        }),
        AppMode::Centralized => None,
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(mode)
        .manage(PodmanState::new())
        .manage(gui_session)
        .invoke_handler(tauri::generate_handler![
            // 通用
            toggle_devtools,
            get_app_mode,
            // 中心化模式
            list_containers,
            create_container,
            start_container,
            stop_container,
            restart_container,
            remove_container,
            inspect_container,
            build_server,
            open_container_window,
            // 环境语义面板（docs/13-mutable-env-paradigm.md）
            flavor_list,
            env_list,
            env_new,
            env_rm,
            env_snapshot,
            env_fork,
            env_start,
            env_stop,
            // 配置管理器（M4 前置）
            get_container_config,
            apply_container_config,
            // 单容器模式 - PTY
            pty_open,
            pty_write,
            pty_resize,
            pty_close,
            pty_ping,
            pty_cwd,
            // 单容器模式 - 文件系统
            fs_list,
            fs_read,
            fs_write,
            // 单容器模式 - 桌面应用
            apps_list,
            // 单容器模式 - Passthrough
            passthrough_state,
            passthrough_export,
            passthrough_revoke,
            passthrough_set_auto_start,
            passthrough_add_custom,
            passthrough_remove_app,
            export_gui_shortcut,
            // 单容器模式 - 配置
            config_get,
            config_set,
            // 单容器模式 - 生命周期
            container_shutdown,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
