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

use easytidy_protocol::ops::{
    AppLogs, AppLogsResp, AppsLaunch, AppsLaunchItem, AppsLaunchResp, AppsLaunchResult, AppsPsResp,
    ManagedProcess,
};
use easytidy_protocol::frame::FrameCodec;
use easytidy_protocol::{Frame, Handshake, Message, MsgKind, PROTOCOL_VERSION};
use futures::{SinkExt, StreamExt};
use serde_json::json;
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

/// 宿主侧 passthrough 配置（只留**收藏** pinned；每容器应用列表 + auto-start
/// 已随「配置入容器」迁到容器内 `/home/easytidy/.config/easytidy/passthrough.toml`，
/// 由 server 自读拉起）。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PassthroughConfig {
    pub schema_version: u32,
    /// 收藏（pin 到 GUI 工具栏）：按容器分组的应用列表，顺序 = 显示顺序
    #[serde(default)]
    pub pinned: HashMap<String, Vec<PassthroughApp>>,
}

impl PassthroughConfig {
    pub fn new() -> Self {
        Self {
            schema_version: 1,
            pinned: HashMap::new(),
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

    /// 读取某容器的收藏（pin）列表（无条目返回空；顺序 = pin 顺序）
    pub fn pinned(&self, container: &str) -> Result<Vec<PassthroughApp>> {
        let config = self.load()?;
        Ok(config.pinned.get(container).cloned().unwrap_or_default())
    }

    /// 收藏（pin）应用：按 id upsert（保持顺序），容器条目空则移除键。
    pub fn pin_app(&self, container: &str, app: PassthroughApp) -> Result<()> {
        let mut config = self.load()?;
        let pinned = config.pinned.entry(container.to_string()).or_default();
        if let Some(existing) = pinned.iter_mut().find(|a| a.id == app.id) {
            *existing = app;
        } else {
            pinned.push(app);
        }
        self.save(&config)
    }

    /// 取消收藏（unpin）：移除指定应用；条目空则移除键。
    pub fn unpin_app(&self, container: &str, id: &str) -> Result<()> {
        let mut config = self.load()?;
        let Some(pinned) = config.pinned.get_mut(container) else {
            return Ok(());
        };
        pinned.retain(|a| a.id != id);
        if pinned.is_empty() {
            config.pinned.remove(container);
        }
        self.save(&config)
    }
}

/// 连接到容器 server 并完成 apps 握手（apps.launch/ps/logs 共用）。
///
/// 连接重试 2s 窗口容忍 server 就绪延迟。
async fn connect_apps(container: &str) -> Result<Framed<UnixStream, FrameCodec>> {
    let socket_path = crate::host_socket_path(container)?;

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
    let mut framed = framed.ok_or_else(|| {
        Error::Connect(format!("连接容器 server 超时：{}", socket_path.display()))
    })?;

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
        return Err(Error::Connect("发送握手失败".to_string()));
    }
    match framed.next().await {
        Some(Ok(Frame::Json(_))) => {}
        _ => return Err(Error::Connect("握手未确认".to_string())),
    }

    Ok(framed)
}

/// 经 server socket 拉起应用（apps.launch；server spawn 的子进程独立于
/// 连接存活）。连接重试 2s 窗口容忍 server 就绪延迟；返回逐条结果
/// （成功 pid / 失败 error）。
pub async fn launch_apps(
    container: &str,
    apps: &[PassthroughApp],
) -> Result<Vec<AppsLaunchResult>> {
    if apps.is_empty() {
        return Ok(Vec::new());
    }

    let mut framed = connect_apps(container).await?;

    // apps.launch
    let launch = Frame::Json(Message {
        id: 2,
        kind: MsgKind::Req,
        op: "apps.launch".to_string(),
        payload: serde_json::to_value(AppsLaunch {
            apps: apps
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
        return Err(Error::Connect("发送 apps.launch 失败".to_string()));
    }
    match framed.next().await {
        Some(Ok(Frame::Json(resp))) => serde_json::from_value::<AppsLaunchResp>(resp.payload)
            .map(|r| r.results)
            .map_err(|e| Error::Connect(format!("解析 apps.launch 响应失败：{e}"))),
        _ => Err(Error::Connect("apps.launch 响应异常".to_string())),
    }
}

/// 查询 server 托管的进程列表（apps.ps：含运行中与最近退出的，带退出码
/// 与 stdio 长度）。用于拉起后检测即时退出（如命令不存在 → 127）。
pub async fn list_managed_processes(container: &str) -> Result<Vec<ManagedProcess>> {
    let mut framed = connect_apps(container).await?;
    let ps = Frame::Json(Message {
        id: 2,
        kind: MsgKind::Req,
        op: "apps.ps".to_string(),
        payload: json!(null),
        err: None,
    });
    if framed.send(ps).await.is_err() {
        return Err(Error::Connect("发送 apps.ps 失败".to_string()));
    }
    match framed.next().await {
        Some(Ok(Frame::Json(resp))) => {
            if let Some(err) = resp.err {
                return Err(Error::Connect(format!(
                    "apps.ps 失败：{} {}",
                    err.code, err.message
                )));
            }
            serde_json::from_value::<AppsPsResp>(resp.payload)
                .map(|r| r.processes)
                .map_err(|e| Error::Connect(format!("解析 apps.ps 响应失败：{e}")))
        }
        _ => Err(Error::Connect("apps.ps 响应异常".to_string())),
    }
}

/// 获取某托管进程捕获的 stdio（apps.logs：stdout+stderr 合并的有界缓冲）。
pub async fn fetch_process_logs(container: &str, pid: u32) -> Result<String> {
    let mut framed = connect_apps(container).await?;
    let logs = Frame::Json(Message {
        id: 2,
        kind: MsgKind::Req,
        op: "apps.logs".to_string(),
        payload: serde_json::to_value(AppLogs { pid }).unwrap_or_default(),
        err: None,
    });
    if framed.send(logs).await.is_err() {
        return Err(Error::Connect("发送 apps.logs 失败".to_string()));
    }
    match framed.next().await {
        Some(Ok(Frame::Json(resp))) => {
            if let Some(err) = resp.err {
                return Err(Error::Connect(format!(
                    "apps.logs 失败：{} {}",
                    err.code, err.message
                )));
            }
            serde_json::from_value::<AppLogsResp>(resp.payload)
                .map(|r| r.stdio)
                .map_err(|e| Error::Connect(format!("解析 apps.logs 响应失败：{e}")))
        }
        _ => Err(Error::Connect("apps.logs 响应异常".to_string())),
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
    fn test_pinned_roundtrip() {
        // 宿主配置只存收藏(pinned)；每容器应用列表+auto-start 已迁容器内
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
        f.pin_app("chrome", scanned.clone()).unwrap();
        let pinned = f.pinned("chrome").unwrap();
        assert_eq!(pinned.len(), 1);
        assert_eq!(pinned[0], scanned);
    }

    #[test]
    fn test_pin_unpin() {
        let dir = TempDir::new().unwrap();
        let f = test_file(&dir);
        let a = app("chrome", "Chrome", false);

        // pin：按 id upsert 保持顺序
        f.pin_app("c", a.clone()).unwrap();
        f.pin_app("c", a.clone()).unwrap(); // 重复 pin = upsert
        let pinned = f.pinned("c").unwrap();
        assert_eq!(pinned.len(), 1);
        assert_eq!(pinned[0].id, "chrome");

        // 多容器隔离
        f.pin_app("other", a.clone()).unwrap();
        assert_eq!(f.pinned("c").unwrap().len(), 1);

        // unpin：移除；条目空则移除键
        f.unpin_app("c", "chrome").unwrap();
        assert!(f.pinned("c").unwrap().is_empty());
    }

}
