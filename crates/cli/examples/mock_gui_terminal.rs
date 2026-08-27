// 模拟 GUI 多终端: Mount 1 持久 + Mount 2 attach,Mount 1 主动写数据模拟用户敲键。
use easytidy_core::host_socket_path;
use easytidy_protocol::{
    Frame, FrameCodec, Handshake, Message, MsgKind, PtyOpen, PROTOCOL_VERSION,
};
use futures::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio_util::codec::Framed;

async fn recv_json<S>(f: &mut S) -> anyhow::Result<Message>
where
    S: StreamExt<Item = Result<Frame, std::io::Error>> + Unpin,
{
    match f.next().await {
        Some(Ok(Frame::Json(m))) => Ok(m),
        Some(Ok(_)) => anyhow::bail!("expected JSON frame"),
        Some(Err(e)) => anyhow::bail!("frame error: {e}"),
        None => anyhow::bail!("stream ended"),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let container = std::env::args().nth(1).unwrap_or_else(|| "chrome".into());
    let socket = host_socket_path(&container)?;
    eprintln!("[MOCK] socket = {}", socket.display());

    // === Mount 1: open persistent ===
    let stream1 = UnixStream::connect(&socket).await?;
    let framed1 = Framed::new(stream1, FrameCodec::new());
    let (mut m1_sink, mut m1_stream) = framed1.split();
    let hello = Handshake {
        v: PROTOCOL_VERSION,
        client: "mock-gui-mount1".into(),
        wants: vec!["pty".into()],
    };
    m1_sink
        .send(Frame::Json(Message {
            id: 1,
            kind: MsgKind::Req,
            op: "hello".into(),
            payload: serde_json::to_value(&hello)?,
            err: None,
        }))
        .await?;
    let _ = recv_json(&mut m1_stream).await?;
    let open_req = PtyOpen {
        cmd: String::new(),
        argv: vec![],
        env: HashMap::new(),
        cwd: "/".into(),
        cols: 80,
        rows: 24,
        attach: false,
        persistent: true,
        attach_stream: None,
    };
    m1_sink
        .send(Frame::Json(Message {
            id: 2,
            kind: MsgKind::Req,
            op: "pty.open".into(),
            payload: serde_json::to_value(&open_req)?,
            err: None,
        }))
        .await?;
    let resp = recv_json(&mut m1_stream).await?;
    let sid = resp.payload["stream_id"].as_u64().unwrap() as u32;
    eprintln!("[MOCK-M1] pty.open sid={}", sid);

    let m1_frames = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let m1_bytes = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let m1_frames_c = m1_frames.clone();
    let m1_bytes_c = m1_bytes.clone();
    let m1_task = tokio::spawn(async move {
        while let Some(item) = m1_stream.next().await {
            match item {
                Ok(Frame::Raw { stream_id, data }) => {
                    m1_frames_c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    m1_bytes_c.fetch_add(data.len() as u64, std::sync::atomic::Ordering::Relaxed);
                    let preview = String::from_utf8_lossy(&data[..data.len().min(60)]);
                    eprintln!(
                        "[MOCK-M1] frame: sid={} bytes={} preview={:?}",
                        stream_id, data.len(), preview
                    );
                }
                Ok(Frame::Json(m)) => eprintln!("[MOCK-M1] json: op={}", m.op),
                Err(e) => {
                    eprintln!("[MOCK-M1] err: {e}");
                    break;
                }
            }
        }
        eprintln!("[MOCK-M1] stream ended");
    });

    tokio::time::sleep(Duration::from_millis(500)).await;

    // === Mount 2: attach ===
    let stream2 = UnixStream::connect(&socket).await?;
    let framed2 = Framed::new(stream2, FrameCodec::new());
    let (mut m2_sink, mut m2_stream) = framed2.split();
    let hello2 = Handshake {
        v: PROTOCOL_VERSION,
        client: "mock-gui-mount2".into(),
        wants: vec!["pty".into()],
    };
    m2_sink
        .send(Frame::Json(Message {
            id: 1,
            kind: MsgKind::Req,
            op: "hello".into(),
            payload: serde_json::to_value(&hello2)?,
            err: None,
        }))
        .await?;
    let _ = recv_json(&mut m2_stream).await?;
    let attach_req = PtyOpen {
        cmd: String::new(),
        argv: vec![],
        env: HashMap::new(),
        cwd: "/".into(),
        cols: 80,
        rows: 24,
        attach: false,
        persistent: true,
        attach_stream: Some(sid),
    };
    m2_sink
        .send(Frame::Json(Message {
            id: 2,
            kind: MsgKind::Req,
            op: "pty.open".into(),
            payload: serde_json::to_value(&attach_req)?,
            err: None,
        }))
        .await?;
    let resp2 = recv_json(&mut m2_stream).await?;
    eprintln!("[MOCK-M2] attach resp sid={:?}", resp2.payload["stream_id"]);

    let m2_frames = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let m2_bytes = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let m2_frames_c = m2_frames.clone();
    let m2_bytes_c = m2_bytes.clone();
    let m2_task = tokio::spawn(async move {
        let mut replay_seen = false;
        while let Some(item) = m2_stream.next().await {
            match item {
                Ok(Frame::Raw { stream_id, data }) => {
                    m2_frames_c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    m2_bytes_c.fetch_add(data.len() as u64, std::sync::atomic::Ordering::Relaxed);
                    let tag = if !replay_seen { "REPLAY" } else { "LIVE" };
                    let preview = String::from_utf8_lossy(&data[..data.len().min(60)]);
                    eprintln!(
                        "[MOCK-M2] {} frame: sid={} bytes={} preview={:?}",
                        tag, stream_id, data.len(), preview
                    );
                    replay_seen = true;
                }
                Ok(Frame::Json(m)) => eprintln!("[MOCK-M2] json: op={}", m.op),
                Err(e) => {
                    eprintln!("[MOCK-M2] err: {e}");
                    break;
                }
            }
        }
        eprintln!("[MOCK-M2] stream ended");
    });

    tokio::time::sleep(Duration::from_millis(500)).await;

    // 模拟用户敲键
    eprintln!("[MOCK-M1] >>> sending 'echo hello\\n'");
    m1_sink
        .send(Frame::Raw {
            stream_id: sid,
            data: b"echo hello\n".to_vec(),
        })
        .await?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    eprintln!("[MOCK-M1] >>> sending 'pwd\\n'");
    m1_sink
        .send(Frame::Raw {
            stream_id: sid,
            data: b"pwd\n".to_vec(),
        })
        .await?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    let m1_f = m1_frames.load(std::sync::atomic::Ordering::Relaxed);
    let m1_b = m1_bytes.load(std::sync::atomic::Ordering::Relaxed);
    let m2_f = m2_frames.load(std::sync::atomic::Ordering::Relaxed);
    let m2_b = m2_bytes.load(std::sync::atomic::Ordering::Relaxed);
    eprintln!(
        "[MOCK] FINAL: M1 frames={} bytes={}; M2 frames={} bytes={}",
        m1_f, m1_b, m2_f, m2_b
    );

    drop(m1_sink);
    drop(m2_sink);
    let _ = m1_task.await;
    let _ = m2_task.await;
    Ok(())
}
