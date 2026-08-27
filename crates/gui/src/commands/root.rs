//! root 终端命令（宿主 root 通道：`easytidy-root-channel` 进程）。
//!
//! root 终端**不走**容器 server：宿主 rootless socket `exec --user 0`
//! 持有每容器一个**共享** root shell（容器内父 = conmon），多客户端
//! attach/detach + 128KB 回放（2026 同步包裹）。detach = 仅退订，
//! 会话存活至容器销毁或 `root_terminal_close`。
//!
//! 与 pty.rs 同构：每条 attach 一条专用连接，写侧存 `GuiSession.root_sink`
//! （单容器单共享会话）。前端契约：root 面板是单例（无 stream_id 概念，
//! 流 ID 恒为 `ROOT_STREAM_ID`）。

use futures::{SinkExt, StreamExt};
use std::sync::Arc;

use easytidy_protocol::rc::{
    RcAttach, RcClose, RcPing, RcPingResp, RcResize, ROOT_STREAM_ID,
};
use easytidy_protocol::{Frame, FrameCodec, Handshake, HandshakeAck, Message, MsgKind, PROTOCOL_VERSION};
use tokio::net::{UnixStream};
use tokio_util::codec::Framed;

use crate::state::{GuiSession, PtyEvent};

/// 连接 root 通道（按需拉起进程 → connect → hello 握手）。
///
/// 与 `socket::connect_to_container`（容器 server）同构，但目标是宿主侧
/// `$XDG_RUNTIME_DIR/easytidy-root/<name>.sock`。
async fn connect_root_channel(container_name: &str) -> anyhow::Result<Framed<UnixStream, FrameCodec>> {
    let socket = easytidy_core::root_channel::ensure_running(container_name)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let stream = UnixStream::connect(&socket)
        .await
        .map_err(|e| anyhow::anyhow!("连接 root 通道 socket 失败（{e}）：{}", socket.display()))?;

    let mut framed = Framed::new(stream, FrameCodec::new());

    let handshake = Handshake {
        v: PROTOCOL_VERSION,
        client: "easytidy-gui".to_string(),
        wants: vec!["root".to_string()],
    };
    framed
        .send(Frame::Json(Message {
            id: 1,
            kind: MsgKind::Req,
            op: "hello".to_string(),
            payload: serde_json::to_value(handshake)?,
            err: None,
        }))
        .await
        .map_err(|e| anyhow::anyhow!("发送 root 通道握手失败：{e}"))?;

    let ack_frame = framed
        .next()
        .await
        .ok_or_else(|| anyhow::anyhow!("root 通道握手确认帧为空"))?
        .map_err(|e| anyhow::anyhow!("接收 root 通道握手确认失败：{e}"))?;
    let Frame::Json(ack_msg) = ack_frame else {
        anyhow::bail!("root 通道握手响应应为 JSON 帧");
    };
    if ack_msg.kind != MsgKind::Resp || ack_msg.op != "hello" {
        anyhow::bail!("root 通道握手响应格式错误");
    }
    let ack: HandshakeAck = serde_json::from_value(ack_msg.payload)?;
    tracing::debug!("root 通道握手成功：server={}, v={}, session={}", ack.server, ack.v, ack.session_id);

    Ok(framed)
}

/// 确保 root 通道在运行（按需拉起）。容器未运行 / 通道起不来时报错
/// （前端据此提示「容器未运行」）。
#[tauri::command]
pub async fn root_channel_ensure(
    session: tauri::State<'_, Option<GuiSession>>,
) -> Result<String, String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    let socket = easytidy_core::root_channel::ensure_running(&sess.container_name)
        .await
        .map_err(|e| e.to_string())?;
    Ok(socket.display().to_string())
}

