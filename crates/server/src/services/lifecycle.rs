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

    /// fs.list 符号链接按解析后的目标类型分类（目录链接 → Dir 可导航，
    /// 文件链接 → File 带目标大小，坏链接 → Symlink）
    #[tokio::test]
    async fn test_fs_list_symlinks_resolved_by_target() {
        use std::os::unix::fs::symlink;

        let temp_dir = tempfile::tempdir().unwrap();
        fs::create_dir(temp_dir.path().join("realdir")).unwrap();
        fs::write(temp_dir.path().join("real.txt"), b"hello").unwrap();
        symlink(temp_dir.path().join("realdir"), temp_dir.path().join("dirlink")).unwrap();
        symlink(temp_dir.path().join("real.txt"), temp_dir.path().join("filelink")).unwrap();
        symlink(temp_dir.path().join("missing"), temp_dir.path().join("broken")).unwrap();

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
        let list_resp = if let Frame::Json(resp) = msg {
            serde_json::from_value::<FsListResp>(resp.payload).unwrap()
        } else {
            panic!("Expected JSON frame");
        };

        let by_name: std::collections::HashMap<String, easytidy_protocol::ops::FsEntry> =
            list_resp
                .entries
                .into_iter()
                .map(|e| (e.name.clone(), e))
                .collect();

        assert!(
            matches!(by_name["dirlink"].entry_type, easytidy_protocol::ops::FsEntryType::Dir),
            "目录符号链接应分类为 Dir（GUI 可导航）"
        );
        assert!(by_name["dirlink"].is_symlink, "目录链接应标记 is_symlink（图标区分）");
        assert!(
            matches!(by_name["filelink"].entry_type, easytidy_protocol::ops::FsEntryType::File),
            "文件符号链接应分类为 File（GUI 可打开）"
        );
        assert_eq!(by_name["filelink"].size, Some(5), "文件链接应显示目标大小");
        assert!(by_name["filelink"].is_symlink, "文件链接应标记 is_symlink（图标区分）");
        assert!(
            matches!(by_name["broken"].entry_type, easytidy_protocol::ops::FsEntryType::Symlink),
            "坏链接应保留 Symlink 类型"
        );
        assert!(by_name["broken"].is_symlink);
        assert!(
            matches!(by_name["realdir"].entry_type, easytidy_protocol::ops::FsEntryType::Dir)
        );
        assert!(!by_name["realdir"].is_symlink, "普通目录不应标记 is_symlink");
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

    /// 身份自发现：server 自身 uid 必有身份（名字可能为 uid<uid> 兜底）
    #[test]
    fn test_identity_self_discovery() {
        let (uid, gid) = easytidy_core::env::self_uid_gid();
        let id = easytidy_core::env::resolve_identity(
            "root:x:0:0:root:/root:/bin/sh\n",
            uid,
            gid,
            None,
        );
        assert_eq!(id.uid, uid);
        assert_eq!(id.gid, gid);
        assert!(!id.name.is_empty());
        assert!(!id.home.is_empty());
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
