//! easytidy 线协议（socket 协议 v0）。
//!
//! 定义宿主侧（GUI / CLI）与容器内 server 之间的 wire format：
//! 长度前缀分帧 + 1 字节判别符（`0x01` JSON 消息 / `0x02` 原始流数据），
//! 以及全部消息族（pty / fs / apps / passthrough / config / lifecycle）。
//!
//! 详见 docs/08-requirements.md「Socket 协议 v0」与实施计划。

pub const PROTOCOL_VERSION: u32 = 1;

/// 帧判别符：JSON 消息
pub const FRAME_JSON: u8 = 0x01;
/// 帧判别符：原始流数据（PTY I/O，stream_id u32 BE + 字节）
pub const FRAME_RAW: u8 = 0x02;
