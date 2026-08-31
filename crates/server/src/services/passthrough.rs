//! 容器内 passthrough 配置（应用列表 + 收藏）——随容器层持久、server 自读拉起。
//!
//! 配置存**容器内** `{user.home}/.config/easytidy/passthrough.toml`（user_map 失败回退
//! `/home/easytidy`），随容器可写层/快照/rebuild 提交持久，容器自包含。server 启动时
//! 自读并拉起 auto_start 应用（不再依赖宿主推送）。
//!
//! 两类条目同存一文件：
//! - `apps`：有状态条目（auto_start 应用 + 自定义应用）
//! - `pinned`：收藏（pin 到 GUI 工具栏）——**随容器自包含**，容器删除/同名重建即清空，
//!   不再泄漏到宿主（修复「新建容器继承旧容器收藏」bug）。

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
        .unwrap_or_else(|| std::env::var("HOME").unwrap_or_else(|_| "/".to_string()));
    PathBuf::from(home).join(".config/easytidy/passthrough.toml")
}

/// 容器内配置文件的磁盘格式（应用列表 + 收藏，同存一文件，容器自包含）
#[derive(Debug, Default, Serialize, Deserialize)]
struct ContainerPassthroughConfig {
    schema_version: u32,
    #[serde(default)]
    apps: Vec<PtConfiguredApp>,
    #[serde(default)]
    pinned: Vec<PtConfiguredApp>,
}

/// 读容器内配置（文件缺失 → 空配置；解析失败仅告警）
fn read_container_config(path: &std::path::Path) -> ContainerPassthroughConfig {
    let Ok(content) = std::fs::read_to_string(path) else {
        return ContainerPassthroughConfig::default();
    };
    toml::from_str::<ContainerPassthroughConfig>(&content).unwrap_or_else(|e| {
        warn!("解析容器内 passthrough 配置失败（忽略）：{e}");
        ContainerPassthroughConfig::default()
    })
}

/// 写容器内配置（建目录 + 临时文件 + rename 原子写；apps + pinned 整份覆盖）
fn write_container_config(
    path: &std::path::Path,
    apps: &[PtConfiguredApp],
    pinned: &[PtConfiguredApp],
) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建 passthrough 配置目录失败：{}", parent.display()))?;
    }
    let config = ContainerPassthroughConfig {
        schema_version: 1,
        apps: apps.to_vec(),
        pinned: pinned.to_vec(),
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
    let cfg = read_container_config(&container_passthrough_path());
    Ok(Frame::Json(Message {
        id: msg.id,
        kind: MsgKind::Resp,
        op: "passthrough.list".to_string(),
        payload: serde_json::to_value(PassthroughListResp {
            apps: cfg.apps,
            pinned: cfg.pinned,
        })?,
        err: None,
    }))
}

/// passthrough.set：整份覆盖容器内配置（apps + pinned）
pub(crate) async fn handle_passthrough_set(
    msg: Message,
    _state: &Arc<ServerState>,
) -> Result<Frame> {
    let req: PassthroughSet =
        serde_json::from_value(msg.payload).context("解析 PassthroughSet 失败")?;
    write_container_config(&container_passthrough_path(), &req.apps, &req.pinned)
        .with_context(|| "写入容器内 passthrough 配置失败".to_string())?;
    info!(
        "passthrough 配置已写入容器：{} 个应用、{} 个收藏",
        req.apps.len(),
        req.pinned.len()
    );
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
    let cfg = read_container_config(&container_passthrough_path());
    let to_launch: Vec<PtConfiguredApp> = cfg.apps.into_iter().filter(|a| a.auto_start).collect();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_container_config_roundtrip_with_pinned() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("passthrough.toml");

        let apps = vec![PtConfiguredApp {
            id: "/usr/share/applications/foo.desktop".to_string(),
            name: "Foo".to_string(),
            cmd: "foo".to_string(),
            desktop_file: Some("/usr/share/applications/foo.desktop".to_string()),
            auto_start: true,
            icon: None,
        }];
        let pinned = vec![PtConfiguredApp {
            id: "custom:bar".to_string(),
            name: "Bar".to_string(),
            cmd: "bar".to_string(),
            desktop_file: None,
            auto_start: false,
            icon: Some("/home/easytidy/.easytidy/icons/x.png".to_string()),
        }];

        write_container_config(&path, &apps, &pinned).unwrap();
        let cfg = read_container_config(&path);
        assert_eq!(cfg.apps, apps);
        assert_eq!(cfg.pinned, pinned);
    }

    #[test]
    fn test_container_config_missing_file_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.toml");
        let cfg = read_container_config(&path);
        assert!(cfg.apps.is_empty());
        assert!(cfg.pinned.is_empty());
    }

    #[test]
    fn test_container_config_backward_compat_no_pinned_field() {
        // 旧格式（无 pinned 字段）仍能解析（pinned 默认空）——升级不丢 apps
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.toml");
        std::fs::write(&path, "schema_version = 1\napps = []\n").unwrap();
        let cfg = read_container_config(&path);
        assert!(cfg.apps.is_empty());
        assert!(cfg.pinned.is_empty());
    }
}
