//! easytidy-root-channel：宿主侧常驻 root 通道进程（rootless）。
//!
//! ## 职责
//! 经 rootless podman socket `exec --user 0` 在运行中容器内持有一个**共享
//! root shell**（容器内 exec 进程父 = conmon，容器 stop 时被 conmon 回收），
//! 通过 unix socket 向多个客户端（GUI / CLI）提供 attach/detach/close/resize
//! 与流式 I/O + 128KB 回放。
//!
//! ## 进程归属（用户定案 2026-08-27）
//! - **容器内 root shell** 归 conmon：容器 stop → conmon 杀 exec 进程 →
//!   本进程输出流 EOF → 自退。容器侧生命周期本进程不管理。
//! - **宿主 root-channel** 由 GUI/CLI spawn；GUI 退出后进程 orphan、重父到
//!   会话 PID1 / systemd-user（subreaper）——**OS 行为，不干预**（不 setsid、
//!   不 PR_SET_CHILD_SUBREAPER）。存活真相 = flock + socket 存在性，
//!   客户端 `rc.ping` 探测即真相。
//!
//! ## 生命周期
//! 按需拉起（`core::root_channel::ensure_running`）→ flock 防多实例 →
//! exec 持有 → socket 监听 → select 主循环。退出三源：
//! 1. exec 流 EOF（容器销毁 / shell 死）→ 广播 `rc.exited` → 清理
//! 2. `rc.close`（用户主动）→ 写侧 sink（EOF 杀 shell）→ 等 EOF → 清理
//! 3. SIGTERM / SIGINT
//!
//! detach 语义：客户端断开 = 仅退订，root shell 与 root-channel 继续运行，
//! 直至容器销毁或 `rc.close`。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use futures::{SinkExt, StreamExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::oneshot;
use tokio_util::codec::Framed;
use tracing::{error, info, warn};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use easytidy_core::podman::Podman;
use easytidy_core::root_channel::{
    root_channel_dir, root_channel_lock_path, root_channel_socket_path,
};
use easytidy_protocol::frame::FrameCodec;
use easytidy_protocol::rc::{
    RcAttach, RcAttachAck, RcPingResp, RcResize, ROOT_STREAM_ID,
};
use easytidy_protocol::{Frame, Handshake, HandshakeAck, Message, MsgKind, PROTOCOL_VERSION, RpcError};

use session::RootSession;

mod session;

#[derive(Parser)]
#[command(name = "easytidy-root-channel")]
struct Args {
    /// 容器名（root 通道作用域）
    #[arg(long)]
    container: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let name = Args::parse().container;

