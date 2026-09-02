//! root-channel client 模式（连 daemon + 桥 stdio 到 session 流）。
//!
//! 调用入口：宿主 GUI/CLI → `podman exec --user 0 -it <container> /run/easytidy-bin/easytidy-root-channel --client {new|attach <sid>}`
//! 运行身份：容器内 root（因 exec --user 0；stdin/stdout 由 podman exec 接到 client 进程）
//!
//! 行为：
//! 1. 连 daemon socket
//! 2. 握手 hello
//! 3. 发命令：new（建新 session）或 attach <sid>（attach 现有 session）
//! 4. 接收 ack（new: session_id；attach: stream_id）
//! 5. 桥流：stdin → Raw 帧到 daemon；daemon 输出 → stdout；SIGWINCH → rc.resize
//! 6. 进程退出 = detach（session 保留在 daemon，下次 attach 续）

use std::os::unix::io::AsRawFd;

use anyhow::Context;
use clap::ValueEnum;
use futures::{SinkExt, StreamExt};
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio_util::codec::Framed;
use tracing::{debug, info};

use easytidy_protocol::frame::FrameCodec;
use easytidy_protocol::rc::{
    RcAttach, RcAttachAck, RcCloseReq, RcNew, RcNewAck, RcPing, RcPingResp, RcResize,
    ROOT_STREAM_ID,
};
use easytidy_protocol::{Frame, Handshake, HandshakeAck, Message, MsgKind, PROTOCOL_VERSION};

use crate::daemon::DAEMON_SOCKET;

/// client 子命令
#[derive(Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum ClientCmd {
    /// 建新 root bash session
    New,
    /// attach 到指定 session_id
    Attach,
    /// 仅探测存活（不订阅，发完即断）
    Ping,
    /// 列出所有 session（发完即断）
    List,
    /// 关闭指定 session（kill bash，发完即断）
    Close,
}

pub struct ClientArgs {
    pub cmd: ClientCmd,
    /// attach 时的 session_id
    pub session_id: Option<u64>,
    /// 终端列数
    pub cols: u16,
    /// 终端行数
    pub rows: u16,
}

pub async fn run_client(args: ClientArgs) -> anyhow::Result<()> {
    tracing::info!("client connecting to {}", DAEMON_SOCKET);
    let stream = UnixStream::connect(DAEMON_SOCKET)
        .await
        .with_context(|| format!("connect daemon socket {} failed", DAEMON_SOCKET))?;
    let mut framed = Framed::new(stream, FrameCodec::new());
    tracing::info!("client connected to daemon");

    // 握手
    let hs = Handshake {
        v: PROTOCOL_VERSION,
        client: "easytidy-root-channel-client".into(),
        wants: vec!["root".into()],
    };
    framed
        .send(Frame::Json(Message {
            id: 1,
            kind: MsgKind::Req,
            op: "hello".into(),
            payload: serde_json::to_value(hs)?,
            err: None,
        }))
        .await
        .context("send hello")?;
    let ack_frame = framed
        .next()
        .await
        .ok_or_else(|| anyhow::anyhow!("hello ack empty"))??;
    let Frame::Json(ack_msg) = ack_frame else {
        anyhow::bail!("hello ack should be JSON");
    };
    if ack_msg.kind != MsgKind::Resp || ack_msg.op != "hello" {
        anyhow::bail!("hello ack format error");
    }
    let _ack: HandshakeAck = serde_json::from_value(ack_msg.payload)?;
    debug!("hello ack ok");

    // 命令
    let (op, payload): (String, serde_json::Value) = match args.cmd {
        ClientCmd::Ping => (
            "rc.ping".into(),
            serde_json::to_value(RcPing)?,
        ),
        ClientCmd::List => (
            "rc.list".into(),
            serde_json::Value::Null,
        ),
        ClientCmd::Close => {
            let sid = args
                .session_id
                .ok_or_else(|| anyhow::anyhow!("close requires session_id"))?;
            (
                "rc.close".into(),
                serde_json::to_value(RcCloseReq { session_id: sid })?,
            )
        }
        ClientCmd::New => (
            "rc.new".into(),
            serde_json::to_value(RcNew {
                cols: args.cols,
                rows: args.rows,
            })?,
        ),
        ClientCmd::Attach => {
            let sid = args
                .session_id
                .ok_or_else(|| anyhow::anyhow!("attach requires session_id"))?;
            (
                "rc.attach".into(),
                serde_json::to_value(RcAttach {
                    cols: args.cols,
                    rows: args.rows,
                    session_id: sid,
                })?,
            )
        }
    };
    tracing::info!("client sending op={op}");

    let id = match args.cmd {
        ClientCmd::Ping => 2,
        ClientCmd::List => 2,
        ClientCmd::Close => 2,
        ClientCmd::New => 2,
        ClientCmd::Attach => 2,
    };
    framed
        .send(Frame::Json(Message {
            id,
            kind: MsgKind::Req,
            op: op.clone(),
            payload: payload.clone(),
            err: None,
        }))
        .await
        .context("send command")?;

    if matches!(args.cmd, ClientCmd::Ping) {
        // ping：读 ack，输出 alive，退出
        let resp = framed
            .next()
            .await
            .ok_or_else(|| anyhow::anyhow!("ping resp empty"))??;
        let Frame::Json(msg) = resp else {
            anyhow::bail!("ping resp should be JSON");
        };
        let r: RcPingResp = serde_json::from_value(msg.payload)?;
        println!("alive: {}", r.alive);
        return Ok(());
    }

    if matches!(args.cmd, ClientCmd::List) {
        // list：读 ack，输出 JSON（GUI root_session_list 解析），退出
        let resp = framed
            .next()
            .await
            .ok_or_else(|| anyhow::anyhow!("list resp empty"))??;
        let Frame::Json(msg) = resp else {
            anyhow::bail!("list resp should be JSON");
        };
        if let Some(err) = msg.err {
            anyhow::bail!("rc.list failed: {} {}", err.code, err.message);
        }
        // 直接透传 payload JSON 到 stdout
        println!("{}", msg.payload);
        return Ok(());
    }

    if matches!(args.cmd, ClientCmd::Close) {
        // close：读 ack，退出（session 已被 daemon kill）
        let resp = framed
            .next()
            .await
            .ok_or_else(|| anyhow::anyhow!("close resp empty"))??;
        let Frame::Json(msg) = resp else {
            anyhow::bail!("close resp should be JSON");
        };
        if let Some(err) = msg.err {
            anyhow::bail!("rc.close failed: {} {}", err.code, err.message);
        }
        tracing::info!("session closed");
        return Ok(());
    }

    // new/attach：读 ack（含 stream_id）+ 进入桥流
    let resp = framed
        .next()
        .await
        .ok_or_else(|| anyhow::anyhow!("command resp empty"))??;
    let Frame::Json(msg) = resp else {
        anyhow::bail!("command resp should be JSON");
    };
    if let Some(err) = msg.err {
        anyhow::bail!("{} failed: {} {}", op, err.code, err.message);
    }
    let _stream_id = match args.cmd {
        ClientCmd::New => {
            let r: RcNewAck = serde_json::from_value(msg.payload)?;
            if !r.alive {
                anyhow::bail!("new session alive=false (bash exited immediately)");
            }
            r.stream_id
        }
        ClientCmd::Attach => {
            let r: RcAttachAck = serde_json::from_value(msg.payload)?;
            if !r.alive {
                anyhow::bail!("session dead (bash exited)");
            }
            r.stream_id
        }
        ClientCmd::Ping => unreachable!(),
        ClientCmd::List => unreachable!(),
        ClientCmd::Close => unreachable!(),
    };
    info!("attached, stream_id={}", _stream_id);

    // attach 路径还会收到回放 Raw 帧（在 ack 之后）——读帧循环一并消费
    bridge(framed).await
}

