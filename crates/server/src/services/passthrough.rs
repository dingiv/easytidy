//! 容器内 passthrough 配置（应用列表 + 收藏）——随容器层持久、server 自读拉起。
//!
//! 配置存**容器内** `{user.home}/.easytidy/passthrough.toml`，与宿主侧 `~/.easytidy` 约定
//! 一致；自定义应用图标同根存于 `{user.home}/.easytidy/icons/`。两者均随容器可写层/
//! 快照/rebuild 提交持久，容器自包含。路径与旧 XDG 位置（`.config/easytidy/`、
//! `.local/share/icons/easytidy/`）的一次性文件搬移由 `crate::storage::init` 统一负责
//! （启动时）；本模块只负责配置里**绝对图标路径**的格式感知改写（旧图标目录前缀
//! → 新图标目录前缀），首次访问时执行。
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

/// 进程内一次性守卫（旧 XDG 图标目录前缀 → 新 `.easytidy/icons` 的配置改写）。
static LEGACY_ICONS_MIGRATED: std::sync::Once = std::sync::Once::new();

/// 旧自定义应用图标目录相对 home 的路径（XDG icons 位置；仅作迁移源）。
const LEGACY_ICONS_REL: &str = ".local/share/icons/easytidy";

/// 新自定义应用图标目录相对 home 的路径（与 `crate::storage::icons_dir` 一致）。
const ICONS_REL: &str = ".easytidy/icons";

/// 容器内配置路径：`{user.home}/.easytidy/passthrough.toml`（由 `crate::storage` 统一管理）。
///
/// 首次访问时执行一次性图标路径改写：配置里指向旧 XDG 图标目录的绝对路径改写为
/// 新 `.easytidy/icons` 前缀（文件搬移已由 `crate::storage::init` 完成）。
fn container_passthrough_path() -> PathBuf {
    let path = crate::storage::passthrough_path();

    LEGACY_ICONS_MIGRATED.call_once(|| {
        let home = user_map()
            .map(|u| u.home.clone())
            .unwrap_or_else(|| std::env::var("HOME").unwrap_or_default());
        if home.is_empty() {
            return;
        }
        let legacy_dir = format!("{home}/{LEGACY_ICONS_REL}");
        let new_dir = format!("{home}/{ICONS_REL}");
        rewrite_icon_paths(&path, &legacy_dir, &new_dir);
    });

    path
}

/// 把配置里指向旧图标目录的绝对路径前缀改写为新目录前缀（幂等；无旧前缀则不动）。
///
/// 只改 `apps`/`pinned` 里 `icon` 字段中以 `{legacy_dir}/` 开头的路径（我们的图标）；
/// 指向系统位置（如 `/usr/share/icons/...`）的应用自身图标不受影响。
fn rewrite_icon_paths(path: &std::path::Path, legacy_dir: &str, new_dir: &str) {
    let Ok(content) = std::fs::read_to_string(path) else {
        return;
    };
    let Ok(mut cfg) = toml::from_str::<ContainerPassthroughConfig>(&content) else {
        return;
    };
    let legacy_prefix = format!("{legacy_dir}/");
    let new_prefix = format!("{new_dir}/");
    let mut changed = 0usize;
    for app in cfg.apps.iter_mut().chain(cfg.pinned.iter_mut()) {
        if let Some(icon) = app.icon.as_mut() {
            if let Some(rest) = icon.strip_prefix(&legacy_prefix) {
                *icon = format!("{new_prefix}{rest}");
                changed += 1;
            }
        }
    }
    if changed > 0 {
        match toml::to_string(&cfg) {
            Ok(s) => match std::fs::write(path, s) {
                Ok(()) => info!(
                    "passthrough 图标路径已迁移：{} → {}（{} 个）",
                    legacy_dir,
                    new_dir,
                    changed
                ),
                Err(e) => warn!("图标路径改写写回失败：{e}"),
            },
            Err(e) => warn!("图标路径改写序列化失败：{e}"),
        }
    }
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

/// 自定义应用列表（passthrough.toml 中 id 以 `custom:` 开头的条目；
/// apps.launch_app 解析自定义应用引用用）
pub(crate) fn custom_apps() -> Vec<PtConfiguredApp> {
    let cfg = read_container_config(&container_passthrough_path());
    cfg.apps
        .into_iter()
        .chain(cfg.pinned)
        .filter(|a| a.id.starts_with("custom:"))
        .collect()
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

    #[test]
    fn test_rewrite_icon_paths_moves_custom_icons() {
        // 配置里指向旧 XDG 图标目录的自定义应用图标 → 改写为新 .easytidy/icons 前缀
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".easytidy/passthrough.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "schema_version = 2\napps = []\n\n[[pinned]]\nid = \"custom:chrome\"\n\n"
                .to_string() +
                "name = \"Chrome\"\ncmd = \"google-chrome\"\n"
                    + "icon = \"/home/node/.local/share/icons/easytidy/phoebe.png\"\n",
        )
        .unwrap();

        rewrite_icon_paths(
            &path,
            "/home/node/.local/share/icons/easytidy",
            "/home/node/.easytidy/icons",
        );

        let cfg = read_container_config(&path);
        assert_eq!(
            cfg.pinned[0].icon.as_deref(),
            Some("/home/node/.easytidy/icons/phoebe.png"),
            "自定义图标路径应改写为新前缀"
        );
    }

    #[test]
    fn test_rewrite_icon_paths_keeps_system_icons() {
        // 指向系统位置（/usr/share/icons/...）的应用自身图标不受影响
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".easytidy/passthrough.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "schema_version = 2\napps = []\n\n[[pinned]]\nid = \"a\"\nname = \"A\"\n"
                .to_string() +
                "cmd = \"x\"\nicon = \"/usr/share/icons/hicolor/256x256/apps/x.png\"\n",
        )
        .unwrap();

        rewrite_icon_paths(
            &path,
            "/home/node/.local/share/icons/easytidy",
            "/home/node/.easytidy/icons",
        );

        let cfg = read_container_config(&path);
        assert_eq!(
            cfg.pinned[0].icon.as_deref(),
            Some("/usr/share/icons/hicolor/256x256/apps/x.png"),
            "系统图标路径不应被改写"
        );
    }

    #[test]
    fn test_rewrite_icon_paths_idempotent_when_no_legacy() {
        // 已是新前缀（或无旧前缀）→ 不改写、不报错（幂等）
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".easytidy/passthrough.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original =
            "schema_version = 2\napps = []\n\n[[pinned]]\nid = \"a\"\nname = \"A\"\ncmd = \"x\"\n"
                .to_string()
                + "icon = \"/home/node/.easytidy/icons/a.png\"\n";
        std::fs::write(&path, &original).unwrap();

        rewrite_icon_paths(
            &path,
            "/home/node/.local/share/icons/easytidy",
            "/home/node/.easytidy/icons",
        );

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            original,
            "无旧前缀时不应改动文件"
        );
    }

    #[test]
    fn test_rewrite_icon_paths_noop_when_file_missing() {
        // 配置文件不存在 → 什么都不做（不报错、不建文件）
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".easytidy/passthrough.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        rewrite_icon_paths(
            &path,
            "/home/node/.local/share/icons/easytidy",
            "/home/node/.easytidy/icons",
        );

        assert!(!path.exists(), "文件不存在时不应创建");
    }
}
