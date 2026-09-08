//! Passthrough 应用拉起（宿主侧经容器 server socket）。
//!
//! 配置（应用列表 + auto-start + 收藏 pinned）**存容器内**
//! `{home}/.easytidy/passthrough.toml`，由容器内 server 自读自管，
//! 容器自包含——容器删除/同名重建即随之清空。宿主侧不再持有 per-container
//! 配置（修复「新建容器继承旧容器收藏」bug）。
//!
//! 宿主侧仅负责经容器 server socket 发 `apps.launch`（按命令串，auto-start/
//! 收藏）或 `apps.launch_app`（按 id/名称，server 查登记表自行决定启动）
//! ——**由 server 拉起并保活**
//! （宿主一次性 CLI 连接断开即杀 PTY 会话，实测；server spawn 的子进程
//! 独立于连接存活）。

use easytidy_protocol::ops::{
    AppLaunchApp, AppLaunchAppResp, AppLogs, AppLogsResp, AppsLaunch, AppsLaunchItem,
    AppsLaunchResp, AppsLaunchResult, AppsPsResp, ManagedProcess,
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

/// 按引用拉起应用（apps.launch_app：server 查应用登记表/自定义应用解析
/// exec 并自行 spawn——调用方只传 id 或名称）。连接重试 2s 窗口容忍
/// server 就绪延迟。
pub async fn launch_app_by_ref(
    container: &str,
    id_or_name: &str,
) -> Result<AppLaunchAppResp> {
    let mut framed = connect_apps(container).await?;

    let launch = Frame::Json(Message {
        id: 2,
        kind: MsgKind::Req,
        op: "apps.launch_app".to_string(),
        payload: serde_json::to_value(AppLaunchApp { id_or_name: id_or_name.to_string() })
            .unwrap_or_default(),
        err: None,
    });
    if framed.send(launch).await.is_err() {
        return Err(Error::Connect("发送 apps.launch_app 失败".to_string()));
    }
    match framed.next().await {
        Some(Ok(Frame::Json(resp))) => {
            if let Some(err) = resp.err {
                return Err(Error::Connect(format!(
                    "apps.launch_app 失败：{} {}",
                    err.code, err.message
                )));
            }
            serde_json::from_value::<AppLaunchAppResp>(resp.payload)
                .map_err(|e| Error::Connect(format!("解析 apps.launch_app 响应失败：{e}")))
        }
        _ => Err(Error::Connect("apps.launch_app 响应异常".to_string())),
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
