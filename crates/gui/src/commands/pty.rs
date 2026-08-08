//! PTY 命令：打开（attach 常驻终端）/写入/尺寸/关闭/心跳/工作目录。

use std::sync::Arc;
use futures::{SinkExt, StreamExt};

use easytidy_protocol::{
    Frame, Message, MsgKind,
    ops::{PtyOpen, PtyOpenResp, PtyResize, PtyClose, PtyExited},
};
use tracing::error;

use std::collections::HashMap;

use crate::state::{GuiSession, PtyEvent};
use crate::commands::socket::{connect_to_container, send_json_request};

/// 打开 PTY 会话（attach 常驻终端；每条会话一条专用连接）
#[tauri::command]
pub async fn pty_open(
    session: tauri::State<'_, Option<GuiSession>>,
    on_event: tauri::ipc::Channel<PtyEvent>,
    cmd: Option<String>,
    cols: u16,
    rows: u16,
    // 以 root 运行（root 终端；Tauri 参数名 camelCase → 前端传 asRoot）
    as_root: bool,
) -> Result<u32, String> {
    let sess = session.inner().as_ref().ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    let container_name = sess.container_name.clone();

    // 专用连接：连接 + 握手（与 cli cmd_run 同构）
    let mut framed = connect_to_container(&container_name)
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
        // ⚠️ 曾硬编码 false 且命令缺 as_root 参数——前端 asRoot 被静默忽略,
        // root 终端 attach 到 node 会话（2026-08-08 实测）
        as_root,
        // 接线常驻终端：GUI 终端复用以容器为单位的常驻会话
        // （server 持有句柄，连接断开不清理；重开窗口回放当前屏幕）
        attach: true,
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
                    Frame::Raw { stream_id: sid, data } => {
                        if sid == stream_id {
                            let event = PtyEvent {
                                kind: "data".to_string(),
                                data: Some(data),
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
                            if let Some(cwd) = msg
                                .payload
                                .get("cwd")
                                .and_then(|c| c.as_str())
                            {
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
                    error!("PTY 流读取错误：{}", e);
                    break;
                }
                None => break,
            }
        }

        // 清理
        let mut active = active_ptys.lock().await;
        active.remove(&stream_id);
    });

    Ok(stream_id)
}

/// 向 PTY 写入数据（走该 PTY 专用连接的写侧）
#[tauri::command]
pub async fn pty_write(
    session: tauri::State<'_, Option<GuiSession>>,
    stream_id: u32,
    data: Vec<u8>,
) -> Result<(), String> {
    let sess = session.inner().as_ref().ok_or_else(|| "当前模式不是单容器模式".to_string())?;

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

/// 调整 PTY 大小（走该 PTY 专用连接的写侧）
#[tauri::command]
pub async fn pty_resize(
    session: tauri::State<'_, Option<GuiSession>>,
    stream_id: u32,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let sess = session.inner().as_ref().ok_or_else(|| "当前模式不是单容器模式".to_string())?;

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

/// 关闭 PTY 会话（走该 PTY 专用连接的写侧）
#[tauri::command]
pub async fn pty_close(
    session: tauri::State<'_, Option<GuiSession>>,
    stream_id: u32,
) -> Result<(), String> {
    let sess = session.inner().as_ref().ok_or_else(|| "当前模式不是单容器模式".to_string())?;

    // 流已关闭时静默忽略
    let maybe_sink = {
        let active = sess.active_ptys.lock().await;
        active.get(&stream_id).cloned()
    };
    let Some(sink) = maybe_sink else {
        return Ok(());
    };

    let close = PtyClose {
        stream_id,
    };
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
    let sess = session.inner().as_ref().ok_or_else(|| "当前模式不是单容器模式".to_string())?;

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
    let sess = session.inner().as_ref().ok_or_else(|| "当前模式不是单容器模式".to_string())?;

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
    let cwd_resp: easytidy_protocol::ops::PtyCwdResp = serde_json::from_value(resp.payload)
        .map_err(|e| format!("解析 pty.cwd 响应失败：{e}"))?;
    Ok(cwd_resp.cwd)
}

