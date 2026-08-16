//! 连接层：单条连接的生命周期（读帧→路由→写响应;PTY 订阅退订）。
use crate::router::handle_message;
use crate::services::pty::handle_raw_data;
use crate::state::ServerState;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use easytidy_protocol::{
    Frame, MsgKind, PtyClose, PtyExited, PtyOpenResp,
};
use futures::{SinkExt, StreamExt};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_util::codec::Framed;
use tracing::{error, info, warn};

/// Handle a single connection
pub(crate) async fn handle_connection(
    stream: UnixStream,
    state: Arc<ServerState>,
) -> Result<()> {
    let mut framed = Framed::new(stream, easytidy_protocol::frame::FrameCodec::new());

    // Create channel for outgoing frames (both JSON and Raw)
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<Frame>();

    info!("Client connected");

    // Track handshake completion
    let handshake_done = Arc::new(AtomicBool::new(false));

    // 本连接打开的 PTY 流：有活跃 PTY 时跳过空闲超时（交互 shell 会长时间无输入）
    let mut conn_ptys: std::collections::HashSet<u32> = std::collections::HashSet::new();

    // 连接唯一 token（PTY 订阅退订标识；UnboundedSender 无 PartialEq）
    let conn_token = state.next_conn_id.fetch_add(1, Ordering::SeqCst);

    // 主循环包在内层函数：无论以何种方式退出（break / `?` 错误 / 超时），
    // 外层统一清理本连接打开的 PTY 会话（防 su/-bash 孤儿泄漏）
    let result = connection_loop(
        &mut framed,
        &state,
        &event_tx,
        &mut event_rx,
        &handshake_done,
        &mut conn_ptys,
        conn_token,
    )
    .await;
    close_conn_ptys(&state, &conn_ptys, conn_token).await;
    result
}

/// 单连接主循环（见 [`handle_connection`]：退出后统一清理 PTY 会话）。
pub(crate) async fn connection_loop(
    framed: &mut Framed<UnixStream, easytidy_protocol::frame::FrameCodec>,
    state: &Arc<ServerState>,
    event_tx: &mpsc::UnboundedSender<Frame>,
    event_rx: &mut mpsc::UnboundedReceiver<Frame>,
    handshake_done: &Arc<AtomicBool>,
    conn_ptys: &mut std::collections::HashSet<u32>,
    conn_token: u64,
) -> Result<()> {
    // Main connection loop
    loop {
        if state.shutting_down.load(Ordering::SeqCst) {
            info!("Server shutting down, closing connection");
            break;
        }

        // 空闲超时：无 PTY 的连接 30s，有 PTY 的放宽到 1h（PTY 存活即视为活跃）
        let idle = if conn_ptys.is_empty() {
            Duration::from_secs(30)
        } else {
            Duration::from_secs(3600)
        };

        // Wait for either incoming frame from socket or outgoing event
        tokio::select! {
            // Incoming frame from socket
            frame_result = timeout(idle, framed.next()) => {
                match frame_result {
                    Ok(Some(Ok(frame))) => {
                        match frame {
                            Frame::Json(msg) => {
                                let event_tx_clone = event_tx.clone();
                                // 提前克隆：msg 会被 move 进 handle_message
                                let req_op = msg.op.clone();
                                let req_payload = msg.payload.clone();
                                let response = handle_message(
                                    msg,
                                    state,
                                    handshake_done,
                                    event_tx_clone,
                                    conn_token,
                                ).await?;

                                if let Some(resp) = response {
                                    // pty.open 响应 → 登记本连接的 PTY；pty.close 请求 → 注销
                                    if let Frame::Json(ref rmsg) = resp {
                                        if rmsg.op == "pty.open" && rmsg.kind == MsgKind::Resp {
                                            if let Ok(open) =
                                                serde_json::from_value::<PtyOpenResp>(rmsg.payload.clone())
                                            {
                                                conn_ptys.insert(open.stream_id);
                                            }
                                        } else if req_op == "pty.close" && rmsg.kind == MsgKind::Resp {
                                            if let Ok(close) =
                                                serde_json::from_value::<PtyClose>(req_payload.clone())
                                            {
                                                conn_ptys.remove(&close.stream_id);
                                            }
                                        }
                                    }
                                    framed.send(resp).await?;
                                }
                            }
                            Frame::Raw { stream_id, data } => {
                                // Forward raw data to PTY
                                handle_raw_data(stream_id, &data, state).await?;
                            }
                        }
                    }
                    Ok(Some(Err(e))) => {
                        error!("Frame error: {}", e);
                        break;
                    }
                    Ok(None) => {
                        info!("Client disconnected");
                        break;
                    }
                    Err(_) => {
                        warn!("Connection timeout (idle)");
                        break;
                    }
                }
            }
            // Outgoing frame to send to client
            Some(frame) = event_rx.recv() => {
                // pty.exited 事件 → 注销本连接的 PTY
                if let Frame::Json(ref msg) = frame {
                    if msg.op == "pty.exited" {
                        if let Ok(ev) = serde_json::from_value::<PtyExited>(msg.payload.clone()) {
                            conn_ptys.remove(&ev.stream_id);
                        }
                    }
                }
                framed.send(frame).await?;
            }
        }
    }

    Ok(())
}

/// 连接退出时清理本连接打开的 PTY 会话。
///
/// - 非持久会话（CLI run 等）：remove → session drop → PTY writer 关闭 →
///   子进程 SIGHUP 退出 → catatonit 收割（防 su/-bash 孤儿泄漏，实测）
/// - **持久会话（attach 常驻终端）：保留——server 持有句柄，仅退订
///   本连接的输出通道**；会话由 pty.close / 自然退出终结
pub(crate) async fn close_conn_ptys(
    state: &Arc<ServerState>,
    conn_ptys: &std::collections::HashSet<u32>,
    conn_token: u64,
) {
    if conn_ptys.is_empty() {
        return;
    }
    let mut unsubscribed = 0;
    let mut to_remove = Vec::new();
    let sessions = state.sessions.read().await;
    for sid in conn_ptys {
        let Some(session) = sessions.get(sid) else {
            continue;
        };
        if session.persistent.load(Ordering::SeqCst) {
            // 常驻：退订本连接（按连接 token），会话保留继续缓冲输出
            let mut subs = session.subs.lock().unwrap();
            subs.retain(|(t, _)| *t != conn_token);
            unsubscribed += 1;
        } else {
            to_remove.push(*sid);
        }
    }
    drop(sessions);
    if !to_remove.is_empty() {
        let mut sessions = state.sessions.write().await;
        for sid in &to_remove {
            sessions.remove(sid);
        }
    }
    if unsubscribed > 0 || !to_remove.is_empty() {
        info!("连接退出：退订 {unsubscribed} 个常驻终端，清理 {} 个会话", to_remove.len());
    }
}
