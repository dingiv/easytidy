//! GUI 托管状态（Tauri State 类型）。
//!
//! - AppMode（启动时解析的命令行参数）
//! - PodmanState（Master GUI 模式,延迟连接）
//! - GuiSession（单容器模式,socket 会话）
//! - PtyEvent 等命令返回类型

use anyhow::{Context, Result};
use futures::stream::SplitSink;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, OnceLock};
use tokio::net::UnixStream;
use tokio_util::codec::Framed;

use easytidy_core::podman::Podman;
use easytidy_protocol::{Frame, FrameCodec, Message};

/// 全局 AppHandle（lib.rs setup 时注入；后台任务如 socket reader 用它
/// emit 前端事件——Tauri 2 无 AppHandle::global()）。
static APP_HANDLE: OnceLock<tauri::AppHandle> = OnceLock::new();

/// 注入全局 AppHandle（Tauri setup 钩子调用，幂等：已设则忽略）。
pub fn set_app_handle(app: tauri::AppHandle) {
    let _ = APP_HANDLE.set(app);
}

/// 取全局 AppHandle（setup 后恒有值）。
pub fn app_handle() -> Option<tauri::AppHandle> {
    APP_HANDLE.get().cloned()
}

/// GUI 应用模式（main.rs 传入）
#[derive(Debug, Clone, PartialEq)]
pub enum AppMode {
    /// Master GUI：管理所有容器（中心化管理界面）
    Master,
    /// Worker GUI：单容器窗口（从前称 per-container / 单实例 GUI）
    Worker { name: String },
}

/// 单条 PTY 会话的写侧（该 PTY 专用连接的 sink）
pub type PtySink = Arc<tokio::sync::Mutex<SplitSink<Framed<UnixStream, FrameCodec>, Frame>>>;

/// root 终端 exec 的 stdin 写侧（`exec_no_tty` 的 `ExecPty.input`）。
/// 与 server socket 的 `PtySink` 不同：root 终端经 `podman exec --client new`
/// 桥接，输入写到 exec 流 → client 桥给 daemon PTY。
pub type RootInputSink =
    Arc<tokio::sync::Mutex<std::pin::Pin<Box<dyn tokio::io::AsyncWrite + Send>>>>;

/// 与容器 server 的连接状态机（客户端侧）。
///
/// 与 `GuiSession.socket`（Option<Framed>）同步维护：`None`=Unconnected、
/// `Some`=Connected，`Connecting` 表示连接/握手进行中（tokio Mutex 持锁期间）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    /// 未连接（初始 / 连接失败后回到此态）
    Unconnected,
    /// 连接中（socket connect + 握手进行中）
    Connecting,
    /// 已连接（握手完成，可发请求）
    Connected,
}

/// 共享 socket 连接（读写拆分：写侧给请求方共享，读侧归 reader 任务独占）。
///
/// push-ready 模型（2026-09-03）：server 可主动推 Evt 帧（如 ui.edit）——
/// 旧同步模型（发一帧收一帧）会在无请求在途时没人读 socket，事件滞留；
/// 拆出 reader 任务后事件即时送达（Tauri emit），响应按 msg.id 路由到
/// 各自的 oneshot（[`GuiSession::pending`]），与 pty.rs 专用连接同模式。
pub struct SharedConn {
    /// 写侧（Mutex 串行化帧写入；并发请求各自锁住发一帧即释放）
    pub sink: tokio::sync::Mutex<SplitSink<Framed<UnixStream, FrameCodec>, Frame>>,
}

/// 单容器 GUI 会话（托管在 Tauri State 中）
pub struct GuiSession {
    /// 容器名
    pub container_name: String,
    /// 共享 socket 连接（push-ready：Arc 供 reader/keepalive 任务持有；
    /// Option = 未连接/已断开）
    pub socket: Arc<tokio::sync::Mutex<Option<Arc<SharedConn>>>>,
    /// 连接状态机（Arc：reader 任务断连时清除；与 socket 同步维护）
    pub conn_state: Arc<std::sync::Mutex<ConnectionState>>,
    /// 握手返回的会话 ID（已连接后非空；server 每连接创建）
    pub session_id: std::sync::Mutex<Option<String>>,
    /// 下一个消息 ID（Arc：keepalive 任务持有克隆）
    pub next_msg_id: Arc<AtomicU64>,
    /// 待响应等待者：msg.id → oneshot（reader 任务按 id 路由响应；
    /// Arc：命令任务与 reader 任务共享）
    pub pending: Arc<tokio::sync::Mutex<HashMap<u64, tokio::sync::oneshot::Sender<Message>>>>,
    /// 活动 PTY 流（stream_id -> 该 PTY 专用连接的写侧）
    pub active_ptys: Arc<tokio::sync::Mutex<HashMap<u32, PtySink>>>,
    /// root 会话写侧（每容器一个共享 root shell；经 podman exec 的 client
    /// stdin 桥到 daemon PTY。None = 未 attach。root 会话是单例——不需要
    /// stream_id 索引）
    pub root_sink: Arc<tokio::sync::Mutex<Option<RootInputSink>>>,
    /// root 终端 attach 串行锁：`root_terminal_attach` 的「查 session → 建/attach」
    /// 全程持锁，保证幂等（StrictMode 双 invoke 并发 attach 只建一个 root bash，
    /// 第二个复用既有 session）。
    pub root_attach_lock: Arc<tokio::sync::Mutex<()>>,
}

/// PTY 事件（通过 Channel 发送给前端）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PtyEvent {
    /// 事件类型："data" / "exited" / "cwdChanged"
    pub kind: String,
    /// 数据（kind="data" 时；base64 编码——JSON 数字数组体积膨胀 4-5 倍，
    /// vi 等全屏重绘的输出洪流会打爆 WebKitGTK IPC 管道，曾致终端输入卡死）
    pub data: Option<String>,
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
    pub async fn get(&self) -> Result<Podman> {
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
    pub async fn return_podman(&self, podman: Podman) {
        let mut guard = self.0.lock().await;
        *guard = Some(podman);
    }
}

/// 前端按 result.mode 判断："master" | "worker"。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppModeResponse {
    pub mode: String,
    pub name: Option<String>,
}
