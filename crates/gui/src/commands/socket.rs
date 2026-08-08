//! 容器 server socket 连接与请求（共享连接 + 专用 PTY 连接）。

use std::sync::atomic::Ordering;
use futures::{SinkExt, StreamExt};
use tokio::net::UnixStream;
use tokio_util::codec::Framed;
use anyhow::{Context, Result};
use tracing::{debug, info, warn};

use easytidy_core::podman::Podman;
use easytidy_protocol::{
    Frame, FrameCodec, Message, MsgKind, Handshake, HandshakeAck, PROTOCOL_VERSION,
};

use crate::state::GuiSession;

// ============================================================================
// 单容器模式命令（socket 通信）
// ============================================================================

/// 连接到容器 server socket（延迟连接，仅在首次单容器命令时调用）
pub async fn connect_to_container(container_name: &str) -> Result<Framed<UnixStream, FrameCodec>> {
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
pub async fn ensure_session_connected(session: &GuiSession) -> Result<()> {
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
pub async fn try_send_json_request(
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
pub async fn send_json_request(
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
