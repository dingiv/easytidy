//! 容器 server socket 连接 + 握手 + one-shot op（`easytidy` 宿主 CLI 与
//! 容器内 `ets` 客户端共用）。

use anyhow::{bail, Context, Result};
use futures::{SinkExt, StreamExt};
use std::path::Path;
use tokio::net::UnixStream;
use tokio_util::codec::Framed;
use tracing::debug;

use easytidy_protocol::{
    Frame, FrameCodec, Handshake, HandshakeAck, Message, MsgKind, PROTOCOL_VERSION,
};

/// 连接容器 server socket + 握手，返回已建帧的双向流。
///
/// `client` = 握手 client 标识（"easytidy-cli" / "ets"）——server 侧连接
/// 登记表据此识别客户端（ui.edit 事件路由等）。
pub async fn connect_server(
    socket: &Path,
    client: &str,
) -> Result<Framed<UnixStream, FrameCodec>> {
    let stream = UnixStream::connect(socket)
        .await
        .with_context(|| format!("连接 socket 失败（容器可能未就绪）：{}", socket.display()))?;

    let codec = FrameCodec::new();
    let mut framed = Framed::new(stream, codec);

    // 握手
    let handshake = Handshake {
        v: PROTOCOL_VERSION,
        client: client.to_string(),
        wants: vec!["pty".to_string(), "apps".to_string()],
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
        Frame::Raw { .. } => bail!("握手响应应为 JSON 帧"),
    };

    if ack_msg.kind != MsgKind::Resp || ack_msg.op != "hello" {
        bail!("握手响应格式错误");
    }

    let ack: HandshakeAck = serde_json::from_value(ack_msg.payload).context("解析握手确认失败")?;

    debug!("握手成功：server={}, v={}", ack.server, ack.v);

    if ack.v != PROTOCOL_VERSION {
        bail!(
            "协议版本不匹配：客户端={}，服务端={}",
            PROTOCOL_VERSION,
            ack.v
        );
    }

    Ok(framed)
}

/// one-shot JSON op：发请求 → 等同 id 响应 → 返回 payload（err 响应先报错）。
/// 用于无流式交互的查询/拉起类 op（如 apps.launch_app）。
pub async fn send_json_op(
    mut framed: Framed<UnixStream, FrameCodec>,
    op: &str,
    req: &impl serde::Serialize,
) -> Result<serde_json::Value> {
    let msg = Message {
        id: 2,
        kind: MsgKind::Req,
        op: op.to_string(),
        payload: serde_json::to_value(req)?,
        err: None,
    };
    framed
        .send(Frame::Json(msg))
        .await
        .with_context(|| format!("发送 {op} 失败"))?;

    let resp_frame = framed
        .next()
        .await
        .with_context(|| format!("接收 {op} 响应失败"))?
        .with_context(|| format!("{op} 响应帧为空"))?;

    let resp_msg = match resp_frame {
        Frame::Json(msg) => msg,
        Frame::Raw { .. } => bail!("{op} 响应应为 JSON 帧（收到 Raw 帧）"),
    };
    // 错误响应优先检查（payload=null，直接 from_value 会报解析错误掩盖真实原因）
    if let Some(err) = &resp_msg.err {
        bail!("{op} 失败：{} - {}", err.code, err.message);
    }
    Ok(resp_msg.payload)
}
