//! `ets` —— 容器内 easytidy-server 命令行客户端。
//!
//! 在容器内（`podman exec` 或直接进容器终端）与容器内 server 直接通信，
//! 不经过 podman / 宿主。默认连 `/run/easytidy/server.sock`（server socket
//! 目录，bind-mount 进容器）。
//!
//! 子命令（v1）：
//! - `ping`          连通性检查
//! - `launch <REF>`  拉起桌面应用（apps.launch_app，按 id 或名称）
//! - `pt list`       列出 passthrough 应用（容器内配置）
//! - `edit <PATH>`   打开文件：server 有存活 GUI 连接 → 通知 GUI 开编辑器；
//!   无 GUI → 当前终端起默认编辑器（$VISUAL/$EDITOR/vim/vi/nano）
//! - `raw <OP> [JSON]`  任意 op 透传（新 op 零维护覆盖）
//!
//! 输出：统一 pretty JSON（脚本友好）。

mod conn;

use std::future::Future;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use serde_json::Value;
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

use easytidy_protocol::FrameCodec;
use tokio::net::UnixStream;
use tokio_util::codec::Framed;

/// 默认 server socket 路径（容器内 /run/easytidy = 宿主 socket 目录 bind-mount）
const DEFAULT_SOCKET: &str = "/run/easytidy/server.sock";

#[derive(Parser)]
#[command(name = "ets", about = "容器内 easytidy-server 命令行客户端")]
struct Ets {
    /// server socket 路径（默认 $EASYTIDY_SOCKET 或 /run/easytidy/server.sock）
    #[arg(long, global = true)]
    socket: Option<PathBuf>,

    /// op 往返超时（秒）
    #[arg(long, global = true, default_value_t = 10)]
    timeout: u64,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 连通性检查（ping）
    Ping,
    /// 拉起桌面应用（按 id 或名称）
    Launch {
        /// 应用 id（pt-xxx）或名称
        id_or_name: String,
    },
    /// Passthrough 管理
    Pt {
        #[command(subcommand)]
        cmd: PtCmd,
    },
    /// 打开文件编辑：GUI 连着 → 通知 GUI 打开编辑器；否则当前终端起默认编辑器
    Edit {
        /// 容器内文件路径（~ 展开）
        path: String,
    },
    /// 任意 op 透传（payload 为任意 JSON，缺省 null）
    Raw {
        /// op 名（如 server.info / apps.ps / pty.list / fs.list …）
        op: String,
        /// 请求 payload（任意 JSON；缺省 null）
        #[arg(default_value = "null")]
        payload: String,
    },
}

#[derive(Subcommand)]
enum PtCmd {
    /// 列出 passthrough 应用（容器内配置：apps + pinned）
    List,
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("ets: {e:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let cli = Ets::parse();

    // 日志：默认静默（输出即数据）；RUST_LOG=debug 打开
    let _ = tracing_subscriber::registry()
        .with(EnvFilter::from_default_env().add_directive(tracing::Level::ERROR.into()))
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .try_init();

    let socket = cli
        .socket
        .or_else(|| std::env::var("EASYTIDY_SOCKET").ok().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_SOCKET));

    let framed = conn::connect_server(&socket, "ets").await
        .with_context(|| format!("连接 server 失败（{socket:?}）：容器 server 是否在运行？"))?;

    let timeout = Duration::from_secs(cli.timeout);
    match cli.cmd {
        Cmd::Ping => {
            let v = op(
                timeout,
                conn::send_json_op(framed, "ping", &Value::Null),
            )
            .await?;
            print_json(&v);
        }
        Cmd::Launch { id_or_name } => {
            let req = serde_json::json!({ "id_or_name": id_or_name });
            let v = op(timeout, conn::send_json_op(framed, "apps.launch_app", &req)).await?;
            print_json(&v);
            if v.get("error").is_some() {
                std::process::exit(1);
            }
        }
        Cmd::Pt {
            cmd: PtCmd::List,
        } => {
            let v = op(timeout, conn::send_json_op(framed, "passthrough.list", &Value::Null))
                .await?;
            print_json(&v);
        }
        Cmd::Edit { path } => cmd_edit(framed, timeout, path).await?,
        Cmd::Raw { op: op_name, payload } => {
            let payload: Value = serde_json::from_str(&payload)
                .with_context(|| "payload 不是合法 JSON")?;
            let v = op(timeout, conn::send_json_op(framed, &op_name, &payload)).await?;
            print_json(&v);
        }
    }
    Ok(())
}

