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

use std::collections::HashMap;
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
use tracing::{debug, error, info};

use easytidy_core::configfile::ConfigFile;
use easytidy_core::desktop;
use easytidy_core::models::{ContainerConfig, ContainerSummary};
use easytidy_core::podman::Podman;
use easytidy_protocol::{
    Frame, FrameCodec, Message, MsgKind, Handshake, HandshakeAck, PROTOCOL_VERSION,
};
use easytidy_protocol::ops::{
    PtyOpen, PtyOpenResp, PtyResize, PtyClose, PtyExited,
    FsList, FsListResp, FsRead, FsReadResp, FsWrite,
    AppsList, AppsListResp,
    PtState, PtStateResp, PtExport, PtExportResp, PtRevoke, PtRevokeResp,
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
    /// 事件类型："data" / "exited"
    pub kind: String,
    /// 数据（kind="data" 时）
    pub data: Option<Vec<u8>>,
    /// 退出码（kind="exited" 时）
    pub code: Option<i32>,
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

    // 创建容器
    let id = p.create(&name, &image, &server_bin)
        .await
        .map_err(|e| e.to_string())?;

    // 注册到配置文件
    let config_path = ConfigFile::default_path()
        .map_err(|e| format!("解析配置路径失败：{}", e))?;
    let config_file = ConfigFile::with_path(config_path);

    let container_config = ContainerConfig {
        name: name.clone(),
        image: image.clone(),
        entry: None,
        silent_boot: false,
        persistent: true,
    };

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
async fn send_json_request(
    session: &GuiSession,
    op: String,
    payload: serde_json::Value,
) -> Result<Message> {
    ensure_session_connected(session).await?;

    let msg_id = session.next_msg_id.fetch_add(1, Ordering::SeqCst);

    let msg = Message {
        id: msg_id,
        kind: MsgKind::Req,
        op,
        payload,
        err: None,
    };

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
                                    };
                                    let _ = on_event.send(event);
                                    break;
                                }
                                _ => {}
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

    let sink = {
        let active = sess.active_ptys.lock().await;
        active.get(&stream_id).cloned()
    }
    .ok_or_else(|| format!("PTY 流 {} 不存在", stream_id))?;

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

    let sink = {
        let active = sess.active_ptys.lock().await;
        active.get(&stream_id).cloned()
    }
    .ok_or_else(|| format!("PTY 流 {} 不存在", stream_id))?;

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
    }).collect();

    Ok(apps)
}

// ============================================================================
// Passthrough 命令
// ============================================================================

/// Passthrough 状态条目（前端）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PtEntryFrontend {
    pub desktop_file: String,
    pub menu: bool,
    pub silent_boot: bool,
    pub generated_path: String,
}

/// 获取 passthrough 状态
#[tauri::command]
async fn passthrough_state(
    session: tauri::State<'_, Option<GuiSession>>,
) -> Result<Vec<PtEntryFrontend>, String> {
    let sess = session.inner().as_ref().ok_or_else(|| "当前模式不是单容器模式".to_string())?;

    let resp = send_json_request(sess, "passthrough.state".to_string(),
        serde_json::to_value(PtState).map_err(|e| e.to_string())?)
        .await.map_err(|e| e.to_string())?;

    let state_resp: PtStateResp = serde_json::from_value(resp.payload)
        .map_err(|e| format!("解析 passthrough.state 响应失败：{}", e))?;

    let entries: Vec<PtEntryFrontend> = state_resp.entries.into_iter().map(|e| PtEntryFrontend {
        desktop_file: e.desktop_file,
        menu: e.menu,
        silent_boot: e.silent_boot,
        generated_path: e.generated_path,
    }).collect();

    Ok(entries)
}

/// 导出 passthrough 应用
#[tauri::command]
async fn passthrough_export(
    session: tauri::State<'_, Option<GuiSession>>,
    desktop_file: String,
) -> Result<String, String> {
    let sess = session.inner().as_ref().ok_or_else(|| "当前模式不是单容器模式".to_string())?;

    let export_req = PtExport {
        desktop_file: desktop_file.clone(),
        menu: true,
        silent_boot: false,
    };

    let resp = send_json_request(sess, "passthrough.export".to_string(),
        serde_json::to_value(export_req).map_err(|e| e.to_string())?)
        .await.map_err(|e| e.to_string())?;

    let export_resp: PtExportResp = serde_json::from_value(resp.payload)
        .map_err(|e| format!("解析 passthrough.export 响应失败：{}", e))?;

    Ok(export_resp.generated_path)
}

/// 撤销 passthrough 导出
#[tauri::command]
async fn passthrough_revoke(
    session: tauri::State<'_, Option<GuiSession>>,
    desktop_file: String,
) -> Result<String, String> {
    let sess = session.inner().as_ref().ok_or_else(|| "当前模式不是单容器模式".to_string())?;

    let revoke_req = PtRevoke {
        desktop_file: desktop_file.clone(),
    };

    let resp = send_json_request(sess, "passthrough.revoke".to_string(),
        serde_json::to_value(revoke_req).map_err(|e| e.to_string())?)
        .await.map_err(|e| e.to_string())?;

    let revoke_resp: PtRevokeResp = serde_json::from_value(resp.payload)
        .map_err(|e| format!("解析 passthrough.revoke 响应失败：{}", e))?;

    Ok(revoke_resp.removed_path)
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
            // 单容器模式 - PTY
            pty_open,
            pty_write,
            pty_resize,
            pty_close,
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
            // 单容器模式 - 配置
            config_get,
            config_set,
            // 单容器模式 - 生命周期
            container_shutdown,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
