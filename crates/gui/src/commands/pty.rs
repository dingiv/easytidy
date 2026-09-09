//! PTY 命令：打开（attach 常驻终端）/写入/尺寸/关闭/心跳/工作目录。
//!
//! 终端通道：容器 server socket，承载**容器默认用户**会话。每会话一条
//! 专用连接，server 持有生命周期 + 环形缓冲回放 + cwd 跟随，
//! persistent/attach 语义 → 幂等。root 终端不经此通道——走宿主
//! root-channel 进程（见 `commands/root.rs` 的 `root_terminal_*` 命令）。

use futures::{SinkExt, StreamExt};
use std::sync::Arc;

use base64::Engine as _;
use easytidy_protocol::{
    ops::{
        PtyClose, PtyExited, PtyList, PtyListResp, PtyOpen, PtyOpenResp, PtyResize, PtyTerminalInfo,
    },
    Frame, Message, MsgKind,
};
use tracing::error;

use std::collections::HashMap;

use crate::commands::socket::{connect_to_container, send_json_request};
use crate::state::{GuiSession, PtyEvent};

/// 获取当前所有活跃终端（server 持有的 PTY 会话；多终端面板恢复用——
/// 重开窗口时逐个 attach_stream 重连，输出经环形缓冲回放）。
///
/// 仅覆盖容器默认用户终端（root 终端在宿主 root-channel 进程内，
/// 经 `root_terminal_status` 单独探测）。
#[tauri::command]
pub async fn get_terminals(
    session: tauri::State<'_, Option<GuiSession>>,
) -> Result<Vec<PtyTerminalInfo>, String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;

    let resp = send_json_request(
        sess,
        "pty.list".to_string(),
        serde_json::to_value(PtyList).map_err(|e| e.to_string())?,
    )
    .await
    .map_err(|e| e.to_string())?;
    if let Some(err) = resp.err {
        return Err(format!("{} {}", err.code, err.message));
    }
    let list: PtyListResp =
        serde_json::from_value(resp.payload).map_err(|e| format!("解析 pty.list 响应失败：{e}"))?;
    Ok(list.terminals)
}