/// edit：ui.edit 请求 → GUI 路由结果；无 GUI 时本地起默认编辑器（前台，
/// 继承 stdio，退出码透传）。
async fn cmd_edit(framed: Framed<UnixStream, FrameCodec>, timeout: Duration, path: String) -> Result<()> {
    let path = expand_tilde(&path);
    if path.trim().is_empty() {
        bail!("路径为空");
    }

    let req = serde_json::json!({ "path": path });
    let v = op(timeout, conn::send_json_op(framed, "ui.edit", &req))
        .await
        .with_context(|| "ui.edit 请求失败（server 版本过旧不支持该 op？）")?;

    let routed = v.get("routed").and_then(Value::as_bool).unwrap_or(false);
    if routed {
        println!("已通知 GUI 打开编辑器：{path}");
        return Ok(());
    }

    // 无 GUI 连接：当前终端起默认编辑器
    println!("GUI 未连接，使用终端默认编辑器：{path}");
    let editor = pick_editor()?;
    let mut parts = editor.split_whitespace();
    let bin = parts.next().context("编辑器命令为空")?;
    let mut cmd = Command::new(bin);
    for arg in parts {
        cmd.arg(arg);
    }
    cmd.arg(&path);
    let status = cmd
        .status()
        .with_context(|| format!("启动编辑器失败：{bin} {path}"))?;
    std::process::exit(status.code().unwrap_or(1));
}

/// 超时包装（op 往返超过 --timeout 报错）。
async fn op<T, F: Future<Output = Result<T>>>(timeout: Duration, fut: F) -> Result<T> {
    match tokio::time::timeout(timeout, fut).await {
        Ok(r) => r,
        Err(_) => bail!("op 往返超时（{}s）", timeout.as_secs()),
    }
}

/// 客户端展开 ~（$HOME）。
fn expand_tilde(p: &str) -> String {
    expand_tilde_with(std::env::var("HOME").as_deref().unwrap_or(""), p)
}

/// 展开 ~（纯函数，home 参数化便于测试）。
fn expand_tilde_with(home: &str, p: &str) -> String {
    if home.is_empty() {
        return p.to_string();
    }
    if p == "~" {
        home.to_string()
    } else if let Some(rest) = p.strip_prefix("~/") {
        format!("{home}/{rest}")
    } else {
        p.to_string()
    }
}

/// 默认编辑器选择（纯函数：优先序 $VISUAL → $EDITOR → 按序可用项；空串跳过）。
fn choose_editor(visual: Option<&str>, editor: Option<&str>, available: &[&str]) -> Option<String> {
    if let Some(v) = visual.filter(|v| !v.trim().is_empty()) {
        return Some(v.to_string());
    }
    if let Some(v) = editor.filter(|v| !v.trim().is_empty()) {
        return Some(v.to_string());
    }
    available.iter().copied().find(|a| !a.trim().is_empty()).map(|a| a.to_string())
}

/// 默认编辑器选择：$VISUAL → $EDITOR（可含参数）→ vim → vi → nano（PATH 探测）。
fn pick_editor() -> Result<String> {
    let visual = std::env::var("VISUAL").ok().filter(|v| !v.trim().is_empty());
    let editor = std::env::var("EDITOR").ok().filter(|v| !v.trim().is_empty());
    let available: Vec<&str> = ["vim", "vi", "nano"]
        .iter()
        .copied()
        .filter(|c| which(c))
        .collect();
    choose_editor(visual.as_deref(), editor.as_deref(), &available)
        .ok_or_else(|| anyhow::anyhow!("找不到可用编辑器（请设置 $EDITOR）"))
}

