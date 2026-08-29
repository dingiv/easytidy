//! 容器 server socket 连接与请求（共享连接 + 专用 PTY 连接）。

use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use std::io::ErrorKind;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio_util::codec::Framed;
use tracing::{debug, info, warn};

use easytidy_core::podman::Podman;
use easytidy_protocol::{
    Frame, FrameCodec, Handshake, HandshakeAck, Message, MsgKind, PROTOCOL_VERSION,
};

use crate::state::GuiSession;

// ============================================================================
// 单容器模式命令（socket 通信）
// ============================================================================

/// 连接就绪等待：容器刚创建时 podman 已报 running，但容器内 server 可能
/// 尚未 bind socket（文件不存在 ENOENT / 无监听 ECONNREFUSED）——这类瞬时
/// 错误重试等待，其余错误（协议不匹配、容器未运行等）立即返回。
const CONNECT_ATTEMPTS: u32 = 10;
const CONNECT_RETRY_DELAY: Duration = Duration::from_millis(500);

/// 「server 未就绪」类瞬时连接错误（socket 文件未创建 / 尚无监听）——可重试。
fn is_not_ready_error(e: &anyhow::Error) -> bool {
    e.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .map(|io| matches!(io.kind(), ErrorKind::NotFound | ErrorKind::ConnectionRefused))
            .unwrap_or(false)
    })
}

/// 连接 + 握手，server 未就绪时按 [`CONNECT_ATTEMPTS`] × [`CONNECT_RETRY_DELAY`]
/// 重试等待（容器刚创建、server 还在启动的典型场景）。
async fn connect_with_retry(
    socket_path: &std::path::Path,
) -> Result<(Framed<UnixStream, FrameCodec>, String)> {
    for attempt in 1..=CONNECT_ATTEMPTS {
        match connect_and_handshake(socket_path).await {
            Ok(result) => return Ok(result),
            Err(e) => {
                if !is_not_ready_error(&e) || attempt == CONNECT_ATTEMPTS {
                    return Err(e);
                }
                warn!(
                    "容器 server 未就绪（第 {attempt}/{CONNECT_ATTEMPTS} 次尝试，{}ms 后重试）：{e}",
                    CONNECT_RETRY_DELAY.as_millis()
                );
                tokio::time::sleep(CONNECT_RETRY_DELAY).await;
            }
        }
    }
    unreachable!("重试循环必然 return")
}

/// 连接到容器 server socket。
///
/// 返回 `(Framed, session_id)`：握手成功即已进入「已连接」，session_id 由 server
/// 每连接创建并返回（协议规范化：握手 → session ID → 可发请求）。
/// server 未就绪时按 [`CONNECT_ATTEMPTS`] × [`CONNECT_RETRY_DELAY`] 等待。
pub async fn connect_to_container(
    container_name: &str,
) -> Result<(Framed<UnixStream, FrameCodec>, String)> {
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

    // 连接到 socket（server 未就绪时重试等待）
    let socket_path = easytidy_core::host_socket_path(container_name)?;
    info!("连接到 socket：{}", socket_path.display());
    connect_with_retry(&socket_path).await
}

