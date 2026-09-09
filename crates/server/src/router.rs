//! 路由层：协议消息 → 服务分发。handler 失败转显式 error 响应（不断连）。
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use easytidy_protocol::{
    Frame,
    Handshake, HandshakeAck, Message, MsgKind, ServerEnvItem, ServerEnvResp, ServerInfoResp,
    PROTOCOL_VERSION, RpcError, UiEdit, UiEditResp,
};
use serde_json::json;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::setup::injected_env;
use crate::state::ServerState;
use crate::services::apps::{handle_apps_get_icon, handle_apps_kill, handle_apps_launch, handle_apps_launch_app, handle_apps_list, handle_apps_logs, handle_apps_ps};
use crate::services::config::{handle_config_get, handle_config_set};
use crate::services::fs::{handle_fs_copy, handle_fs_list, handle_fs_mkdir, handle_fs_read, handle_fs_stat, handle_fs_write};
use crate::services::lifecycle::{handle_lifecycle_entry_launch, handle_lifecycle_shutdown};
use crate::services::pty::{handle_pty_close, handle_pty_cwd, handle_pty_list, handle_pty_open, handle_pty_resize};
use crate::services::passthrough::{handle_passthrough_list, handle_passthrough_set};
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
    session_id: &str,
) -> Result<Option<Frame>> {
    let msg_id = msg.id;
    let msg_op = msg.op.clone();
    match dispatch(msg, state, handshake_done, event_tx, conn_token, session_id).await {
        Ok(resp) => Ok(resp),
        Err(e) => {
            // 文件/路径不存在（如 GUI 探测未安装应用的图标）是预期场景，客户端
            // 已优雅降级（AppIcon 占位）——降级为 debug，避免 ERROR 噪声；
            // 其余失败 {:#} = anyhow 全错误链（context + 根因），日志与响应一致。
            let is_not_found = e.chain().any(|cause| {
                cause
                    .downcast_ref::<std::io::Error>()
                    .map(|ie| ie.kind() == std::io::ErrorKind::NotFound)
                    .unwrap_or(false)
            });
            if is_not_found {
                debug!("{msg_op}（文件/路径不存在，客户端可忽略）：{e:#}");
            } else {
                error!("{msg_op} 处理失败：{e:#}");
            }
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
    session_id: &str,
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
            Ok(Some(
                handle_handshake(msg, state, handshake_done, event_tx, conn_token, session_id)
                    .await?,
            ))
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
        (MsgKind::Req, "apps.launch_app") => {
            Ok(Some(handle_apps_launch_app(msg, state, event_tx).await?))
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
        (MsgKind::Req, "server.env") => {
            Ok(Some(handle_server_env(msg).await?))
        }
        (MsgKind::Req, "config.get") => {
            Ok(Some(handle_config_get(msg).await?))
        }
        (MsgKind::Req, "config.set") => {
            Ok(Some(handle_config_set(msg).await?))
        }
        (MsgKind::Req, "passthrough.list") => {
            Ok(Some(handle_passthrough_list(msg, state).await?))
        }
        (MsgKind::Req, "passthrough.set") => {
            Ok(Some(handle_passthrough_set(msg, state).await?))
        }
        (MsgKind::Req, "lifecycle.entryLaunch") => {
            Ok(Some(handle_lifecycle_entry_launch(msg, state, event_tx).await?))
        }
        (MsgKind::Req, "lifecycle.shutdown") => {
            Ok(Some(handle_lifecycle_shutdown(msg, state).await?))
        }
        (MsgKind::Req, "ui.edit") => {
            Ok(Some(handle_ui_edit(msg, state).await?))
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

/// Handle handshake（并登记连接——ui.edit 事件路由的查找源）
pub(crate) async fn handle_handshake(
    msg: Message,
    state: &Arc<ServerState>,
    handshake_done: &Arc<AtomicBool>,
    event_tx: mpsc::UnboundedSender<Frame>,
    conn_token: u64,
    session_id: &str,
) -> Result<Frame> {
    let hs: Handshake = serde_json::from_value(msg.payload)
        .context("Failed to parse Handshake")?;

    info!(
        "Handshake from client '{}' (v{}, session={session_id}, wants: {:?})",
        hs.client, hs.v, hs.wants
    );

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

    // 登记连接（client/wants + 发送通道）：ui.edit 等事件据此找 GUI 连接推送；
    // 连接退出时由 handle_connection 注销。
    state.conns.write().await.insert(
        conn_token,
        Arc::new(crate::state::ConnInfo {
            client: hs.client.clone(),
            wants: hs.wants.clone(),
            events: event_tx.clone(),
        }),
    );

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
        session_id: session_id.to_string(),
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

/// Handle server.info（容器默认用户 home 等）
pub(crate) async fn handle_server_info(msg: Message) -> Result<Frame> {
    Ok(Frame::Json(Message {
        id: msg.id,
        kind: MsgKind::Resp,
        op: "server.info".to_string(),
        payload: serde_json::to_value(ServerInfoResp {
            // 容器默认用户 home（server 自身身份；setup 后恒有值，防御性 unwrap_or_default）
            home_dir: crate::setup::user_map().map(|u| u.home.clone()).unwrap_or_default(),
        })?,
        err: None,
    }))
}

/// Handle server.env（server 运行时注入的环境变量：XAUTHORITY 探测 /
/// XDG_DATA_DIRS 修正——配置定义不了、宿主无法预知，配置管理器展示为只读行）。
pub(crate) async fn handle_server_env(msg: Message) -> Result<Frame> {
    let env = injected_env()
        .iter()
        .map(|e| ServerEnvItem {
            key: e.key.to_string(),
            value: e.value.clone(),
            note: e.note.to_string(),
        })
        .collect::<Vec<_>>();
    Ok(Frame::Json(Message {
        id: msg.id,
        kind: MsgKind::Resp,
        op: "server.env".to_string(),
        payload: serde_json::to_value(ServerEnvResp { env })?,
        err: None,
    }))
}

/// Handle ui.edit：把编辑请求事件推到存活的 GUI 连接。
///
/// GUI 连接 = client "easytidy-gui" 且握手 wants 含 "events"（新 GUI 共享
/// socket 声明；PTY 专用连接不声明、不会收到）。多 GUI 窗口 → 取 conn_token
/// 最小（最先连接）的一个，避免多窗口重复开编辑器。无 GUI → routed:false，
/// 调用方（容器内 ets）自行回退本地编辑器。
pub(crate) async fn handle_ui_edit(msg: Message, state: &Arc<ServerState>) -> Result<Frame> {
    let req: UiEdit = serde_json::from_value(msg.payload).context("Failed to parse UiEdit")?;
    if req.path.trim().is_empty() {
        return Err(anyhow::anyhow!("ui.edit: path 不能为空"));
    }

    let conns = state.conns.read().await;
    let target = conns
        .iter()
        .filter(|(_, c)| c.client == "easytidy-gui" && c.wants.iter().any(|w| w == "events"))
        .min_by_key(|(token, _)| *token)
        .map(|(_, c)| c.clone());
    drop(conns);

    match target {
        Some(conn) => {
            let evt = Frame::Json(Message {
                id: state.next_msg_id.fetch_add(1, Ordering::SeqCst) as u64,
                kind: MsgKind::Evt,
                op: "ui.edit".to_string(),
                payload: serde_json::to_value(&req)?,
                err: None,
            });
            conn.events.send(evt).map_err(|e| {
                anyhow::anyhow!("ui.edit: 向 GUI 推送事件失败（连接已断？）：{e}")
            })?;
            info!("ui.edit: 已推事件到 GUI 连接：{}", req.path);
            Ok(Frame::Json(Message {
                id: msg.id,
                kind: MsgKind::Resp,
                op: "ui.edit".to_string(),
                payload: serde_json::to_value(UiEditResp {
                    routed: true,
                    target: "gui".to_string(),
                })?,
                err: None,
            }))
        }
        None => {
            debug!("ui.edit: 无存活 GUI 连接，回 none（调用方回退本地编辑器）");
            Ok(Frame::Json(Message {
                id: msg.id,
                kind: MsgKind::Resp,
                op: "ui.edit".to_string(),
                payload: serde_json::to_value(UiEditResp {
                    routed: false,
                    target: "none".to_string(),
                })?,
                err: None,
            }))
        }
    }
}
