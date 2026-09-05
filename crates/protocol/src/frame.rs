//! 帧定义与 codec（长度前缀分帧 + 判别符）。
//!
//! 帧格式：
//! - 4 字节**大端（网络序）**长度（不包括长度字段本身）——`LengthDelimitedCodec`
//!   默认大端，两端用同一 codec 故内部通信正常；抓包/第三方工具须按大端解析
//! - 1 字节判别符（FRAME_JSON / FRAME_RAW）
//! - N 字节载荷（≤ [`MAX_FRAME_LEN`]）
//!
//! JSON 帧载荷：JSON 文本（UTF-8）
//! RAW 帧载荷：u32 大端序 stream_id + 原始字节
//!
//! 帧长上限 [`MAX_FRAME_LEN`]（64 MiB）：超过时 decode 报 `InvalidData`。
//! 调大到 64 MiB 是因为 `FsRead` 一次性读整个文件并以 Base64 回传——8 MiB
//! 默认上限下读 ~6MB 文件（Base64 后 >8MiB）会被静默拒绝。大文件读取宜分块
//! （`offset`/`len`），此处仅放宽上限兜底。

use bytes::{Buf, BufMut, BytesMut};
use serde::{Deserialize, Serialize};
use tokio_util::codec::{Decoder, Encoder, LengthDelimitedCodec};

use crate::message::Message;
use crate::{FRAME_JSON, FRAME_RAW};

/// 单帧最大载荷（不含 4 字节长度前缀）。64 MiB。
pub const MAX_FRAME_LEN: usize = 64 * 1024 * 1024;

/// 协议帧
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Frame {
    /// JSON 消息帧（RPC 请求/响应/事件）
    Json(Message),
    /// 原始数据流帧（PTY I/O 等）
    Raw { stream_id: u32, data: Vec<u8> },
}

/// 帧编码器/解码器
pub struct FrameCodec {
    length_delimited: LengthDelimitedCodec,
}

impl FrameCodec {
    pub fn new() -> Self {
        Self {
            // 显式放宽 max_frame_length（默认 8 MiB 会拒大 Base64 帧，见模块 doc）
            length_delimited: LengthDelimitedCodec::builder()
                .max_frame_length(MAX_FRAME_LEN)
                .new_codec(),
        }
    }
}

impl Default for FrameCodec {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder for FrameCodec {
    type Item = Frame;
    type Error = std::io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        // 先读取长度前缀的帧
        let Some(mut frame_bytes) = self.length_delimited.decode(src)? else {
            return Ok(None);
        };

        // 读取判别符
        if frame_bytes.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "frame discriminator missing",
            ));
        }
        let discriminator = frame_bytes.get_u8();

        match discriminator {
            FRAME_JSON => {
                // JSON 帧：直接解析 Message
                let json_str = std::str::from_utf8(&frame_bytes)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
                let msg = serde_json::from_str(json_str)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
                Ok(Some(Frame::Json(msg)))
            }
            FRAME_RAW => {
                // RAW 帧：stream_id (u32 BE) + data
                if frame_bytes.len() < 4 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "raw frame too short for stream_id",
                    ));
                }
                let stream_id = frame_bytes.get_u32();
                let data = frame_bytes.to_vec();
                Ok(Some(Frame::Raw { stream_id, data }))
            }
            other => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unknown frame discriminator: 0x{:02x}", other),
            )),
        }
    }
}

impl Encoder<Frame> for FrameCodec {
    type Error = std::io::Error;

    fn encode(&mut self, item: Frame, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let mut payload = BytesMut::new();

        match item {
            Frame::Json(msg) => {
                // JSON 帧：判别符 + JSON 文本
                payload.put_u8(FRAME_JSON);
                let json_bytes = serde_json::to_vec(&msg)
                    .map_err(std::io::Error::other)?;
                payload.extend_from_slice(&json_bytes);
            }
            Frame::Raw { stream_id, data } => {
                // RAW 帧：判别符 + stream_id (u32 BE) + data
                payload.put_u8(FRAME_RAW);
                payload.put_u32(stream_id);
                payload.extend_from_slice(&data);
            }
        }

        // 使用 LengthDelimitedCodec 处理长度前缀
        self.length_delimited.encode(payload.freeze(), dst)
    }
}

/// 便捷函数：编码单帧为 BytesMut
pub fn encode_frame(frame: Frame) -> BytesMut {
    let mut codec = FrameCodec::new();
    let mut dst = BytesMut::new();
    codec.encode(frame, &mut dst).expect("frame encoding failed");
    dst
}