/// PATH 中是否存在可执行文件。
fn which(name: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| {
        let cand = dir.join(name);
        cand.metadata()
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    })
}

/// pretty JSON 输出。
fn print_json(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use easytidy_protocol::{Frame, HandshakeAck, Message, MsgKind, PROTOCOL_VERSION};
    use futures::{SinkExt, StreamExt};

    #[test]
    fn test_choose_editor_priority() {
        // $VISUAL 优先（可含参数）
        assert_eq!(
            choose_editor(Some("code -w"), Some("vim"), &["vim"]),
            Some("code -w".to_string())
        );
        // 空串跳过 → 落 $EDITOR
        assert_eq!(
            choose_editor(Some("  "), Some("nano"), &["vim"]),
            Some("nano".to_string())
        );
        // 两者皆空 → 按序可用项
        assert_eq!(
            choose_editor(None, None, &["", "vi", "nano"]),
            Some("vi".to_string())
        );
        // 全部无 → None
        assert_eq!(choose_editor(None, None, &[]), None);
    }

    #[test]
    fn test_expand_tilde_with() {
        assert_eq!(expand_tilde_with("/home/u", "~"), "/home/u");
        assert_eq!(expand_tilde_with("/home/u", "~/.bashrc"), "/home/u/.bashrc");
        assert_eq!(expand_tilde_with("/home/u", "/etc/hosts"), "/etc/hosts");
        // home 缺失 → 原样返回
        assert_eq!(expand_tilde_with("", "~/.bashrc"), "~/.bashrc");
    }

    /// mock server：bind 由调用方先行（就绪信号），每连接一个处理任务
    /// （探针连接无害：断开即退）。回握手 ack（版本 `version`）+ 任意 op 回
    /// {"op": <op>}。
    async fn mock_server(listener: tokio::net::UnixListener, version: u32) {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut framed = Framed::new(stream, FrameCodec::new());
                while let Some(Ok(Frame::Json(msg))) = framed.next().await {
                    let resp = match msg.op.as_str() {
                        "hello" => Message {
                            id: msg.id,
                            kind: MsgKind::Resp,
                            op: "hello".to_string(),
                            payload: serde_json::to_value(HandshakeAck {
                                v: version,
                                server: "mock".to_string(),
                                capabilities: vec![],
                                session_id: "mock-session".to_string(),
                            })
                            .unwrap(),
                            err: None,
                        },
                        other => Message {
                            id: msg.id,
                            kind: MsgKind::Resp,
                            op: other.to_string(),
                            payload: serde_json::json!({ "op": other }),
                            err: None,
                        },
                    };
                    if framed.send(Frame::Json(resp)).await.is_err() {
                        break;
                    }
                    if msg.op != "hello" {
                        break;
                    }
                }
            });
        }
    }

    /// 集成：connect_server 握手 + one-shot op 往返（进程内 mock server）。
    #[tokio::test]
    async fn test_connect_and_ping_against_mock() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("server.sock");
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        tokio::spawn(mock_server(listener, PROTOCOL_VERSION));

        let framed = match conn::connect_server(&sock, "ets").await {
            Ok(f) => f,
            Err(e) => panic!("连接 mock server 失败：{e}"),
        };
        let v = conn::send_json_op(framed, "ping", &Value::Null).await.unwrap();
        assert_eq!(v, serde_json::json!({ "op": "ping" }));
    }

    /// 集成：协议版本不匹配 → 握手报版本错。
    #[tokio::test]
    async fn test_version_mismatch_reported() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("server.sock");
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        tokio::spawn(mock_server(listener, PROTOCOL_VERSION + 1));

        let err = match conn::connect_server(&sock, "ets").await {
            Ok(_) => panic!("应当报版本不匹配错误"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("协议版本不匹配"));
    }
}