    // 日志：stderr（宿主进程，无 log file；宿主侧日志归 journald/终端）。
    // RUST_LOG 可覆盖默认 info 级别。
    use tracing_subscriber::{fmt, EnvFilter};
    let _ = tracing_subscriber::registry()
        .with(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with(fmt::layer().with_writer(std::io::stderr))
        .try_init();

    // 1. 状态目录 + flock 防多实例（EX|NB；已占用 → 已在运行，静默退出 0）
    let dir = root_channel_dir()?;
    tokio::fs::create_dir_all(&dir)
        .await
        .context("创建 root 通道状态目录失败")?;
    let lock_path = root_channel_lock_path(&name)?;
    // fs2 的 FileExt 作用于 std::fs::File（同步 flock）
    let lock_file = std::fs::File::create(&lock_path).context("创建 lock 文件失败")?;
    use fs2::FileExt;
    match lock_file.try_lock_exclusive() {
        Ok(()) => {}
        Err(_) => {
            info!("root 通道已在运行（lock 占用）：{name}");
            return Ok(());
        }
    }

    // 2. 连 podman + 起共享 root shell（exec --user 0，父 = conmon）
    let podman = Arc::new(Podman::connect().await.context("连接 podman socket 失败")?);
    let exec = podman
        .exec_pty(&name, "0", 80, 24, vec!["/bin/sh".to_string(), "-l".to_string()])
        .await
        .with_context(|| format!("exec root shell 失败（容器 {name} 未运行？）"))?;

    // stdin 写侧：exec.input 已为 Arc<tokio::sync::Mutex<...>>（session::StdinWriter 同型），
    // close 时换 sink（EOF 杀 shell）
    let session = Arc::new(RootSession::new(exec.input, exec.exec_id.clone()));

    // 3. socket 监听（先清旧 socket；chmod 0600 仅宿主登录用户可连）
    let socket_path = root_channel_socket_path(&name)?;
    let _ = tokio::fs::remove_file(&socket_path).await;
    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("bind socket 失败：{}", socket_path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = tokio::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600)).await;
    }
    info!("root 通道就绪：{name}（socket={}）", socket_path.display());

    // 输出泵：exec 输出流 → ring + fan-out；EOF（容器销毁/shell 死）→ 广播
    // rc.exited + 通知主循环退出
    let (exited_tx, mut exited_rx) = oneshot::channel::<()>();
    let pump_session = session.clone();
    let pump_name = name.clone();
    let pump = tokio::spawn(async move {
        let mut stream = exec.output;
        while let Some(item) = stream.next().await {
            match item {
                Ok(bytes) => pump_session.push_output(&bytes),
                Err(e) => {
                    warn!("exec 输出流错误：{e}");
                    break;
                }
            }
        }
        pump_session.set_dead();
        pump_session.broadcast_exited();
        info!("root 会话输出流 EOF（容器销毁或 shell 退出）：{pump_name}");
        let _ = exited_tx.send(());
    });

    // 信号处理器注册在循环外（每次迭代 new 会泄漏注册）
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;

    // accept 主循环
    let conn_id = Arc::new(AtomicU64::new(1));
    loop {
        tokio::select! {
            _ = sigint.recv() => {
                info!("收到 SIGINT，退出");
                break;
            }
            _ = sigterm.recv() => {
                info!("收到 SIGTERM，退出");
                break;
            }
            _ = &mut exited_rx => {
                info!("root 会话已死（容器销毁/shell 退出），退出");
                break;
            }
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _)) => {
                        let token = conn_id.fetch_add(1, Ordering::SeqCst);
                        let s = session.clone();
                        let p = podman.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_client(stream, s, p, token).await {
                                warn!("客户端连接处理失败（token={token}）：{e}");
                            }
                        });
                    }
                    Err(e) => {
                        error!("accept 失败：{e}");
                        break;
                    }
                }
            }
        }
    }

    // 4. 清理：kill → 等输出泵 → 删 socket
    session.kill();
    let _ = pump.await;
    let _ = tokio::fs::remove_file(&socket_path).await;
    drop(lock_file);
    info!("root 通道退出：{name}");
    Ok(())
}

