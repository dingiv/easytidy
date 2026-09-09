//! 容器 server socket 连接与请求（共享连接 + 专用 PTY 连接）。
//!
//! 共享连接为 **push-ready** 模型（2026-09-03）：连接建立后拆读写侧
//! （照 [`crate::commands::pty`] 专用连接模式）——写侧给所有请求方共享，
//! 读侧归单一 **reader 任务**：Resp 按 msg.id 路由到各自 oneshot
//! （[`GuiSession::pending`]），Evt（server 主动推送，如 ui.edit）即时
//! Tauri emit 到前端。旧同步模型（发一帧收一帧）在无请求在途时没人读
//! socket，server 推的事件会滞留缓冲。`spawn_keepalive` 每 20s ping 防
//! server 30s 空闲超时，使「GUI 开着」≈「GUI 连着」。

use anyhow::{Context, Result};
use futures::stream::SplitStream;
use futures::{SinkExt, StreamExt};
use std::io::ErrorKind;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio_util::codec::Framed;
use tracing::{debug, info, warn};

use easytidy_core::podman::Podman;
use easytidy_protocol::{
    Frame, FrameCodec, Handshake, HandshakeAck, Message, MsgKind, PROTOCOL_VERSION, UiEdit,
};
use tauri::Emitter;

use crate::state::{ConnectionState, GuiSession, SharedConn};

// ============================================================================
// 单容器模式命令（socket 通信）
// ============================================================================

/// 连接就绪等待：容器刚创建时 podman 已报 running，但容器内 server 可能
/// 尚未 bind socket（文件不存在 ENOENT / 无监听 ECONNREFUSED）——这类瞬时
/// 错误重试等待，其余错误（协议不匹配、容器未运行等）立即返回。
const CONNECT_ATTEMPTS: u32 = 10;
const CONNECT_RETRY_DELAY: Duration = Duration::from_millis(500);

/// 响应等待超时（server 无 PTY 连接空闲超时 30s；正常 op 毫秒级返回）
const RESP_TIMEOUT: Duration = Duration::from_secs(30);

/// 保活 ping 周期（< server 无 PTY 连接空闲超时 30s）
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(20);

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
            // 订阅 server 主动推送事件（ui.edit 等）：server 据此把 Evt 路由
            // 到本连接（PTY 专用连接不声明，不会收到）
            "events".to_string(),
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

/// 确保共享 socket 已连接（无则连接 + 拆读写侧 + 起 reader/keepalive 任务），
/// 返回当前连接（写侧句柄）。
///
/// 连接建立持 [`GuiSession::socket`] 锁完成；reader/keepalive 任务持 Arc 克隆
/// 独立运行（任务不借用 GuiSession 本体，可长于任何命令生命周期）。
async fn ensure_connect(session: &GuiSession) -> Result<Arc<SharedConn>> {
    let mut guard = session.socket.lock().await;
    if let Some(conn) = guard.clone() {
        return Ok(conn);
    }

    *session.conn_state.lock().unwrap() = ConnectionState::Connecting;
    let container_name = session.container_name.clone();
    tracing::warn!("socket 未建立，连接到 {container_name}");
    let (framed, session_id) = match connect_to_container(&container_name).await {
        Ok(r) => r,
        Err(e) => {
            *session.conn_state.lock().unwrap() = ConnectionState::Unconnected;
            return Err(e);
        }
    };

    // 拆读写侧（照 pty.rs 模式）：写侧共享，读侧归 reader 任务
    let (sink, stream) = framed.split();
    let conn = Arc::new(SharedConn {
        sink: tokio::sync::Mutex::new(sink),
    });

    *session.session_id.lock().unwrap() = Some(session_id);
    *session.conn_state.lock().unwrap() = ConnectionState::Connected;
    *guard = Some(conn.clone());
    drop(guard);
    tracing::info!("socket 已建立（push-ready：读侧归 reader 任务）");

    spawn_shared_reader(session, stream, conn.clone());
    spawn_keepalive(session, conn.clone());
    Ok(conn)
}

/// 共享 socket reader 任务：Resp 按 msg.id 路由到 pending oneshot；Evt 推
/// Tauri 事件（前端监听）；读错/EOF 清除连接（下次请求自动重连）。
fn spawn_shared_reader(
    session: &GuiSession,
    stream: SplitStream<Framed<UnixStream, FrameCodec>>,
    conn: Arc<SharedConn>,
) {
    let socket = session.socket.clone();
    let pending = session.pending.clone();
    let conn_state = session.conn_state.clone();
    tokio::spawn(async move {
        let mut stream = stream;
        loop {
            match stream.next().await {
                Some(Ok(Frame::Json(msg))) => match msg.kind {
                    MsgKind::Resp => {
                        // 按 id 路由；无等待者（客户端已超时）→ 丢弃
                        let mut p = pending.lock().await;
                        if let Some(tx) = p.remove(&msg.id) {
                            let _ = tx.send(msg);
                        }
                    }
                    MsgKind::Evt => match msg.op.as_str() {
                        "ui.edit" => {
                            let Ok(payload) =
                                serde_json::from_value::<UiEdit>(msg.payload.clone())
                            else {
                                warn!("ui.edit 事件 payload 解析失败");
                                continue;
                            };
                            match crate::state::app_handle() {
                                Some(app) => {
                                    if let Err(e) = app.emit("server-ui-edit", &payload) {
                                        warn!("emit server-ui-edit 失败：{e}");
                                    }
                                }
                                None => warn!("无 AppHandle（setup 未完成？），无法 emit server-ui-edit"),
                            }
                        }
                        _ => debug!("忽略未处理的 Evt：{}", msg.op),
                    },
                    MsgKind::Req => {
                        warn!("收到 server 发来的 Req 帧（不应发生）：{}", msg.op);
                    }
                },
                Some(Ok(Frame::Raw { .. })) => {
                    debug!("共享连接收到 Raw 帧（不应发生），忽略");
                }
                Some(Err(e)) => {
                    warn!("共享 socket 读取错误：{e}");
                    break;
                }
                None => {
                    warn!("共享 socket 服务端关闭");
                    break;
                }
            }
        }
        // 退出时清连接（仅当仍为当前连接——避免误清新连接）
        let mut guard = socket.lock().await;
        if guard.as_ref().is_some_and(|c| Arc::ptr_eq(c, &conn)) {
            guard.take();
            *conn_state.lock().unwrap() = ConnectionState::Unconnected;
            tracing::warn!("共享 socket 连接已清除（下次请求自动重连）");
        }
    });
}

