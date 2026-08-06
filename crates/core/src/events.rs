//! podman /events 流消费者（含重连循环）。
//!
//! ## 已知 podman bug（#23712，≥5.2.1）
//!
//! 响应头延迟直到第一个事件存在 → bollard 空闲 ~2 分钟后超时 RequestTimeoutError。
//!
//! **对策**：
//! 1. 不设置事件类型过滤器（podman 过滤器是 AND 逻辑，且用 "died" 非 "die"）
//! 2. 客户端侧过滤（仅转发 easytidy 管理的容器事件）
//! 3. 设置长/无超时
//! 4. **重连循环**（超时/错误时重建流）

use std::time::Duration;
use bollard::system::EventsOptions;
use bollard::models::EventMessage;
use futures::StreamExt;
use tokio::sync::mpsc;
use tracing::{info, error, debug};
use crate::podman::Podman;
use crate::models::EngineEvent;

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
    // 构建选项：不设置过滤器（客户端侧过滤）
    let opts = EventsOptions::<String> {
        since: None, // 从现在开始
        until: None,
        filters: std::collections::HashMap::new(), // 不设置过滤器（podman bug 对策）
    };

    // 获取底层数据流
    let docker = &podman.docker;

    // 创建事件流（无超时，通过重连循环处理空闲超时）
    let mut stream = docker.events(Some(opts));

    info!("事件流已连接，开始监听...");

    while let Some(result) = stream.next().await {
        match result {
            Ok(event) => {
                if let Some(engine_event) = map_event(event) {
                    debug!("收到事件：{:?}", engine_event);

                    // 发送到 channel（失败则说明接收端关闭，退出）
                    if tx.send(engine_event).await.is_err() {
                        error!("事件 channel 关闭，停止监听");
                        return Ok(());
                    }
                }
            }
            Err(e) => {
                // 流错误，返回外层重连
                return Err(format!("事件流错误：{}", e).into());
            }
        }
    }

    Ok(())
}

/// 从 podman EventMessage 映射为 EngineEvent（客户端侧过滤）。
///
/// 仅处理 easytidy 管理的容器（按标签 `manager=easytidy`）。
fn map_event(event: EventMessage) -> Option<EngineEvent> {
    // 获取 actor
    let actor = event.actor?;

    // 检查是否为容器事件（仅处理容器）
    if actor.attributes.as_ref().and_then(|a| a.get("kind")) != Some(&"container".to_string()) {
        return None;
    }

    // 检查是否为 easytidy 管理（按标签）
    let is_easytidy = actor.attributes
        .as_ref()
        .and_then(|attrs| attrs.get("label"))
        .map(|labels| {
            // podman 标签格式："manager=easytidy,easytidy.name=xxx"
            labels.contains("manager=easytidy")
        })
        .unwrap_or(false);

    if !is_easytidy {
        debug!("跳过非 easytidy 容器事件：{:?}", actor.attributes);
        return None;
    }

    // 提取容器名
    let name = actor.attributes
        .as_ref()
        .and_then(|attrs| attrs.get("name"))
        .cloned()
        .unwrap_or_else(|| actor.id.clone().unwrap_or_default());

    let container_id = actor.id?;

    // 匹配事件类型
    match event.action.as_deref() {
        // 创建事件
        Some("create") => {
            Some(EngineEvent::ContainerCreated {
                container_id,
                name,
            })
        }
        // 启动事件
        Some("start") => {
            Some(EngineEvent::ContainerStarted {
                container_id,
                name,
            })
        }
        // 停止事件（podman 用 "died"）
        Some("died") => {
            // 提取退出码（podman 提供在 actor.attributes 中）
            let exit_code = actor.attributes
                .as_ref()
                .and_then(|attrs| attrs.get("exitCode"))
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(-1);

            Some(EngineEvent::ContainerDied {
                container_id,
                name,
                exit_code,
            })
        }
        // 删除事件
        Some("destroy") => {
            Some(EngineEvent::ContainerRemoved {
                container_id,
                name,
            })
        }
        // 其他事件忽略
        _ => {
            debug!("忽略事件类型：{:?}", event.action);
            None
        }
    }
}
