//! root-channel 专用载荷（`easytidy-dock` daemon ↔ client 进程）。
//!
//! 与 server 协议共用帧层（`frame.rs`）与握手（`message.rs` `hello`），
//! op 名前缀 `rc.`。语义：每容器一个**共享 root shell**（宿主 rootless
//! socket `exec --user 0` 持有，容器内 exec 进程父 = conmon，容器销毁时
//! 由 conmon 回收）；多个客户端可 attach 同一会话（ring buffer 回放）。
//!
//! 生命周期：
//! - attach：订阅共享会话（回放 + 流式）；客户端断开 = **detach**（仅退订，
//!   root shell 与 root-channel 继续运行）
//! - close：用户主动关闭 = kill 容器内 root shell + root-channel 进程退出
//! - 容器销毁 = exec 流 EOF = 广播 `rc.exited` 事件 + root-channel 自退

use serde::{Deserialize, Serialize};

/// 共享 root 会话的流 ID 常量（`Frame::Raw` 的 stream_id 恒为此值；
/// 沿用旧 GUI `EXEC_STREAM_ID_BASE = 1 << 30` 的段位，与 server PTY
/// 动态分配的 stream_id 空间不撞）。
pub const ROOT_STREAM_ID: u32 = 1 << 30;

/// 挂载 root 会话（客户端 → root-channel daemon）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RcAttach {
    /// 终端列数
    pub cols: u16,
    /// 终端行数
    pub rows: u16,
    /// 要 attach 的 session_id（daemon 内）
    #[serde(default)]
    pub session_id: u64,
}

/// RcAttach 响应：root shell 存活状态（false = 会话已死，不应再渲染输入）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RcAttachAck {
    /// 流 ID（恒为 [`ROOT_STREAM_ID`]）
    pub stream_id: u32,
    /// root shell 是否存活
    pub alive: bool,
}

/// 调整 root 会话 TTY 尺寸（转发 `resize_exec`）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RcResize {
    pub cols: u16,
    pub rows: u16,
}

/// 主动关闭（kill 容器内 root shell + root-channel 退出）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RcClose;

/// 主动关闭指定 session（client → daemon，standalone 不 attach）。
///
/// 与 attach 路径内的 `rc.close` 不同：这是独立的一次性命令（`client close
/// --session-id N`），GUI 点终端"关闭"按钮时经它 kill 后台 bash。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RcCloseReq {
    /// 要关闭的 daemon session_id
    pub session_id: u64,
}

/// 存活探测（root_terminal_status 用；无需 attach 订阅）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RcPing;

/// RcPing 响应
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RcPingResp {
    /// root shell 是否存活
    pub alive: bool,
}

/// 新建 root 会话（client → daemon）：分配新 session、fork bash、返回 session_id
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RcNew {
    pub cols: u16,
    pub rows: u16,
}

/// RcNew 响应
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RcNewAck {
    /// daemon 内 session id（attach 用）
    pub session_id: u64,
    /// bash 是否存活（false = 创建后立即退出）
    pub alive: bool,
    /// 流 ID（恒为 [`ROOT_STREAM_ID`]）
    pub stream_id: u32,
}

/// 会话列表条目（root-channel 多 session 模型；rc.list 响应用）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: u64,
    pub alive: bool,
    pub spawn_pid: u32,
}

/// RcList 响应
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RcListResp {
    pub sessions: Vec<SessionInfo>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_root_stream_id_segment() {
        assert_eq!(ROOT_STREAM_ID, 1 << 30);
    }

    #[test]
    fn test_rc_attach_serde() {
        let op = RcAttach { cols: 80, rows: 24, session_id: 0 };
        let json = serde_json::to_string(&op).unwrap();
        assert_eq!(serde_json::from_str::<RcAttach>(&json).unwrap(), op);
    }

    #[test]
    fn test_rc_resize_close_ping_serde() {
        let r = RcResize { cols: 120, rows: 40 };
        let j = serde_json::to_string(&r).unwrap();
        assert_eq!(serde_json::from_str::<RcResize>(&j).unwrap(), r);

        let c = RcClose;
        let j = serde_json::to_string(&c).unwrap();
        assert_eq!(j, "null"); // unit struct → JSON null

        let p = RcPing;
        let j = serde_json::to_string(&p).unwrap();
        assert_eq!(j, "null");

        let ack = RcAttachAck { stream_id: ROOT_STREAM_ID, alive: true };
        let j = serde_json::to_string(&ack).unwrap();
        assert_eq!(serde_json::from_str::<RcAttachAck>(&j).unwrap(), ack);

        let pr = RcPingResp { alive: true };
        let j = serde_json::to_string(&pr).unwrap();
        assert_eq!(serde_json::from_str::<RcPingResp>(&j).unwrap(), pr);
    }
}