/// 便捷函数：从字节解码单帧。
///
/// 返回 `Ok(None)` = 数据不足（再等几个字节）；`Ok(Some(frame))` = 解出一帧；
/// `Err` = 协议违例（未知判别符 / 坏 JSON / raw 帧过短 / 超 [`MAX_FRAME_LEN`]）。
/// 调用方须区分「再等」与「对端发了非法帧」——后者不应继续等。
pub fn decode_frame(src: &mut BytesMut) -> Result<Option<Frame>, std::io::Error> {
    let mut codec = FrameCodec::new();
    codec.decode(src)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Message, MsgKind};

    #[test]
    fn test_json_frame_roundtrip() {
        let msg = Message {
            id: 1,
            kind: MsgKind::Req,
            op: "ping".to_string(),
            payload: serde_json::json!({ "value": 42 }),
            err: None,
        };
        let frame = Frame::Json(msg.clone());

        let encoded = encode_frame(frame.clone());
        let mut decoded_src = encoded.clone();
        let decoded = decode_frame(&mut decoded_src).unwrap().expect("decode failed");

        assert_eq!(frame, decoded);
    }

    #[test]
    fn test_raw_frame_roundtrip() {
        let frame = Frame::Raw {
            stream_id: 123,
            data: vec![0x01, 0x02, 0x03, 0x04],
        };

        let encoded = encode_frame(frame.clone());
        let mut decoded_src = encoded.clone();
        let decoded = decode_frame(&mut decoded_src).unwrap().expect("decode failed");

        assert_eq!(frame, decoded);
    }

    #[test]
    fn test_empty_raw_frame() {
        let frame = Frame::Raw {
            stream_id: 0,
            data: vec![],
        };

        let encoded = encode_frame(frame.clone());
        let mut decoded_src = encoded.clone();
        let decoded = decode_frame(&mut decoded_src).unwrap().expect("decode failed");

        assert_eq!(frame, decoded);
    }

    #[test]
    fn test_incremental_decode() {
        // 测试使用同一个 codec 实例进行增量解码
        let frame = Frame::Raw {
            stream_id: 999,
            data: vec![0xAA; 100],
        };
        let encoded = encode_frame(frame.clone());

        let mut codec = FrameCodec::new();
        let mut buffer = BytesMut::new();

        // 先给一小部分（小于长度前缀）
        let chunk_size = 2;
        buffer.extend_from_slice(&encoded[..chunk_size]);

        // 第一次解码应该返回 None（数据不足）
        assert!(codec.decode(&mut buffer).unwrap().is_none());

        // 再给一小部分，但仍不足一帧
        buffer.extend_from_slice(&encoded[chunk_size..chunk_size * 2]);
        assert!(codec.decode(&mut buffer).unwrap().is_none());

        // 再给剩余部分
        buffer.extend_from_slice(&encoded[chunk_size * 2..]);

        // 现在应该能解码
        let decoded = codec.decode(&mut buffer).unwrap().expect("decode failed");
        assert_eq!(frame, decoded);
    }

    #[test]
    fn test_multi_frame_decode() {
        let frame1 = Frame::Raw {
            stream_id: 1,
            data: vec![0x01],
        };
        let frame2 = Frame::Raw {
            stream_id: 2,
            data: vec![0x02],
        };

        let mut encoded = BytesMut::new();
        encoded.extend(encode_frame(frame1.clone()));
        encoded.extend(encode_frame(frame2.clone()));

        let mut buffer = encoded;
        let decoded1 = decode_frame(&mut buffer).unwrap().expect("decode frame1 failed");
        let decoded2 = decode_frame(&mut buffer).unwrap().expect("decode frame2 failed");

        assert_eq!(frame1, decoded1);
        assert_eq!(frame2, decoded2);
        assert!(buffer.is_empty());
    }

    #[test]
    fn test_decode_frame_insufficient_vs_violation() {
        use crate::message::{MsgKind, Message};
        // 数据不足 → Ok(None)（不是 Err）
        let mut partial = BytesMut::from(&[0u8, 0, 0, 10][..]); // 声称 10 字节但没给够
        assert_eq!(decode_frame(&mut partial).unwrap(), None);
        // 协议违例（未知判别符 0x99）→ Err
        let mut src = encode_frame(Frame::Json(Message {
            id: 1,
            kind: MsgKind::Req,
            op: "x".into(),
            payload: serde_json::json!({}),
            err: None,
        }));
        assert!(src.len() > 5); // 长度前缀 4 + 判别符 1 + 载荷 ≥ 1
        src[4] = 0x99; // 篡改判别符字节（长度前缀 4 字节之后）
        assert!(decode_frame(&mut src).is_err());
    }
}