/// 连接 socket + 握手（`connect_to_container` 的可重试单元）。
async fn connect_and_handshake(
    socket_path: &std::path::Path,
) -> Result<(Framed<UnixStream, FrameCodec>, String)> {
    let stream = UnixStream::connect(socket_path).await.context(format!(
        "连接 socket 失败（容器可能未就绪）：{}",
        socket_path.display()
    ))?;

    // 握手
    let codec = FrameCodec::new();
    let mut framed = Framed::new(stream, codec);

    let handshake = Handshake {
        v: PROTOCOL_VERSION,
        client: "easytidy-gui".to_string(),
        wants: vec![
            "pty".to_string(),
            "fs".to_string(),
            "apps".to_string(),
            "passthrough".to_string(),
            "config".to_string(),
            "lifecycle".to_string(),
        ],
    };
    let handshake_msg = Message {
        id: 1,
        kind: MsgKind::Req,
        op: "hello".to_string(),
        payload: serde_json::to_value(handshake)?,
        err: None,
    };
    framed
        .send(Frame::Json(handshake_msg))
        .await
        .context("发送握手失败")?;

    let ack_frame = framed
        .next()
        .await
        .context("接收握手确认失败")?
        .context("握手确认帧为空")?;

    let ack_msg = match ack_frame {
        Frame::Json(msg) => msg,
        Frame::Raw { .. } => return Err(anyhow::anyhow!("握手响应应为 JSON 帧")),
    };

    if ack_msg.kind != MsgKind::Resp || ack_msg.op != "hello" {
        return Err(anyhow::anyhow!("握手响应格式错误"));
    }

    let ack: HandshakeAck = serde_json::from_value(ack_msg.payload).context("解析握手确认失败")?;

    debug!(
        "握手成功：server={}, v={}, session={}",
        ack.server, ack.v, ack.session_id
    );

    if ack.v != PROTOCOL_VERSION {
        return Err(anyhow::anyhow!(
            "协议版本不匹配：客户端={}，服务端={}",
            PROTOCOL_VERSION,
            ack.v
        ));
    }

    Ok((framed, ack.session_id))
}