/// 桥接：stdin ↔ framed（daemon），stdout ← framed，SIGWINCH → rc.resize
async fn bridge(framed: Framed<UnixStream, FrameCodec>) -> anyhow::Result<()> {
    use std::sync::Arc;
    use tokio::sync::Mutex;
    let (writer, mut reader) = framed.split();
    // writer 需要被 write_half 和 resize_task 共用 → Arc<Mutex<>>（Mutex 在 tokio
    // 上异步等待；SinkExt::send 本身需要 &mut self）
    let writer = Arc::new(Mutex::new(writer));

    // stdin → writer (Raw 帧，stream_id=ROOT_STREAM_ID)
    let write_half = async {
        let mut stdin = tokio::io::stdin();
        let mut buf = [0u8; 4096];
        loop {
            let n = match stdin.read(&mut buf).await {
                Ok(n) => n,
                Err(_) => break,
            };
            if n == 0 {
                break;
            }
            let frame = Frame::Raw {
                stream_id: ROOT_STREAM_ID,
                data: buf[..n].to_vec(),
            };
            let mut w = writer.lock().await;
            if w.send(frame).await.is_err() {
                break;
            }
        }
        Ok::<_, anyhow::Error>(())
    };

    // reader → stdout（Raw 帧直写；JSON 帧处理）
    let read_half = async {
        let mut stdout = tokio::io::stdout();
        while let Some(item) = reader.next().await {
            match item? {
                Frame::Raw { data, .. } => {
                    if !data.is_empty() {
                        stdout.write_all(&data).await?;
                        stdout.flush().await?;
                    }
                }
                Frame::Json(msg) => {
                    if msg.op == "rc.exited" {
                        info!("session exited");
                        break;
                    }
                    // rc.ping/rc.resize 等来自 daemon 的响应可忽略
                }
            }
        }
        Ok::<_, anyhow::Error>(())
    };

    // SIGWINCH → writer.send(rc.resize)
    let mut sigwinch =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())?;
    let writer_clone = writer.clone();
    let resize_task = async move {
        loop {
            sigwinch.recv().await;
            let (cols, rows) = match terminal_size() {
                Some(s) => s,
                None => continue,
            };
            let msg = Message {
                id: 0,
                kind: MsgKind::Req,
                op: "rc.resize".into(),
                payload: serde_json::to_value(RcResize { cols, rows })?,
                err: None,
            };
            let mut w = writer_clone.lock().await;
            if w.send(Frame::Json(msg)).await.is_err() {
                break;
            }
        }
        Ok::<_, anyhow::Error>(())
    };

    tokio::select! {
        r = write_half => { r?; }
        r = read_half => { r?; }
        r = resize_task => { r?; }
    }
    Ok(())
}

/// 查询当前 TTY 尺寸（非 TTY 场景返回 None）
fn terminal_size() -> Option<(u16, u16)> {
    // TIOCGWINSZ = 0x5413 on linux
    const TIOCGWINSZ: libc::c_ulong = 0x5413;
    let mut ws = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let fd = std::io::stdout().as_raw_fd();
    let r = unsafe { libc::ioctl(fd, TIOCGWINSZ as _, &mut ws) };
    if r == 0 && ws.ws_row > 0 && ws.ws_col > 0 {
        Some((ws.ws_col, ws.ws_row))
    } else {
        None
    }
}
