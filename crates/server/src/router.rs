//! 路由层：协议消息 → 服务分发。handler 失败转显式 error 响应（不断连）。
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use easytidy_protocol::{
    Frame,
    Handshake, HandshakeAck, Message, MsgKind, ServerInfoResp,
    PROTOCOL_VERSION, RpcError,
};
use serde_json::json;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::http::HTTP_PORT;
use crate::state::ServerState;
use crate::services::apps::{handle_apps_get_icon, handle_apps_kill, handle_apps_launch, handle_apps_list, handle_apps_logs, handle_apps_ps};
use crate::services::config::{handle_config_get, handle_config_set};
use crate::services::fs::{handle_fs_copy, handle_fs_list, handle_fs_mkdir, handle_fs_read, handle_fs_stat, handle_fs_write};
use crate::services::lifecycle::{handle_lifecycle_entry_launch, handle_lifecycle_shutdown};
use crate::services::pty::{handle_pty_close, handle_pty_cwd, handle_pty_list, handle_pty_open, handle_pty_resize};
/// Handle a JSON message
/// 消息入口：handler 失败转为显式 error 响应（带 anyhow 全链 {:#}），
/// 不再断连——客户端原本就解析 err 字段，此前 Err 直接冒泡到连接层断开，
/// 用户只能看到"接收响应失败"而拿不到真实原因（如 spawn: No such file）。
pub(crate) async fn handle_message(
    msg: Message,
    state: &Arc<ServerState>,
    handshake_done: &Arc<AtomicBool>,
    event_tx: mpsc::UnboundedSender<Frame>,
    conn_token: u64,
) -> Result<Option<Frame>> {
    let msg_id = msg.id;
    let msg_op = msg.op.clone();
    match dispatch(msg, state, handshake_done, event_tx, conn_token).await {
        Ok(resp) => Ok(resp),
        Err(e) => {
            // {:#} = anyhow 全错误链（context + 根因），日志与响应一致
            error!("{msg_op} 处理失败：{e:#}");
            Ok(Some(Frame::Json(Message {
                id: msg_id,
                kind: MsgKind::Resp,
                op: msg_op,
                payload: json!(null),
                err: Some(RpcError {
                    code: "op_failed".to_string(),
                    message: format!("{e:#}"),
                }),
            })))
        }
    }
}