/// 发送 JSON 请求并接收响应（共享 session socket）。
///
/// **原子**：「无连接则连接」与发送/接收在同一把锁内完成——并发请求要么
/// 排队等连接建好后直接复用，要么自己触发连接，绝不会看到半拆掉的 None
/// socket（旧两段式 ensure→send 在并发 + 旧连接被重试路径 take 时会产生
/// 「Socket 未初始化」误报）。
pub async fn try_send_json_request(session: &GuiSession, msg: &Message) -> Result<Message> {
    let mut socket_guard = session.socket.lock().await;

    // 无连接（首连 / 旧连接因断连被丢弃）：持锁连接，等待者队在我们之后
    if socket_guard.is_none() {
        *session.conn_state.lock().unwrap() = crate::state::ConnectionState::Connecting;
        let container_name = session.container_name.clone();
        tracing::warn!("socket 未建立，连接到 {}", container_name);
        match connect_to_container(&container_name).await {
            Ok((framed, session_id)) => {
                *session.session_id.lock().unwrap() = Some(session_id);
                *session.conn_state.lock().unwrap() = crate::state::ConnectionState::Connected;
                *socket_guard = Some(framed);
                tracing::info!("socket 已建立");
            }
            Err(e) => {
                *session.conn_state.lock().unwrap() = crate::state::ConnectionState::Unconnected;
                return Err(e);
            }
        }
    }

    let socket = socket_guard.as_mut().expect("上面已确保 socket 为 Some");

    tracing::debug!(
        "try_send_json_request: op={} id={}",
        msg.op,
        msg.id
    );

    socket
        .send(Frame::Json(msg.clone()))
        .await
        .context("发送请求失败")?;

    // 接收响应
    let response = socket
        .next()
        .await
        .context("接收响应失败")?
        .context("响应帧为空")?;

    match response {
        Frame::Json(resp_msg) => {
            if let Some(ref err) = resp_msg.err {
                tracing::warn!(
                    "server op_failed: op={} code={} msg={}",
                    msg.op,
                    err.code,
                    err.message
                );
                Err(anyhow::anyhow!(
                    "服务器错误：{} - {}",
                    err.code,
                    err.message
                ))
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
/// server 断开（stale）—— 传输错时丢弃旧连接、重试一次（幂等请求场景
/// 足够）。重试是安全的：`take()` 与重连都在 `try_send_json_request` 的
/// 锁模型内，并发请求会等重连完成复用新连接。
pub async fn send_json_request(
    session: &GuiSession,
    op: String,
    payload: serde_json::Value,
) -> Result<Message> {
    let msg = Message {
        id: session.next_msg_id.fetch_add(1, Ordering::SeqCst),
        kind: MsgKind::Req,
        op: op.clone(),
        payload,
        err: None,
    };

    tracing::debug!("send_json_request → op={} id={}", op, msg.id);

    match try_send_json_request(session, &msg).await {
        Ok(resp) => Ok(resp),
        Err(e) => {
            let msg_str = e.to_string();
            tracing::warn!(
                "send_json_request: op={} id={} err={:?} msg_str_prefix={:?}",
                op,
                msg.id,
                e,
                &msg_str[..msg_str.len().min(40)]
            );
            // 区分"服务器 op_failed"与传输错:server 把 op_failed 用合法的 JSON
            // 帧回包(err.code+err.message 非空),`try_send_json_request` 一律映射
            // 成 Err → 老逻辑把数据错当 socket 错,触发 socket 拆接 + 重连循环,
            // 期间所有后续请求(包括 terminal 的 pty.write)都拿不到 socket → 终端
            // 静默断连。op_failed 是数据回包,不重连;只有真传输错(发送/接收
            // 失败、连接中断)才需要重建 socket 重试。
            if msg_str.starts_with("服务器错误") {
                tracing::warn!("send_json_request: op_failed 不重连 op={}", op);
                return Err(e);
            }
            tracing::warn!("send_json_request: 传输错,丢弃旧连接重试 op={}", op);
            // 丢弃旧连接(可能已被 server 空闲超时断开)；下次 try_send 持锁重连
            session.socket.lock().await.take();
            try_send_json_request(session, &msg).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UnixListener;

    /// 假 server：接受一条连接，消费 hello 帧，回握手 ack（session_id 固定）。
    async fn fake_server_accept_once(stream: UnixStream) {
        let mut framed = Framed::new(stream, FrameCodec::new());
        if let Some(Ok(Frame::Json(_msg))) = framed.next().await {
            let ack = Frame::Json(Message {
                id: 1,
                kind: MsgKind::Resp,
                op: "hello".to_string(),
                payload: serde_json::to_value(HandshakeAck {
                    v: PROTOCOL_VERSION,
                    server: "fake-server".to_string(),
                    capabilities: vec!["pty".to_string()],
                    session_id: "fake-session".to_string(),
                })
                .unwrap(),
                err: None,
            });
            let _ = framed.send(ack).await;
        }
    }

    #[test]
    fn test_is_not_ready_error_classification() {
        use std::io;
        // 与真实代码同构造方式（Result 的 .context() 保留 io::Error 在 chain 中）
        let wrap = |io_err: io::Error| -> anyhow::Error {
            let result: std::result::Result<io::Error, io::Error> = Err(io_err);
            result
                .context("连接 socket 失败（容器可能未就绪）：/run/server.sock")
                .unwrap_err()
        };
        // ENOENT（socket 文件尚未创建）→ 可重试
        let e = wrap(io::Error::new(io::ErrorKind::NotFound, "no such file"));
        assert!(is_not_ready_error(&e), "ENOENT 应判定为 server 未就绪");
        // ECONNREFUSED（文件存在但无监听）→ 可重试
        let e = wrap(io::Error::new(io::ErrorKind::ConnectionRefused, "refused"));
        assert!(is_not_ready_error(&e), "ECONNREFUSED 应判定为 server 未就绪");
        // 非 io 错误（如握手帧异常）→ 不可重试，立即返回
        let e = anyhow::anyhow!("握手响应应为 JSON 帧");
        assert!(!is_not_ready_error(&e));
        // 其他 io 错误（如权限拒绝）→ 不是"未就绪"，重试无意义
        let e = wrap(io::Error::new(io::ErrorKind::PermissionDenied, "denied"));
        assert!(!is_not_ready_error(&e));
    }

    /// server 延迟 listen（模拟容器刚创建、server 还在启动）→
    /// connect_with_retry 等待后成功（回归：此前直接报"容器可能未就绪"）
    #[tokio::test]
    async fn test_connect_with_retry_waits_for_server() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.sock");

        let path_clone = path.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(800)).await;
            let listener = UnixListener::bind(&path_clone).unwrap();
            if let Ok((stream, _)) = listener.accept().await {
                fake_server_accept_once(stream).await;
            }
        });

        let (framed, session_id) = connect_with_retry(&path).await.unwrap();
        assert_eq!(session_id, "fake-session");
        drop(framed);
    }
}
