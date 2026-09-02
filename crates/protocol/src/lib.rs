//! easytidy 线协议（socket 协议 v0）。
//!
//! 定义宿主侧（GUI / CLI）与容器内 server 之间的 wire format：
//! 长度前缀分帧 + 1 字节判别符（`0x01` JSON 消息 / `0x02` 原始流数据），
//! 以及全部消息族（pty / fs / apps / passthrough / config / lifecycle）。
//!
//! 另含 root-channel 专用载荷（`rc` 模块）：`easytidy-dock` client↔daemon
//! 进程与 GUI/CLI 客户端之间的 root 终端通道（同帧层、同握手）。
//!
//! 详见 docs/08-requirements.md「Socket 协议 v0」与实施计划。

/// v2（2026-08-27 身份模型重构）：删除 `PtyOpen.as_root` /
/// `PtyTerminalInfo.as_root`（root 终端改走宿主 root-channel，不经 server）。
pub const PROTOCOL_VERSION: u32 = 2;

/// 帧判别符：JSON 消息
pub const FRAME_JSON: u8 = 0x01;
/// 帧判别符：原始流数据（PTY I/O，stream_id u32 BE + 字节）
pub const FRAME_RAW: u8 = 0x02;

pub mod frame;
pub mod message;
pub mod ops;
pub mod rc;

pub use frame::{Frame, encode_frame, decode_frame, FrameCodec};
pub use message::{Message, MsgKind, RpcError, Handshake, HandshakeAck};
pub use ops::*;
