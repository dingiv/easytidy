//! 容器内 passthrough 配置（auto-start 应用列表）——随容器层持久、server 自读拉起。
//!
//! 配置存**容器内** `{user.home}/.config/easytidy/passthrough.toml`（user_map 失败回退
//! `/home/easytidy`），随容器可写层/快照/rebuild 提交持久，容器自包含。server 启动时
//! 自读并拉起 auto_start 应用（不再依赖宿主推送）。宿主侧只留收藏(pinned)与导出元数据。

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use easytidy_protocol::ops::{PassthroughListResp, PassthroughSet, PtConfiguredApp};
use easytidy_protocol::{Frame, Message, MsgKind};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{info, warn};

use crate::setup::user_map;
use crate::state::ServerState;
use crate::services::apps::spawn_managed_process;

/// 容器内配置路径：`{user.home}/.config/easytidy/passthrough.toml`
fn container_passthrough_path() -> PathBuf {
    let home = user_map()
        .map(|u| u.home.clone())
        .unwrap_or_else(|| "/home/easytidy".to_string());
    PathBuf::from(home).join(".config/easytidy/passthrough.toml")
}

/// 容器内配置文件的磁盘格式
#[derive(Debug, Default, Serialize, Deserialize)]
struct ContainerPassthroughConfig {
    schema_version: u32,
    #[serde(default)]
    apps: Vec<PtConfiguredApp>,
}

/// 读容器内配置（文件缺失 → 空列表；解析失败仅告警）
fn read_container_config(path: &std::path::Path) -> Vec<PtConfiguredApp> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    toml::from_str::<ContainerPassthroughConfig>(&content)
        .map(|c| c.apps)
        .unwrap_or_else(|e| {
            warn!("解析容器内 passthrough 配置失败（忽略）：{e}");
            Vec::new()
        })
}

/// 写容器内配置（建目录 + 临时文件 + rename 原子写）
fn write_container_config(path: &std::path::Path, apps: &[PtConfiguredApp]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建 passthrough 配置目录失败：{}", parent.display()))?;
    }
    let config = ContainerPassthroughConfig {
        schema_version: 1,
        apps: apps.to_vec(),
    };
    let content = toml::to_string(&config).context("序列化 passthrough 配置失败")?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, &content)
        .with_context(|| format!("写 passthrough 配置失败：{}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("原子替换失败：{}", path.display()))?;
    Ok(())
}

/// passthrough.list：读容器内配置（不存在返回空列表）
pub(crate) async fn handle_passthrough_list(
    msg: Message,
    _state: &Arc<ServerState>,
) -> Result<Frame> {
    let apps = read_container_config(&container_passthrough_path());
    Ok(Frame::Json(Message {
        id: msg.id,
        kind: MsgKind::Resp,
        op: "passthrough.list".to_string(),
        payload: serde_json::to_value(PassthroughListResp { apps })?,
        err: None,
    }))
}

/// passthrough.set：整份覆盖容器内配置
pub(crate) async fn handle_passthrough_set(
    msg: Message,
    _state: &Arc<ServerState>,
) -> Result<Frame> {
    let req: PassthroughSet =
        serde_json::from_value(msg.payload).context("解析 PassthroughSet 失败")?;
    write_container_config(&container_passthrough_path(), &req.apps)
        .with_context(|| "写入容器内 passthrough 配置失败".to_string())?;
    info!("passthrough 配置已写入容器：{} 个应用", req.apps.len());
    Ok(Frame::Json(Message {
        id: msg.id,
        kind: MsgKind::Resp,
        op: "passthrough.set".to_string(),
        payload: json!(null),
        err: None,
    }))
}

/// 启动自读拉起 auto-start 应用（fire-and-forget，单条独立成败；继承 server 透传 env）
pub(crate) async fn launch_auto_start(state: &Arc<ServerState>) {
    let apps = read_container_config(&container_passthrough_path());
    let to_launch: Vec<PtConfiguredApp> = apps.into_iter().filter(|a| a.auto_start).collect();
    if to_launch.is_empty() {
        return;
    }
    info!(
        "auto-start：{} 个应用待拉起（容器内配置）",
        to_launch.len()
    );
    for app in to_launch {
        match spawn_managed_process(state, &app.cmd, "passthrough", app.name.clone(), None).await {
            Ok(pid) => info!("auto-start 拉起成功：{} (pid={pid})", app.name),
            Err(e) => warn!("auto-start 拉起失败：{}：{e}", app.name),
        }
    }
}