/// 探测共享 root 会话是否存活（一次性连接：hello + rc.ping 即断）。
///
/// 未运行（socket 不存在）= false（非错误：root 终端按需拉起）。
#[tauri::command]
pub async fn root_terminal_status(
    session: tauri::State<'_, Option<GuiSession>>,
) -> Result<bool, String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    let name = sess.container_name.clone();

    // 未运行 → 直接 false（不拉起进程：状态探测是只读语义）
    let socket = match easytidy_core::root_channel::root_channel_socket_path(&name) {
        Ok(p) => p,
        Err(e) => return Err(e.to_string()),
    };
    let stream = match tokio::net::UnixStream::connect(&socket).await {
        Ok(s) => s,
        Err(_) => return Ok(false),
    };

    let mut framed = Framed::new(stream, FrameCodec::new());
    framed
        .send(Frame::Json(Message {
            id: 1,
            kind: MsgKind::Req,
            op: "hello".to_string(),
            payload: serde_json::to_value(Handshake {
                v: PROTOCOL_VERSION,
                client: "easytidy-gui".to_string(),
                wants: vec!["root".to_string()],
            })
            .map_err(|e| e.to_string())?,
            err: None,
        }))
        .await
        .map_err(|e| e.to_string())?;
    let Frame::Json(_) = framed
        .next()
        .await
        .ok_or_else(|| "root 通道握手确认帧为空".to_string())?
        .map_err(|e| e.to_string())?
    else {
        return Err("root 通道握手响应应为 JSON 帧".to_string());
    };

    framed
        .send(Frame::Json(Message {
            id: 2,
            kind: MsgKind::Req,
            op: "rc.ping".to_string(),
            payload: serde_json::to_value(RcPing).map_err(|e| e.to_string())?,
            err: None,
        }))
        .await
        .map_err(|e| e.to_string())?;
    let Frame::Json(resp) = framed
        .next()
        .await
        .ok_or_else(|| "rc.ping 响应为空".to_string())?
        .map_err(|e| e.to_string())?
    else {
        return Err("rc.ping 响应应为 JSON 帧".to_string());
    };
    if let Some(err) = resp.err {
        return Err(format!("{} {}", err.code, err.message));
    }
    let r: RcPingResp = serde_json::from_value(resp.payload).map_err(|e| format!("解析 rc.ping 响应失败：{e}"))?;
    Ok(r.alive)
}

/// 挂载共享 root 会话（专用连接：rc.attach → 回放 + 流式输出经 Channel）。
///
/// 幂等：多面板/重连 attach 同一会话只 fan-out 输出，不新建 shell。
/// 替换旧 attach（旧连接 drop = detach，仅退订）。
#[tauri::command]
pub async fn root_terminal_attach(
    session: tauri::State<'_, Option<GuiSession>>,
    on_event: tauri::ipc::Channel<PtyEvent>,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    let container_name = sess.container_name.clone();

    let mut framed = connect_root_channel(&container_name).await.map_err(|e| e.to_string())?;

    framed
        .send(Frame::Json(Message {
            id: 1,
            kind: MsgKind::Req,
            op: "rc.attach".to_string(),
            payload: serde_json::to_value(RcAttach { cols, rows }).map_err(|e| e.to_string())?,
            err: None,
        }))
        .await
        .map_err(|e| format!("发送 rc.attach 失败：{e}"))?;

    let resp = framed
        .next()
        .await
        .ok_or_else(|| "rc.attach 响应为空".to_string())?
        .map_err(|e| e.to_string())?;
    let Frame::Json(resp_msg) = resp else {
        return Err("rc.attach 响应应为 JSON 帧".to_string());
    };
    if let Some(err) = resp_msg.err {
        return Err(format!("rc.attach 失败：{} {}", err.code, err.message));
    }
    if let Ok(ack) = serde_json::from_value::<easytidy_protocol::rc::RcAttachAck>(resp_msg.payload) {
        if !ack.alive {
            return Err("root 会话已死（容器未运行或 shell 已退出）".to_string());
        }
    }

    let (sink, stream) = framed.split();
    let sink = Arc::new(tokio::sync::Mutex::new(sink));

    // 替换旧 attach（旧连接 drop = detach；旧 reader 随其流 EOF 退出）
    {
        let mut guard = sess.root_sink.lock().await;
        *guard = Some(sink.clone());
    }

    let root_sink = Arc::clone(&sess.root_sink);
    tokio::spawn(async move {
        let mut stream = stream;
        loop {
            match stream.next().await {
                Some(Ok(Frame::Raw { stream_id, data })) => {
                    if stream_id == ROOT_STREAM_ID && !data.is_empty() {
                        let event = PtyEvent {
                            kind: "data".to_string(),
                            data: Some(data),
                            code: None,
                            cwd: None,
                        };
                        if on_event.send(event).is_err() {
                            break; // 通道关闭（面板卸载）
                        }
                    }
                }
                Some(Ok(Frame::Json(msg))) => {
                    if msg.op == "rc.exited" {
                        let _ = on_event.send(PtyEvent {
                            kind: "exited".to_string(),
                            data: None,
                            code: Some(0),
                            cwd: None,
                        });
                        break;
                    }
                    // ping 等其它 JSON 忽略
                }
                Some(Err(e)) => {
                    tracing::warn!("root 通道流读取错误：{e}");
                    break;
                }
                None => {
                    tracing::warn!("root 通道流关闭（root-channel 退出）");
                    break;
                }
            }
        }
        // 本连接是最后持有者才清（并发 attach 时由新 attach 覆盖；
        // 此处覆盖式清除——会话已死，新 attach 也连不上）
        *root_sink.lock().await = None;
    });

    Ok(())
}