pub(crate) async fn dispatch(
    msg: Message,
    state: &Arc<ServerState>,
    handshake_done: &Arc<AtomicBool>,
    event_tx: mpsc::UnboundedSender<Frame>,
    conn_token: u64,
) -> Result<Option<Frame>> {
    // Require handshake first
    if msg.op != "hello" && !handshake_done.load(Ordering::SeqCst) {
        return Ok(Some(Frame::Json(Message {
            id: msg.id,
            kind: MsgKind::Resp,
            op: msg.op,
            payload: json!(null),
            err: Some(RpcError {
                code: "not_handshaked".to_string(),
                message: "Must handshake first".to_string(),
            }),
        })));
    }
    match (msg.kind, msg.op.as_str()) {
        (MsgKind::Req, "hello") => {
            Ok(Some(handle_handshake(msg, handshake_done).await?))
        }
        (MsgKind::Req, "ping") => {
            Ok(Some(handle_ping(msg).await?))
        }
        (MsgKind::Req, "pty.open") => {
            Ok(Some(handle_pty_open(msg, state, event_tx, conn_token).await?))
        }
        (MsgKind::Req, "pty.resize") => {
            Ok(Some(handle_pty_resize(msg, state).await?))
        }
        (MsgKind::Req, "pty.close") => {
            Ok(Some(handle_pty_close(msg, state).await?))
        }
        (MsgKind::Req, "pty.cwd") => {
            Ok(Some(handle_pty_cwd(msg, state).await?))
        }
        (MsgKind::Req, "pty.list") => {
            Ok(Some(handle_pty_list(msg, state).await?))
        }
        (MsgKind::Req, "fs.list") => {
            Ok(Some(handle_fs_list(msg).await?))
        }
        (MsgKind::Req, "fs.stat") => {
            Ok(Some(handle_fs_stat(msg).await?))
        }
        (MsgKind::Req, "fs.read") => {
            Ok(Some(handle_fs_read(msg).await?))
        }
        (MsgKind::Req, "fs.write") => {
            Ok(Some(handle_fs_write(msg).await?))
        }
        (MsgKind::Req, "fs.copy") => {
            Ok(Some(handle_fs_copy(msg).await?))
        }
        (MsgKind::Req, "fs.mkdir") => {
            Ok(Some(handle_fs_mkdir(msg).await?))
        }
        (MsgKind::Req, "apps.list") => {
            Ok(Some(handle_apps_list(msg).await?))
        }
        (MsgKind::Req, "apps.getIcon") => {
            Ok(Some(handle_apps_get_icon(msg).await?))
        }
        (MsgKind::Req, "apps.launch") => {
            Ok(Some(handle_apps_launch(msg, state, event_tx).await?))
        }
        (MsgKind::Req, "apps.ps") => {
            Ok(Some(handle_apps_ps(msg, state).await?))
        }
        (MsgKind::Req, "apps.logs") => {
            Ok(Some(handle_apps_logs(msg, state).await?))
        }
        (MsgKind::Req, "apps.kill") => {
            Ok(Some(handle_apps_kill(msg, state).await?))
        }
        (MsgKind::Req, "server.info") => {
            Ok(Some(handle_server_info(msg).await?))
        }
        (MsgKind::Req, "config.get") => {
            Ok(Some(handle_config_get(msg).await?))
        }
        (MsgKind::Req, "config.set") => {
            Ok(Some(handle_config_set(msg).await?))
        }
        (MsgKind::Req, "lifecycle.entryLaunch") => {
            Ok(Some(handle_lifecycle_entry_launch(msg, state, event_tx).await?))
        }
        (MsgKind::Req, "lifecycle.shutdown") => {
            Ok(Some(handle_lifecycle_shutdown(msg, state).await?))
        }
        (MsgKind::Req, op) => {
            warn!("Unknown operation: {}", op);
            Ok(Some(Frame::Json(Message {
                id: msg.id,
                kind: MsgKind::Resp,
                op: op.to_string(),
                payload: json!(null),
                err: Some(RpcError {
                    code: "unknown_op".to_string(),
                    message: format!("Unknown operation: {}", op),
                }),
            })))
        }
        _ => {
            // Ignore non-requests or unhandled
            Ok(None)
        }
    }
}

/// Handle handshake
pub(crate) async fn handle_handshake(
    msg: Message,
    handshake_done: &Arc<AtomicBool>,
) -> Result<Frame> {
    let hs: Handshake = serde_json::from_value(msg.payload)
        .context("Failed to parse Handshake")?;

    info!("Handshake from client '{}' (v{}, wants: {:?})", hs.client, hs.v, hs.wants);

    if hs.v != PROTOCOL_VERSION {
        return Ok(Frame::Json(Message {
            id: msg.id,
            kind: MsgKind::Resp,
            op: "hello".to_string(),
            payload: json!(null),
            err: Some(RpcError {
                code: "version_mismatch".to_string(),
                message: format!("Protocol version mismatch: client={}, server={}", hs.v, PROTOCOL_VERSION),
            }),
        }));
    }

    handshake_done.store(true, Ordering::SeqCst);

    let ack = HandshakeAck {
        v: PROTOCOL_VERSION,
        server: "easytidy-server".to_string(),
        capabilities: vec![
            "pty".to_string(),
            "fs".to_string(),
            "apps".to_string(),
            "config".to_string(),
            "lifecycle".to_string(),
        ],
    };

    Ok(Frame::Json(Message {
        id: msg.id,
        kind: MsgKind::Resp,
        op: "hello".to_string(),
        payload: serde_json::to_value(ack)?,
        err: None,
    }))
}

/// Handle ping
pub(crate) async fn handle_ping(msg: Message) -> Result<Frame> {
    Ok(Frame::Json(Message {
        id: msg.id,
        kind: MsgKind::Resp,
        op: "ping".to_string(),
        payload: msg.payload,
        err: None,
    }))
}

/// Handle server.info（HTTP 静态托管端口等）
pub(crate) async fn handle_server_info(msg: Message) -> Result<Frame> {
    Ok(Frame::Json(Message {
        id: msg.id,
        kind: MsgKind::Resp,
        op: "server.info".to_string(),
        payload: serde_json::to_value(ServerInfoResp {
            http_port: HTTP_PORT.load(std::sync::atomic::Ordering::SeqCst),
        })?,
        err: None,
    }))
}
