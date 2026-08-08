//! Passthrough 配置与 auto-start 拉起（宿主侧）。
//!
//! - 配置：独立文件 `$XDG_CONFIG_HOME/easytidy/passthrough.toml`
//!   （不塞进 config.toml——其 settings 是 JSON Value，嵌套结构序列化
//!   TOML 会报错），记录按容器分组的应用列表（含 auto-start 标记）
//! - 拉起：容器启动后（`Podman::start`/`restart` 挂点）宿主侧经容器
//!   server socket 发 `apps.launch`——**由 server 拉起并保活**（宿主
//!   一次性 CLI 连接断开即杀 PTY 会话，实测；server spawn 的子进程
//!   独立于连接存活）

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::path::PathBuf;
use std::sync::Mutex;

use easytidy_protocol::ops::{AppsLaunch, AppsLaunchItem, AppsLaunchResp};
use easytidy_protocol::frame::FrameCodec;
use easytidy_protocol::{Frame, Handshake, Message, MsgKind, PROTOCOL_VERSION};
use futures::{SinkExt, StreamExt};
use tokio::net::UnixStream;
use tokio_util::codec::Framed;

use crate::error::{Error, Result};

/// 应用标识约定：
/// - 容器扫描应用：容器内 .desktop 路径（如 /usr/share/applications/google-chrome.desktop）
/// - 自定义应用：`custom:<name>`（前缀防与路径串撞键，重名可区分）
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PassthroughApp {
    pub id: String,
    pub name: String,
    /// 实际执行命令串（扫描应用已去 %U 占位符；自定义应用原样）
    pub cmd: String,
    /// 仅扫描应用：容器内 .desktop 路径（export/revoke 元数据）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop_file: Option<String>,
    #[serde(default)]
    pub auto_start: bool,
    /// 宿主本地图标路径（~/.easytidy/icons/；自定义应用用户选定后落盘，
    /// export 时 Icon= 直接用；容器应用导出时由 GUI 搬运生成）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

impl PassthroughApp {
    pub fn is_custom(&self) -> bool {
        self.id.starts_with("custom:")
    }
}

/// 全局 passthrough 配置（按容器分组）
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PassthroughConfig {
    pub schema_version: u32,
    #[serde(default)]
    pub containers: HashMap<String, Vec<PassthroughApp>>,
}

impl PassthroughConfig {
    pub fn new() -> Self {
        Self {
            schema_version: 1,
            containers: HashMap::new(),
        }
    }
}

/// passthrough 配置文件（仿 configfile.rs：flock + 原子写）。
pub struct PassthroughConfigFile {
    path: PathBuf,
    cache: Mutex<Option<PassthroughConfig>>,
}

impl PassthroughConfigFile {
    /// 默认路径：~/.easytidy/passthrough.toml
    /// （首次使用自动迁移旧 $XDG_CONFIG_HOME/easytidy/passthrough.toml）
    pub fn default_path() -> Result<PathBuf> {
        crate::appdata::migrate_legacy_configs();
        crate::appdata::passthrough_config_path()
    }

    pub fn with_path(path: PathBuf) -> Self {
        Self {
            path,
            cache: Mutex::new(None),
        }
    }

    /// 加载配置（flock shared lock；文件缺失返回默认空配置）
    pub fn load(&self) -> Result<PassthroughConfig> {
        if !self.path.exists() {
            return Ok(PassthroughConfig::new());
        }
        let file = File::open(&self.path)
            .map_err(|e| Error::Config(format!("打开 passthrough 配置失败：{e}")))?;
        file.lock_shared()
            .map_err(|e| Error::Config(format!("获取 passthrough 配置锁失败：{e}")))?;
        let mut content = String::new();
        {
            let mut reader = file;
            reader
                .read_to_string(&mut content)
                .map_err(|e| Error::Config(format!("读取 passthrough 配置失败：{e}")))?;
        }
        let config: PassthroughConfig = toml::from_str(&content)
            .map_err(|e| Error::Config(format!("解析 passthrough 配置失败：{e}")))?;
        *self.cache.lock().unwrap() = Some(config.clone());
        Ok(config)
    }

    /// 保存配置（flock exclusive + 临时文件 + rename 原子写）
    pub fn save(&self, config: &PassthroughConfig) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Config(format!("创建配置目录失败：{e}")))?;
        }
        // 承载 flock 的句柄：不截断现有文件（并发 load 可能读到半截）
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&self.path)
            .map_err(|e| Error::Config(format!("创建 passthrough 配置文件失败：{e}")))?;
        file.lock()
            .map_err(|e| Error::Config(format!("获取 passthrough 配置锁失败：{e}")))?;
        let content = toml::to_string_pretty(config)
            .map_err(|e| Error::Config(format!("序列化 passthrough 配置失败：{e}")))?;
        let tmp = self.path.with_extension("toml.tmp");
        std::fs::write(&tmp, &content)
            .map_err(|e| Error::Config(format!("写入临时配置失败：{e}")))?;
        std::fs::rename(&tmp, &self.path)
            .map_err(|e| Error::Config(format!("替换配置文件失败：{e}")))?;
        *self.cache.lock().unwrap() = Some(config.clone());
        Ok(())
    }

    /// 读取某容器的应用列表（无条目返回空）
    pub fn apps(&self, container: &str) -> Result<Vec<PassthroughApp>> {
        let config = self.load()?;
        Ok(config.containers.get(container).cloned().unwrap_or_default())
    }

    /// 写入/更新单个应用
    pub fn upsert_app(&self, container: &str, app: PassthroughApp) -> Result<()> {
        let mut config = self.load()?;
        let apps = config.containers.entry(container.to_string()).or_default();
        if let Some(existing) = apps.iter_mut().find(|a| a.id == app.id) {
            *existing = app;
        } else {
            apps.push(app);
        }
        self.save(&config)
    }

    /// 删除应用（存在则删除容器条目;容器条目空则移除键）
    pub fn remove_app(&self, container: &str, id: &str) -> Result<()> {
        let mut config = self.load()?;
        let Some(apps) = config.containers.get_mut(container) else {
            return Ok(());
        };
        apps.retain(|a| a.id != id);
        if apps.is_empty() {
            config.containers.remove(container);
        }
        self.save(&config)
    }

    /// 设置 auto-start。
    ///
    /// 语义：enabled=true → upsert（cmd 一并更新，保证拉起命令最新）；
    /// enabled=false → **custom 条目保留并置 false，扫描应用删条目**
    /// （配置文件只存"有状态"条目，absence = auto_start false）
    pub fn set_auto_start(
        &self,
        container: &str,
        app: PassthroughApp,
        enabled: bool,
    ) -> Result<()> {
        if enabled {
            let mut app = app;
            app.auto_start = true;
            self.upsert_app(container, app)
        } else {
            let mut config = self.load()?;
            let Some(apps) = config.containers.get_mut(container) else {
                return Ok(());
            };
            if let Some(existing) = apps.iter_mut().find(|a| a.id == app.id) {
                if existing.is_custom() {
                    existing.auto_start = false; // custom 保留（是资产，仅关 auto-start）
                } else {
                    apps.retain(|a| a.id != app.id); // 扫描应用 absence = false
                }
                if apps.is_empty() {
                    config.containers.remove(container);
                }
                self.save(&config)?;
            }
            Ok(())
        }
    }

    /// 添加自定义应用（重名报错；返回构造好的应用条目）
    pub fn add_custom(&self, container: &str, name: &str, cmd: &str) -> Result<PassthroughApp> {
        if name.trim().is_empty() || cmd.trim().is_empty() {
            return Err(Error::Config("自定义应用名称与命令不能为空".to_string()));
        }
        let id = format!("custom:{name}");
        let mut config = self.load()?;
        let apps = config.containers.entry(container.to_string()).or_default();
        if apps.iter().any(|a| a.id == id) {
            return Err(Error::Config(format!("自定义应用 {name} 已存在")));
        }
        let app = PassthroughApp {
            id,
            name: name.to_string(),
            cmd: cmd.trim().to_string(),
            desktop_file: None,
            auto_start: false,
            icon: None,
        };
        apps.push(app.clone());
        self.save(&config)?;
        Ok(app)
    }
}