/// 保活任务：每 20s 发一个 ping——防 server 30s 空闲超时断连，使「GUI
/// 开着」≈「GUI 连着」（ui.edit 可路由到 GUI）。只保活不重连：连接断后
/// 本任务退出，下次请求自动重连。ping 的响应由 reader 丢弃（无 pending 注册）。
fn spawn_keepalive(session: &GuiSession, conn: Arc<SharedConn>) {
    let socket = session.socket.clone();
    let next_msg_id = session.next_msg_id.clone();
    tokio::spawn(async move {
        let mut iv = tokio::time::interval(KEEPALIVE_INTERVAL);
        loop {
            iv.tick().await;
            // 连接已被替换/清除 → 退出（新连接有新的 keepalive）
            {
                let guard = socket.lock().await;
                if !guard.as_ref().is_some_and(|c| Arc::ptr_eq(c, &conn)) {
                    return;
                }
            }
            let id = next_msg_id.fetch_add(1, Ordering::SeqCst);
            let msg = Message {
                id,
                kind: MsgKind::Req,
                op: "ping".to_string(),
                payload: serde_json::json!(null),
                err: None,
            };
            if let Err(e) = conn.sink.lock().await.send(Frame::Json(msg)).await {
                debug!("keepalive ping 发送失败（连接已断）：{e}");
                return;
            }
        }
    });
}

/// 发送失败/断连/超时后清除当前连接（仅当仍为该连接——避免误清新连接）。
async fn invalidate(session: &GuiSession, conn: &Arc<SharedConn>) {
    let mut guard = session.socket.lock().await;
    if guard.as_ref().is_some_and(|c| Arc::ptr_eq(c, conn)) {
        guard.take();
    }
    *session.conn_state.lock().unwrap() = ConnectionState::Unconnected;
}

/// 发送 JSON 请求并接收响应（共享 session socket）。
///
/// push-ready 模型：发送与等待响应分离——响应由 reader 任务按 msg.id 路由到
/// oneshot（[`GuiSession::pending`]），server 在请求间隙推的 Evt 事件不再
/// 干扰请求-响应配对；并发请求天然排队（写侧 Mutex 只锁单帧写入）。
pub async fn try_send_json_request(session: &GuiSession, msg: &Message) -> Result<Message> {
    let conn = ensure_connect(session).await?;

    tracing::debug!("try_send_json_request: op={} id={}", msg.op, msg.id);

    let (tx, rx) = tokio::sync::oneshot::channel();
    {
        let mut p = session.pending.lock().await;
        p.insert(msg.id, tx);
    }

    if let Err(e) = conn.sink.lock().await.send(Frame::Json(msg.clone())).await {
        session.pending.lock().await.remove(&msg.id);
        invalidate(session, &conn).await;
        return Err(e).context("发送请求失败");
    }

    let resp = match tokio::time::timeout(RESP_TIMEOUT, rx).await {
        Ok(Ok(r)) => r,
        Ok(Err(_)) => {
            // oneshot 被 drop = reader 任务退出（连接已断）
            invalidate(session, &conn).await;
            return Err(anyhow::anyhow!("等待响应时连接已断开"));
        }
        Err(_) => {
            // 超时：连接可能仍存活（server 慢）；无 30s 级 op，清连更稳
            session.pending.lock().await.remove(&msg.id);
            invalidate(session, &conn).await;
            return Err(anyhow::anyhow!(
                "等待响应超时（{}s）",
                RESP_TIMEOUT.as_secs()
            ));
        }
    };

    if let Some(ref err) = resp.err {
        tracing::warn!(
            "server op_failed: op={} code={} msg={}",
            msg.op,
            err.code,
            err.message
        );
        return Err(anyhow::anyhow!("服务器错误：{} - {}", err.code, err.message));
    }
    Ok(resp)
}

/// 发送 JSON 请求并接收响应。
///
/// 传输错（发送失败/断连/超时）时 [`try_send_json_request`] 已清除死连接，
/// 重试一次自动重连（幂等请求场景足够）。op_failed 是数据回包，不重连。
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

    tracing::debug!("send_json_request → op={} id={}", msg.op, msg.id);

    match try_send_json_request(session, &msg).await {
        Ok(resp) => Ok(resp),
        Err(e) => {
            let msg_str = e.to_string();
            if msg_str.starts_with("服务器错误") {
                tracing::warn!("send_json_request: op_failed 不重连 op={op}");
                return Err(e);
            }
            tracing::warn!("send_json_request: 传输错，重试 op={op}：{msg_str}");
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