/// 打开 PTY 会话（每条会话一条专用连接）。
///
/// 多终端语义：
/// - `persistent=true`：新建**独立持久会话**（server 持有，不随连接断开清理；
///   多开终端入口）
/// - `attach_stream_id=Some(n)`：附接已有会话（重开窗口恢复面板；server
///   回放当前屏幕）
/// - 均缺省（旧语义）：attach 身份默认常驻会话
#[tauri::command]
#[allow(clippy::too_many_arguments)] // Tauri IPC 契约:参数名即前端 invoke 键,不宜包结构体
pub async fn pty_open(
    session: tauri::State<'_, Option<GuiSession>>,
    on_event: tauri::ipc::Channel<PtyEvent>,
    cmd: Option<String>,
    cols: u16,
    rows: u16,
    // 新建独立持久会话（多终端；Tauri 参数名 camelCase → 前端传 persistent）
    persistent: bool,
    // 附接已有会话（多终端恢复；前端传 attachStreamId）
    attach_stream_id: Option<u32>,
) -> Result<u32, String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    let container_name = sess.container_name.clone();

    // 终端走容器 server，承载容器默认用户会话（root 终端走宿主 root-channel
    // 通道，见 commands/root.rs）。persistent/attach/回放语义 → 幂等
    // （StrictMode remount 只 attach 不复建）。
    // 专用连接 + 握手（与 cli cmd_run 同构）。该连接是**双向信道**
    // （server 经 Raw 帧/Evt 反向推送 PTY 输出），session_id 仅日志/排障用。
    let (mut framed, _session_id) = connect_to_container(&container_name)
        .await
        .map_err(|e| e.to_string())?;

    let (cmd, argv) = if let Some(c) = cmd {
        let parts: Vec<String> = c.split_whitespace().map(String::from).collect();
        (parts[0].clone(), parts)
    } else {
        (String::new(), Vec::new())
    };

    let pty_open = PtyOpen {
        cmd,
        argv,
        env: HashMap::new(),
        cwd: "/".to_string(),
        cols,
        rows,
        // 多终端：新开 = 独立持久会话；恢复 = 附接已有会话
        // （server 持有句柄，连接断开不清理；重开窗口回放当前屏幕）
        attach: false,
        persistent,
        attach_stream: attach_stream_id,
    };

    // 在此连接上发送 pty.open 并接收响应
    framed
        .send(Frame::Json(Message {
            id: 1,
            kind: MsgKind::Req,
            op: "pty.open".to_string(),
            payload: serde_json::to_value(pty_open).map_err(|e| e.to_string())?,
            err: None,
        }))
        .await
        .map_err(|e| format!("发送 pty.open 失败：{}", e))?;

    let resp = framed
        .next()
        .await
        .ok_or_else(|| "pty.open 响应为空".to_string())?
        .map_err(|e| format!("接收 pty.open 响应失败：{}", e))?;

    let Frame::Json(resp_msg) = resp else {
        return Err("pty.open 响应应为 JSON 帧".to_string());
    };
    if let Some(err) = resp_msg.err {
        return Err(format!("pty.open 失败：{} {}", err.code, err.message));
    }

    let open_resp: PtyOpenResp = serde_json::from_value(resp_msg.payload)
        .map_err(|e| format!("解析 pty.open 响应失败：{}", e))?;
    let stream_id = open_resp.stream_id;

    // 拆分读写侧：写侧共享给 pty_write/resize/close，读侧归本任务的 reader
    let (sink, stream) = framed.split();
    let sink = Arc::new(tokio::sync::Mutex::new(sink));

    {
        let mut active = sess.active_ptys.lock().await;
        active.insert(stream_id, sink.clone());
    }

    // reader：专用连接读侧 → Channel（data / exited），退出时清理注册
    let active_ptys = Arc::clone(&sess.active_ptys);
    tokio::spawn(async move {
        let mut stream = stream;
        loop {
            match stream.next().await {
                Some(Ok(frame)) => match frame {
                    Frame::Raw {
                        stream_id: sid,
                        data: first,
                    } => {
                        if sid == stream_id {
                            // 合并输出洪流：vi/htop 等全屏应用一次重绘会产生
                            // 大量小帧，逐帧 IPC（JSON 数字数组序列化膨胀 4-5x）
                            // 会打爆 WebKitGTK 消息管道 → 终端输入卡死。合并
                            // 窗口 8ms / 上限 256KB，只发一条事件。
                            let mut merged = first;
                            let deadline = std::time::Duration::from_millis(8);
                            loop {
                                if merged.len() >= 256 * 1024 {
                                    break;
                                }
                                match tokio::time::timeout(deadline, stream.next()).await {
                                    Ok(Some(Ok(Frame::Raw { stream_id: sid2, data: more }))) if sid2 == stream_id => {
                                        merged.extend_from_slice(&more);
                                    }
                                    _ => break,
                                }
                            }
                            let event = PtyEvent {
                                kind: "data".to_string(),
                                data: Some(base64::engine::general_purpose::STANDARD.encode(&merged)),
                                code: None,
                                cwd: None,
                            };
                            if on_event.send(event).is_err() {
                                break; // 通道关闭
                            }
                        }
                    }
                    Frame::Json(msg) => {
                        if msg.op == "pty.exited" {
                            match serde_json::from_value::<PtyExited>(msg.payload) {
                                Ok(exited) if exited.stream_id == stream_id => {
                                    let event = PtyEvent {
                                        kind: "exited".to_string(),
                                        data: None,
                                        code: Some(exited.code),
                                        cwd: None,
                                    };
                                    let _ = on_event.send(event);
                                    break;
                                }
                                _ => {}
                            }
                        } else if msg.op == "pty.cwdChanged" {
                            // server 主动推送（TTY 事件驱动：输入回车时检测 cwd 变化）
                            if let Some(cwd) = msg.payload.get("cwd").and_then(|c| c.as_str()) {
                                let event = PtyEvent {
                                    kind: "cwdChanged".to_string(),
                                    data: None,
                                    code: None,
                                    cwd: Some(cwd.to_string()),
                                };
                                let _ = on_event.send(event);
                            }
                        }
                    }
                },
                Some(Err(e)) => {
                    error!("PTY 流 {} 读取错误：{}", stream_id, e);
                    break;
                }
                None => {
                    tracing::warn!("PTY 流 {} 服务端连接关闭(None)", stream_id);
                    break;
                }
            }
        }

        // 清理
        tracing::warn!("PTY 流 {} reader 退出,清除 active_ptys 登记", stream_id);
        let mut active = active_ptys.lock().await;
        active.remove(&stream_id);
    });

    Ok(stream_id)
}

/// 向 PTY 写入数据（统一走该会话专用连接的写侧）
#[tauri::command]
pub async fn pty_write(
    session: tauri::State<'_, Option<GuiSession>>,
    stream_id: u32,
    data: Vec<u8>,
) -> Result<(), String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;

    let sink = {
        let active = sess.active_ptys.lock().await;
        active.get(&stream_id).cloned()
    }
    .ok_or_else(|| format!("PTY 流 {} 不存在", stream_id))?;

    sink.lock()
        .await
        .send(Frame::Raw { stream_id, data })
        .await
        .map_err(|e| format!("PTY 写入失败：{}", e))?;
    Ok(())
}

