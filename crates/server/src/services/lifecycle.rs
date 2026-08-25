//! 生命周期服务：entry 拉起 / shutdown / 优雅退出。
use crate::services::apps::spawn_managed_process;
use crate::state::{ProcessStatus, ServerState};

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use easytidy_protocol::{
    Frame, LifecycleEntryLaunch, Message, MsgKind, ShutdownAck,
};
use serde_json::json;
use tokio::process::Command as TokioCommand;
use tokio::sync::mpsc;
use tracing::{info, warn};

/// Handle lifecycle.entryLaunch
pub(crate) async fn handle_lifecycle_entry_launch(
    msg: Message,
    state: &Arc<ServerState>,
    event_tx: mpsc::UnboundedSender<Frame>,
) -> Result<Frame> {
    let req: LifecycleEntryLaunch = serde_json::from_value(msg.payload)
        .context("Failed to parse LifecycleEntryLaunch")?;

    // TODO: Look up entry config and spawn the entry process。
    // 现阶段：登录 shell 占位；entry 已移入容器内由 server 管理
    // （宿主 --entry 被忽略，应用经 passthrough auto-start 拉起）
    info!("Entry launch requested: {}", req.entry_id);
    let pid =
        spawn_managed_process(state, "/bin/sh -l", "entry", req.entry_id.clone(), Some(event_tx.clone()))
            .await?;

    // Emit entry.started event
    let msg_id = state.next_msg_id.fetch_add(1, Ordering::SeqCst) as u64;
    event_tx.send(Frame::Json(Message {
        id: msg_id,
        kind: MsgKind::Evt,
        op: "entry.started".to_string(),
        payload: json!({ "pid": pid }),
        err: None,
    }))?;

    Ok(Frame::Json(Message {
        id: msg.id,
        kind: MsgKind::Resp,
        op: "lifecycle.entryLaunch".to_string(),
        payload: json!(null),
        err: None,
    }))
}

/// Handle lifecycle.shutdown
pub(crate) async fn handle_lifecycle_shutdown(
    msg: Message,
    state: &Arc<ServerState>,
) -> Result<Frame> {
    info!("Shutdown requested by client");

    state.shutting_down.store(true, Ordering::SeqCst);

    Ok(Frame::Json(Message {
        id: msg.id,
        kind: MsgKind::Resp,
        op: "lifecycle.shutdown".to_string(),
        payload: serde_json::to_value(ShutdownAck)?,
        err: None,
    }))
}

