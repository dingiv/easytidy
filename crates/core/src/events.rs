//! podman /events 流消费者（含重连循环）。
//!
//! ## 已知 podman bug（#23712，≥5.2.1）
//!
//! 响应头延迟直到第一个事件存在 → 客户端空闲 ~2 分钟后可能超时。
//!
//! **对策**：
//! 1. 不设置事件类型过滤器（podman 过滤器是 AND 逻辑，且用 "died" 非 "die"）
//! 2. 客户端侧过滤（仅转发 easytidy 管理的容器事件）
//! 3. 设置长/无超时
//! 4. **重连循环**（超时/错误时重建流）

use crate::models::EngineEvent;
use crate::podman::Podman;
use futures::StreamExt;
use http_body_util::BodyExt;
use serde_json::Value;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, error, info};

/// 启动事件流（永远运行，含重连）。
///
/// 发送 EngineEvent 到 channel，按 easytidy 标签过滤。
pub async fn events(podman: &Podman, tx: mpsc::Sender<EngineEvent>) {
    let mut backoff = Duration::from_secs(1);
    const MAX_BACKOFF: Duration = Duration::from_secs(30);

    loop {
        match listen_events(podman, tx.clone()).await {
            Ok(_) => {
                // 正常结束（不应发生，除非 channel 关闭）
                info!("事件流正常结束");
                break;
            }
            Err(e) => {
                error!("事件流失败：{}，{} 后重连...", e, backoff.as_secs());
                tokio::time::sleep(backoff).await;

                // 指数退避，上限 30 秒
                backoff = std::cmp::min(backoff * 2, MAX_BACKOFF);
            }
        }
    }
}

/// 监听事件流（单次尝试）。
async fn listen_events(
    podman: &Podman,
    tx: mpsc::Sender<EngineEvent>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (status, body) = podman.http().open_stream("GET", "/events").await?;
    let body = body.into_data_stream();
    if !(200..300).contains(&status) {
        return Err(format!("事件流 HTTP {status}").into());
    }

    info!("事件流已连接，开始监听...");

    // /events 输出为逐行 JSON（EventMessage）
    let mut buf: Vec<u8> = Vec::new();
    let mut body = body;
    while let Some(chunk) = StreamExt::next(&mut body).await {
        let chunk = chunk.map_err(|e| -> Box<dyn std::error::Error> { format!("事件流读取失败：{e}").into() })?;
        buf.extend_from_slice(&chunk);
        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let event: Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(e) => {
                    debug!("事件行解析失败（忽略）：{e}");
                    continue;
                }
            };
            match map_event(&event) {
                Some(engine_event) => {
                    debug!("收到事件：{engine_event:?}");
                    if tx.send(engine_event).await.is_err() {
                        error!("事件 channel 关闭，停止监听");
                        return Ok(());
                    }
                }
                None => debug!("忽略事件：{line}"),
            }
        }
    }

    Ok(())
}

/// 从 compat EventMessage JSON 映射为 EngineEvent（客户端侧过滤）。
///
/// 仅处理容器事件（Type == "container"）且 easytidy 管理的
/// （Actor.Attributes.label 含 `manager=easytidy`）。
fn map_event(event: &Value) -> Option<EngineEvent> {
    // 仅容器事件
    if event.get("Type").and_then(|t| t.as_str()) != Some("container") {
        return None;
    }

    let action = event.get("Action").and_then(|a| a.as_str()).unwrap_or("");
    let actor = event.get("Actor")?;
    let attributes = actor.get("Attributes")?;

    // 检查是否为 easytidy 管理（按标签）
    let is_easytidy = attributes
        .get("label")
        .and_then(|l| l.as_str())
        .map(|labels| labels.contains("manager=easytidy"))
        .unwrap_or(false);

    if !is_easytidy {
        debug!("跳过非 easytidy 容器事件：{attributes}");
        return None;
    }

    let container_id = actor
        .get("ID")
        .and_then(|i| i.as_str())
        .unwrap_or_default()
        .to_string();
    let name = attributes
        .get("name")
        .and_then(|n| n.as_str())
        .unwrap_or_default()
        .to_string();

    match action {
        "create" => Some(EngineEvent::ContainerCreated {
            container_id,
            name,
        }),
        "start" => Some(EngineEvent::ContainerStarted { container_id, name }),
        // 停止事件（podman 用 "died"）
        "died" => {
            let exit_code = attributes
                .get("exitCode")
                .and_then(|s| s.as_str())
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(-1);
            Some(EngineEvent::ContainerDied {
                container_id,
                name,
                exit_code,
            })
        }
        "destroy" => Some(EngineEvent::ContainerRemoved { container_id, name }),
        _ => {
            debug!("忽略事件类型：{action}");
            None
        }
    }
}