/// 单客户端连接：握手 → rc.attach（回放 + 订阅）→ 双向 I/O。断开 = detach（退订）。
async fn handle_client(
    stream: UnixStream,
    session: Arc<RootSession>,
    podman: Arc<Podman>,
    token: u64,
) -> anyhow::Result<()> {
    let mut framed = Framed::new(stream, FrameCodec::new());

    // 握手（不硬校验版本，仅记录——宿主侧工具不引入握手拒绝）
    let handshake = next_frame_timed(&mut framed).await.context("握手超时/连接中断")?;
    if let Frame::Json(msg) = &handshake {
        if msg.op == "hello" {
            let hs: Handshake = serde_json::from_value(msg.payload.clone())
                .context("解析 Handshake 失败")?;
            info!("客户端握手：client={} v{} wants={:?}", hs.client, hs.v, hs.wants);
            let ack = HandshakeAck {
                v: PROTOCOL_VERSION,
                server: "easytidy-root-channel".to_string(),
                capabilities: vec!["root".to_string()],
                session_id: format!("rc-{token}"),
            };
            framed.send(resp(msg.id, "hello", &ack, None)).await?;
        } else {
            warn!("首帧非 hello（op={}），继续", msg.op);
        }
    } else {
        anyhow::bail!("首帧必须是 JSON 帧");
    }

    // attach：订阅 + 2026 包裹回放 + ack。attach 前允许 rc.ping 探测
    // （状态探测连接：hello + rc.ping 即断，不订阅）
    let (attach_id, req) = loop {
        let frame = next_frame_timed(&mut framed).await.context("attach 超时/连接中断")?;
        match frame {
            Frame::Json(msg) if msg.op == "rc.ping" => {
                framed
                    .send(resp(msg.id, "rc.ping", &RcPingResp { alive: session.alive() }, None))
                    .await?;
            }
            Frame::Json(msg) if msg.op == "rc.attach" => {
                let r: RcAttach = serde_json::from_value(msg.payload).context("解析 RcAttach 失败")?;
                break (msg.id, r);
            }
            Frame::Json(msg) => anyhow::bail!("attach 前收到未预期消息（op={}）", msg.op),
            _ => anyhow::bail!("attach 必须是 JSON 帧"),
        }
    };
    // 共享会话按新 attach 尺寸同步 TTY（同 server pty attach 语义）
    if let Err(e) = podman.resize_exec_pty(&session.exec_id(), req.cols, req.rows).await {
        warn!("resize root shell 失败（忽略）：{e}");
    }
    let mut rx = session.subscribe(token);
    framed.send(Frame::Raw {
        stream_id: ROOT_STREAM_ID,
        data: session.replay(),
    })
    .await?;
    framed
        .send(resp(
            attach_id,
            "rc.attach",
            &RcAttachAck {
                stream_id: ROOT_STREAM_ID,
                alive: session.alive(),
            },
            None,
        ))
        .await?;
    info!("客户端 attach（token={token}，alive={}）", session.alive());

    // 双向 I/O 循环（无超时：交互 shell 读屏可长时间静默；对端死亡
    // 由 unix socket EOF 自然终结，不靠超时判定）
    loop {
        tokio::select! {
            incoming = next_frame(&mut framed) => {
                match incoming {
                    Ok(Frame::Json(msg)) => {
                        match msg.op.as_str() {
                            "rc.ping" => {
                                framed.send(resp(msg.id, "rc.ping", &RcPingResp { alive: session.alive() }, None)).await?;
                            }
                            "rc.resize" => {
                                let r: RcResize = serde_json::from_value(msg.payload)
                                    .context("解析 RcResize 失败")?;
                                if let Err(e) = podman.resize_exec_pty(&session.exec_id(), r.cols, r.rows).await {
                                    warn!("resize root shell 失败：{e}");
                                }
                            }
                            "rc.close" => {
                                info!("用户主动关闭 root 会话（token={token}）");
                                session.kill();
                            }
                            other => {
                                framed.send(resp_err(msg.id, other, &format!("unknown op: {other}"))).await?;
                            }
                        }
                    }
                    Ok(Frame::Raw { stream_id, data }) => {
                        if stream_id == ROOT_STREAM_ID && !data.is_empty() {
                            session.write_input(&data).await;
                        }
                    }
                    Err(e) => {
                        warn!("客户端帧错误：{e}");
                        break;
                    }
                }
            }
            frame = rx.recv() => {
                match frame {
                    Some(f) => framed.send(f).await?,
                    None => break, // 输出泵侧通道关闭（会话死）
                }
            }
        }
    }

    // detach：退订（session 与 root shell 继续运行）
    session.unsubscribe(token);
    info!("客户端断开（detach，token={token}）");
    Ok(())
}

/// 读下一帧（双向 I/O 循环用；无超时——对端死亡由 socket EOF 终结）。
async fn next_frame(
    framed: &mut Framed<UnixStream, FrameCodec>,
) -> anyhow::Result<Frame> {
    framed
        .next()
        .await
        .context("连接中断")?
        .context("帧解码失败")
}

/// 读下一帧（5s 超时；握手/attach 阶段用——客户端必须按时序发帧，
/// 超时 = 客户端异常，尽早断连）。
async fn next_frame_timed(
    framed: &mut Framed<UnixStream, FrameCodec>,
) -> anyhow::Result<Frame> {
    tokio::time::timeout(std::time::Duration::from_secs(5), framed.next())
        .await
        .context("帧读取超时")?
        .context("连接中断")?
        .context("帧解码失败")
}

fn resp<T: serde::Serialize>(id: u64, op: &str, payload: &T, err: Option<RpcError>) -> Frame {
    Frame::Json(Message {
        id,
        kind: MsgKind::Resp,
        op: op.to_string(),
        payload: serde_json::to_value(payload).unwrap_or(serde_json::Value::Null),
        err,
    })
}

fn resp_err(id: u64, op: &str, msg: &str) -> Frame {
    Frame::Json(Message {
        id,
        kind: MsgKind::Resp,
        op: op.to_string(),
        payload: serde_json::Value::Null,
        err: Some(RpcError {
            code: "op_failed".to_string(),
            message: msg.to_string(),
        }),
    })
}