/// Perform graceful shutdown
pub(crate) async fn perform_graceful_shutdown(state: Arc<ServerState>) -> Result<()> {
    // Give children time to exit gracefully
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Force kill remaining children（仅仍运行的；已退出条目留待 apps.ps 展示）
    let children = state.children.read().await;
    for (pid, child_info) in children.iter() {
        if child_info.status != ProcessStatus::Running {
            continue;
        }
        info!("Killing child: pid={}, kind={}", pid, child_info.kind);
        if let Err(e) = TokioCommand::new("kill")
            .arg("-SIGKILL")
            .arg(pid.to_string())
            .status()
            .await
        {
            warn!("Failed to kill child {}: {}", pid, e);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    // 三层重构（connection/router/services）后测试从旧 main.rs 移植：
    // 各服务函数已分家到 services::{fs,apps,config}，此处显式引回
    use std::fs;

    use super::*;
    use crate::services::apps::parse_desktop_file;
    use crate::services::config::handle_config_get;
    use crate::services::fs::handle_fs_list;
    use crate::setup::*;
    use easytidy_protocol::ops::{CfgGetResp, FsList, FsListResp};
    use easytidy_protocol::{FrameCodec, Handshake, HandshakeAck, PROTOCOL_VERSION};
    use futures::{SinkExt, StreamExt};
    use tempfile::NamedTempFile;
    use tokio::net::{UnixListener, UnixStream};
    use tokio_util::codec::Framed;

    /// Test handshake roundtrip
    #[tokio::test]
    async fn test_handshake_roundtrip() {
        // Create a temporary socket
        let temp_dir = tempfile::tempdir().unwrap();
        let socket_path = temp_dir.path().join("test.sock");

        // Spawn server in background
        let socket_path_clone = socket_path.clone();
        tokio::spawn(async move {
            let listener = UnixListener::bind(&socket_path_clone).unwrap();
            if let Ok((stream, _)) = listener.accept().await {
                let mut framed = Framed::new(stream, FrameCodec::new());
                // Accept one frame and respond
                if let Some(Ok(Frame::Json(msg))) = framed.next().await {
                    if msg.op == "hello" {
                        let ack = Frame::Json(Message {
                            id: msg.id,
                            kind: MsgKind::Resp,
                            op: "hello".to_string(),
                            payload: serde_json::to_value(HandshakeAck {
                                v: PROTOCOL_VERSION,
                                server: "easytidy-server".to_string(),
                                capabilities: vec!["pty".to_string()],
                                session_id: "test-session".to_string(),
                            }).unwrap(),
                            err: None,
                        });
                        let _ = framed.send(ack).await;
                    }
                }
            }
        });

        // Give server time to start
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Connect as client
        let stream = UnixStream::connect(&socket_path).await.unwrap();
        let mut framed = Framed::new(stream, FrameCodec::new());

        // Send handshake
        let handshake = Frame::Json(Message {
            id: 1,
            kind: MsgKind::Req,
            op: "hello".to_string(),
            payload: serde_json::to_value(Handshake {
                v: PROTOCOL_VERSION,
                client: "test-client".to_string(),
                wants: vec!["pty".to_string()],
            }).unwrap(),
            err: None,
        });

        framed.send(handshake).await.unwrap();

        // Receive response
        if let Some(Ok(Frame::Json(resp))) = framed.next().await {
            assert_eq!(resp.op, "hello");
            assert_eq!(resp.kind, MsgKind::Resp);
        } else {
            panic!("Expected handshake response");
        }
    }

    /// Test FS list
    #[tokio::test]
    async fn test_fs_list() {
        // Create a temporary directory with some files
        let temp_dir = tempfile::tempdir().unwrap();
        let temp_file = temp_dir.path().join("test.txt");
        fs::write(&temp_file, b"test content").unwrap();

        let req = Message {
            id: 1,
            kind: MsgKind::Req,
            op: "fs.list".to_string(),
            payload: serde_json::to_value(FsList {
                path: temp_dir.path().to_str().unwrap().to_string(),
            }).unwrap(),
            err: None,
        };

        let msg = handle_fs_list(req).await.unwrap();
        if let Frame::Json(resp) = msg {
            assert_eq!(resp.op, "fs.list");
            if let Ok(list_resp) = serde_json::from_value::<FsListResp>(resp.payload) {
                assert!(!list_resp.entries.is_empty());
            } else {
                panic!("Failed to parse FsListResp");
            }
        } else {
            panic!("Expected JSON frame");
        }
    }

    /// Test apps list parsing
    #[test]
    fn test_parse_desktop_file() {
        let temp_file = NamedTempFile::new().unwrap();
        let desktop_path = temp_file.path().with_extension("desktop");

        let content = r#"[Desktop Entry]
Name=Test App
Exec=test-app --option
Comment=A test application
Icon=test-icon
"#;

        fs::write(&desktop_path, content).unwrap();

        let app = parse_desktop_file(&desktop_path).unwrap();
        assert_eq!(app.name, "Test App");
        assert_eq!(app.exec, "test-app --option");
        assert_eq!(app.comment, Some("A test application".to_string()));
        assert_eq!(app.icon_path, Some("test-icon".to_string()));
    }

    /// 单引号转义：普通 / 含单引号 / 空串 / 含空白与变量 / unicode
    #[test]
    fn test_shell_escape_single_quote() {
        assert_eq!(shell_escape_single_quote("whoami"), "'whoami'");
        assert_eq!(shell_escape_single_quote("a'b"), "'a'\\''b'");
        assert_eq!(shell_escape_single_quote(""), "''");
        assert_eq!(shell_escape_single_quote("echo $HOME"), "'echo $HOME'");
        assert_eq!(shell_escape_single_quote("中文"), "'中文'");
    }

    /// su -c 完整命令拼接：cmd + argv[1..]（argv[0] == cmd 时与整体 argv 拼接等价）
    #[test]
    fn test_build_su_command() {
        // CLI 形态：easytidy run --container n -- sh -c 'echo $HOME'
        assert_eq!(
            build_su_command(
                "sh",
                &["sh".to_string(), "-c".to_string(), "echo $HOME".to_string()]
            ),
            "'sh' '-c' 'echo $HOME'"
        );
        // 单命令无参
        assert_eq!(build_su_command("whoami", &[]), "'whoami'");
        // 命令内含单引号（如 grep 'a b'）
        assert_eq!(
            build_su_command(
                "bash",
                &["bash".to_string(), "-c".to_string(), "echo 'a b'".to_string()]
            ),
            "'bash' '-c' 'echo '\\''a b'\\'''"
        );
    }

    /// 环境变量解析：缺失任一 EASYTIDY_USER_* → None
    #[test]
    fn test_user_map_from_env_missing() {
        // 测试进程通常无 EASYTIDY_USER_*；即便宿主注入也逐项移除（并行测试安全：
        // 其他用例不读这些变量）
        for key in ["EASYTIDY_USER_NAME", "EASYTIDY_USER_UID", "EASYTIDY_USER_GID", "EASYTIDY_USER_HOME"] {
            std::env::remove_var(key);
        }
        assert!(user_map_from_env().is_none());
    }

    /// Test config get/set
    #[tokio::test]
    async fn test_config_get() {
        let req = Message {
            id: 1,
            kind: MsgKind::Req,
            op: "config.get".to_string(),
            payload: serde_json::json!({}),
            err: None,
        };

        let msg = handle_config_get(req).await.unwrap();
        if let Frame::Json(resp) = msg {
            assert_eq!(resp.op, "config.get");
            if let Ok(get_resp) = serde_json::from_value::<CfgGetResp>(resp.payload) {
                assert!(get_resp.config.is_object());
            } else {
                panic!("Failed to parse CfgGetResp");
            }
        } else {
            panic!("Expected JSON frame");
        }
    }
}
