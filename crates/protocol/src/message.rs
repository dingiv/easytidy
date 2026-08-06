//! 消息定义（JSON RPC 消息）。
//!
//! 所有 JSON 帧承载的都是 Message 类型，用于请求/响应/事件模式。

use serde::{Deserialize, Serialize};

/// RPC 消息
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    /// 消息 ID（请求与响应的关联键）
    pub id: u64,
    /// 消息类型
    pub kind: MsgKind,
    /// 操作名（对应 ops.rs 中的各结构体名）
    pub op: String,
    /// 载荷（各操作的具体参数）
    pub payload: serde_json::Value,
    /// 错误信息（仅响应时可能非空）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub err: Option<RpcError>,
}

/// 消息类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MsgKind {
    /// 请求
    Req,
    /// 响应
    Resp,
    /// 事件（server 主动推送）
    Evt,
}

/// RPC 错误
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    /// 错误码（字符串枚举，如 "not_found", "permission_denied"）
    pub code: String,
    /// 人类可读错误信息
    pub message: String,
}

/// 握手消息（客户端 → server）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Handshake {
    /// 协议版本号
    pub v: u32,
    /// 客户端标识（如 "easytidy-gui", "easytidy-cli"）
    pub client: String,
    /// 客户端请求的能力列表（如 ["pty", "fs", "apps"]）
    pub wants: Vec<String>,
}

/// 握手确认（server → 客户端）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HandshakeAck {
    /// 协议版本号（server 支持的最高版本）
    pub v: u32,
    /// 服务端标识（如 "easytidy-server"）
    pub server: String,
    /// 服务端支持的能力列表
    pub capabilities: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_serde_roundtrip() {
        let msg = Message {
            id: 42,
            kind: MsgKind::Req,
            op: "pty_open".to_string(),
            payload: serde_json::json!({
                "cmd": "/bin/bash",
                "argv": ["bash"],
                "env": {},
                "cwd": "/home/user",
                "cols": 80,
                "rows": 24
            }),
            err: None,
        };

        let json = serde_json::to_string(&msg).expect("serialize failed");
        let decoded: Message = serde_json::from_str(&json).expect("deserialize failed");

        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_message_with_error() {
        let msg = Message {
            id: 10,
            kind: MsgKind::Resp,
            op: "fs_read".to_string(),
            payload: serde_json::Value::Null,
            err: Some(RpcError {
                code: "not_found".to_string(),
                message: "file not found".to_string(),
            }),
        };

        let json = serde_json::to_string(&msg).expect("serialize failed");
        let decoded: Message = serde_json::from_str(&json).expect("deserialize failed");

        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_handshake_serde() {
        let hs = Handshake {
            v: 1,
            client: "easytidy-gui".to_string(),
            wants: vec!["pty".to_string(), "fs".to_string(), "apps".to_string()],
        };

        let json = serde_json::to_string(&hs).expect("serialize failed");
        let decoded: Handshake = serde_json::from_str(&json).expect("deserialize failed");

        assert_eq!(hs, decoded);
    }

    #[test]
    fn test_handshake_ack_serde() {
        let ack = HandshakeAck {
            v: 1,
            server: "easytidy-server".to_string(),
            capabilities: vec![
                "pty".to_string(),
                "fs".to_string(),
                "apps".to_string(),
                "passthrough".to_string(),
                "config".to_string(),
                "lifecycle".to_string(),
            ],
        };

        let json = serde_json::to_string(&ack).expect("serialize failed");
        let decoded: HandshakeAck = serde_json::from_str(&json).expect("deserialize failed");

        assert_eq!(ack, decoded);
    }

    #[test]
    fn test_msg_kind_serialization() {
        use serde_json::json;
        assert_eq!(serde_json::to_value(MsgKind::Req).unwrap(), json!("req"));
        assert_eq!(serde_json::to_value(MsgKind::Resp).unwrap(), json!("resp"));
        assert_eq!(serde_json::to_value(MsgKind::Evt).unwrap(), json!("evt"));

        assert_eq!(serde_json::from_str::<MsgKind>(r#""req""#).unwrap(), MsgKind::Req);
        assert_eq!(serde_json::from_str::<MsgKind>(r#""resp""#).unwrap(), MsgKind::Resp);
        assert_eq!(serde_json::from_str::<MsgKind>(r#""evt""#).unwrap(), MsgKind::Evt);
    }
}
