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
use std::sync::Arc;
use tokio::net::UnixStream;
use tokio_util::codec::Framed;

use easytidy_core::podman::Podman;
use easytidy_protocol::{Frame, FrameCodec};

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

/// 单容器 GUI 会话（托管在 Tauri State 中）
pub struct GuiSession {
    /// 容器名
    pub container_name: String,
    /// Socket 会话（使用 tokio Mutex 因为需要 async）
    pub socket: tokio::sync::Mutex<Option<Framed<UnixStream, FrameCodec>>>,
    /// 连接状态机（与 socket Option 同步维护）
    pub conn_state: std::sync::Mutex<ConnectionState>,
    /// 握手返回的会话 ID（已连接后非空；server 每连接创建）
    pub session_id: std::sync::Mutex<Option<String>>,
    /// 下一个消息 ID
    pub next_msg_id: AtomicU64,
    /// 活动 PTY 流（stream_id -> 该 PTY 专用连接的写侧；node 与 root 会话共用）
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