/// 向共享 root 会话写入输入（Raw 帧，stream_id = ROOT_STREAM_ID）。
/// 未 attach（root_sink 空）→ 报错（前端据此重新 attach）。
#[tauri::command]
pub async fn root_terminal_write(
    session: tauri::State<'_, Option<GuiSession>>,
    data: Vec<u8>,
) -> Result<(), String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;

    let sink = {
        let guard = sess.root_sink.lock().await;
        guard.as_ref().cloned()
    }
    .ok_or_else(|| "root 会话未挂载（先 attach）".to_string())?;

    sink.lock()
        .await
        .send(Frame::Raw { stream_id: ROOT_STREAM_ID, data })
        .await
        .map_err(|e| format!("root 会话写入失败：{e}"))?;
    Ok(())
}

/// 调整共享 root 会话 TTY 尺寸（rc.resize → root-channel 转发 resize_exec）。
#[tauri::command]
pub async fn root_terminal_resize(
    session: tauri::State<'_, Option<GuiSession>>,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;

    let sink = {
        let guard = sess.root_sink.lock().await;
        guard.as_ref().cloned()
    }
    .ok_or_else(|| "root 会话未挂载（先 attach）".to_string())?;

    sink.lock()
        .await
        .send(Frame::Json(Message {
            id: 2,
            kind: MsgKind::Req,
            op: "rc.resize".to_string(),
            payload: serde_json::to_value(RcResize { cols, rows }).map_err(|e| e.to_string())?,
            err: None,
        }))
        .await
        .map_err(|e| format!("root 会话 resize 失败：{e}"))?;
    Ok(())
}

/// 主动关闭共享 root 会话（rc.close → kill 容器内 root shell → root-channel 退出）。
#[tauri::command]
pub async fn root_terminal_close(
    session: tauri::State<'_, Option<GuiSession>>,
) -> Result<(), String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;

    let sink = {
        let guard = sess.root_sink.lock().await;
        guard.as_ref().cloned()
    };
    match sink {
        Some(sink) => {
            sink.lock()
                .await
                .send(Frame::Json(Message {
                    id: 3,
                    kind: MsgKind::Req,
                    op: "rc.close".to_string(),
                    payload: serde_json::to_value(RcClose).map_err(|e| e.to_string())?,
                    err: None,
                }))
                .await
                .map_err(|e| format!("root 会话关闭失败：{e}"))?;
        }
        None => {
            // 本地无句柄（已 detach / root-channel 已退）→ 视为已关闭
            tracing::debug!("root_terminal_close：本地无 root 会话句柄（视为已关闭）");
        }
    }
    *sess.root_sink.lock().await = None;
    Ok(())
}