// ============================================================================
// auto-start 拉起（宿主侧触发，server 保活）
// ============================================================================

/// 容器启动后拉起 auto-start 应用（fire-and-forget，错误仅日志）。
///
/// 流程：读 passthrough 配置 → 无 auto_start 条目零成本返回 →
/// 连接容器 server socket（0.25s 间隔重试 ~6s，容忍 server 启动延迟）→
/// hello 握手 → apps.launch → 读响应记录结果。
pub async fn autostart_apps(container: &str) {
    let config_path = match PassthroughConfigFile::default_path() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("解析 passthrough 配置路径失败：{e}");
            return;
        }
    };
    let config_file = PassthroughConfigFile::with_path(config_path);
    let apps = match config_file.apps(container) {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!("读取 passthrough 配置失败：{e}");
            return;
        }
    };
    let to_launch: Vec<PassthroughApp> = apps.into_iter().filter(|a| a.auto_start).collect();
    if to_launch.is_empty() {
        return; // 零成本：无 auto-start 配置
    }

    tracing::info!(
        "auto-start：{} 个应用待拉起（container={container}）",
        to_launch.len()
    );

    let socket_path = match crate::host_socket_path(container) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("解析容器 socket 路径失败：{e}");
            return;
        }
    };

    // 连接重试：server 随容器启动，容忍就绪延迟（2s 窗口——
    // start 挂点是 await 语义，窗口过大拖慢启动路径；超时静默降级）
    let mut framed: Option<Framed<UnixStream, FrameCodec>> = None;
    for _ in 0..8 {
        match UnixStream::connect(&socket_path).await {
            Ok(stream) => {
                framed = Some(Framed::new(stream, FrameCodec::new()));
                break;
            }
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(250)).await,
        }
    }
    let mut framed = match framed {
        Some(f) => f,
        None => {
            // 非 easytidy 容器（无 server）/server 启动失败：静默
            tracing::warn!("auto-start：连接容器 server 超时（container={container}）");
            return;
        }
    };

    // hello 握手
    let handshake = Handshake {
        v: PROTOCOL_VERSION,
        client: "easytidy-core".to_string(),
        wants: vec!["apps".to_string()],
    };
    let hello = Frame::Json(Message {
        id: 1,
        kind: MsgKind::Req,
        op: "hello".to_string(),
        payload: serde_json::to_value(handshake).unwrap_or_default(),
        err: None,
    });
    if framed.send(hello).await.is_err() {
        tracing::warn!("auto-start：发送握手失败（container={container}）");
        return;
    }
    match framed.next().await {
        Some(Ok(Frame::Json(_))) => {}
        _ => {
            tracing::warn!("auto-start：握手未确认（container={container}）");
            return;
        }
    }

    // apps.launch
    let launch = Frame::Json(Message {
        id: 2,
        kind: MsgKind::Req,
        op: "apps.launch".to_string(),
        payload: serde_json::to_value(AppsLaunch {
            apps: to_launch
                .iter()
                .map(|a| AppsLaunchItem {
                    name: a.name.clone(),
                    cmd: a.cmd.clone(),
                })
                .collect(),
        })
        .unwrap_or_default(),
        err: None,
    });
    if framed.send(launch).await.is_err() {
        tracing::warn!("auto-start：发送 apps.launch 失败（container={container}）");
        return;
    }
    match framed.next().await {
        Some(Ok(Frame::Json(resp))) => {
            if let Ok(launch_resp) =
                serde_json::from_value::<AppsLaunchResp>(resp.payload)
            {
                for r in launch_resp.results {
                    match r.pid {
                        Some(pid) => tracing::info!("auto-start 拉起成功：{} (pid={pid})", r.name),
                        None => tracing::warn!(
                            "auto-start 拉起失败：{}：{}",
                            r.name,
                            r.error.as_deref().unwrap_or("unknown")
                        ),
                    }
                }
            }
        }
        _ => tracing::warn!("auto-start：apps.launch 响应异常（container={container}）"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_file(dir: &TempDir) -> PassthroughConfigFile {
        PassthroughConfigFile::with_path(dir.path().join("passthrough.toml"))
    }

    fn app(id: &str, name: &str, auto_start: bool) -> PassthroughApp {
        PassthroughApp {
            id: id.to_string(),
            name: name.to_string(),
            cmd: "echo hi".to_string(),
            desktop_file: None,
            auto_start,
            icon: None,
        }
    }

    #[test]
    fn test_roundtrip() {
        let dir = TempDir::new().unwrap();
        let f = test_file(&dir);
        let scanned = PassthroughApp {
            id: "/usr/share/applications/google-chrome.desktop".to_string(),
            name: "Chrome".to_string(),
            cmd: "google-chrome-stable --disable-dev-shm-usage".to_string(),
            desktop_file: Some("/usr/share/applications/google-chrome.desktop".to_string()),
            auto_start: true,
            icon: None,
        };
        f.upsert_app("chrome", scanned.clone()).unwrap();
        let apps = f.apps("chrome").unwrap();
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0], scanned);
        assert!(apps[0].auto_start);
    }

    #[test]
    fn test_set_auto_start_off_scan_app_removed() {
        let dir = TempDir::new().unwrap();
        let f = test_file(&dir);
        let a = app("/usr/share/applications/x.desktop", "X", true);
        f.set_auto_start("c", a.clone(), true).unwrap();
        f.set_auto_start("c", a.clone(), false).unwrap();
        assert!(f.apps("c").unwrap().is_empty()); // 扫描应用 absence = false
    }

    #[test]
    fn test_set_auto_start_off_custom_kept() {
        let dir = TempDir::new().unwrap();
        let f = test_file(&dir);
        let a = app("custom:MyApp", "MyApp", true);
        f.set_auto_start("c", a.clone(), true).unwrap();
        f.set_auto_start("c", a.clone(), false).unwrap();
        let apps = f.apps("c").unwrap();
        assert_eq!(apps.len(), 1);
        assert!(!apps[0].auto_start); // custom 保留但关掉
    }

    #[test]
    fn test_add_custom_dup_rejected() {
        let dir = TempDir::new().unwrap();
        let f = test_file(&dir);
        f.add_custom("c", "MyApp", "myapp").unwrap();
        assert!(f.add_custom("c", "MyApp", "myapp").is_err());
    }

    #[test]
    fn test_remove_app() {
        let dir = TempDir::new().unwrap();
        let f = test_file(&dir);
        f.add_custom("c", "MyApp", "myapp").unwrap();
        f.remove_app("c", "custom:MyApp").unwrap();
        assert!(f.apps("c").unwrap().is_empty());
    }
}