/// 调整 PTY 大小（统一走该会话专用连接的写侧）
#[tauri::command]
pub async fn pty_resize(
    session: tauri::State<'_, Option<GuiSession>>,
    stream_id: u32,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;

    // 流已关闭时静默忽略（resize 可能在退出竞态中触发）
    let maybe_sink = {
        let active = sess.active_ptys.lock().await;
        active.get(&stream_id).cloned()
    };
    let Some(sink) = maybe_sink else {
        return Ok(());
    };

    let resize = PtyResize {
        stream_id,
        cols,
        rows,
    };
    sink.lock()
        .await
        .send(Frame::Json(Message {
            id: 2,
            kind: MsgKind::Req,
            op: "pty.resize".to_string(),
            payload: serde_json::to_value(resize).map_err(|e| e.to_string())?,
            err: None,
        }))
        .await
        .map_err(|e| format!("PTY resize 失败：{}", e))?;
    Ok(())
}

/// 关闭 PTY 会话（pty.close 帧 → server 终结会话并回收 shell）
#[tauri::command]
pub async fn pty_close(
    session: tauri::State<'_, Option<GuiSession>>,
    stream_id: u32,
) -> Result<(), String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;

    // 流已关闭时静默忽略
    let maybe_sink = {
        let active = sess.active_ptys.lock().await;
        active.get(&stream_id).cloned()
    };
    let Some(sink) = maybe_sink else {
        // 本地无句柄（server 侧会话，如恢复面板 attach 前关闭、遗留
        // root 会话清理）→ 经共享 socket 发 pty.close。幂等：会话已
        // 不存在时 server 报错也视为关闭成功
        let req = PtyClose { stream_id };
        if let Ok(payload) = serde_json::to_value(req) {
            if let Err(e) = send_json_request(sess, "pty.close".to_string(), payload).await {
                tracing::debug!("pty.close（共享 socket）失败（会话可能已不存在）：{e}");
            }
        }
        return Ok(());
    };

    let close = PtyClose { stream_id };
    sink.lock()
        .await
        .send(Frame::Json(Message {
            id: 3,
            kind: MsgKind::Req,
            op: "pty.close".to_string(),
            payload: serde_json::to_value(close).map_err(|e| e.to_string())?,
            err: None,
        }))
        .await
        .map_err(|e| format!("PTY close 失败：{}", e))?;

    // 清理活动 PTY
    {
        let mut active = sess.active_ptys.lock().await;
        active.remove(&stream_id);
    }

    Ok(())
}

/// 心跳保活（走该 PTY 专用连接发 ping 帧）。
///
/// server 对无帧连接有 idle 超时（有 PTY 的连接 1h）——终端长时间无输出
/// 时靠心跳维持连接，否则连接被回收后输入永久失效（前端无法感知）。
/// ping 响应由 pty_open 的 reader 任务消费（非 pty.exited 的 JSON 忽略）。
#[tauri::command]
pub async fn pty_ping(
    session: tauri::State<'_, Option<GuiSession>>,
    stream_id: u32,
) -> Result<(), String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;

    // 流已关闭时静默忽略（ping 可能在退出竞态中触发）
    let maybe_sink = {
        let active = sess.active_ptys.lock().await;
        active.get(&stream_id).cloned()
    };
    let Some(sink) = maybe_sink else {
        return Ok(());
    };

    sink.lock()
        .await
        .send(Frame::Json(Message {
            id: 0,
            kind: MsgKind::Req,
            op: "ping".to_string(),
            payload: serde_json::Value::Null,
            err: None,
        }))
        .await
        .map_err(|e| format!("ping 发送失败：{}", e))?;
    Ok(())
}

/// 查询 PTY 会话主进程的实时工作目录（文件浏览器"跟随终端"）。
/// 经共享 socket 发 pty.cwd（不占 PTY 专用连接）。
#[tauri::command]
pub async fn pty_cwd(
    session: tauri::State<'_, Option<GuiSession>>,
    stream_id: u32,
) -> Result<String, String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;

    let req = easytidy_protocol::ops::PtyCwd { stream_id };
    let resp = send_json_request(
        sess,
        "pty.cwd".to_string(),
        serde_json::to_value(req).map_err(|e| e.to_string())?,
    )
    .await
    .map_err(|e| e.to_string())?;
    if let Some(err) = resp.err {
        return Err(format!("{} {}", err.code, err.message));
    }
    let cwd_resp: easytidy_protocol::ops::PtyCwdResp =
        serde_json::from_value(resp.payload).map_err(|e| format!("解析 pty.cwd 响应失败：{e}"))?;
    Ok(cwd_resp.cwd)
}
