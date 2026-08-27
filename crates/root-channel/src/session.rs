//! 共享 root 会话：exec 持有 + 输出环形缓冲 + 多客户端订阅 fan-out。
//!
//! 与 server 的 PTY 机制是两条独立进程边界（server 在容器内，root-channel
//! 在宿主），不复用 server 的 `PtySession`——本文件按 server 的
//! `RING_MAX` / `Subscribers` / 2026 包裹模式独立实现（~60 行重复可接受，
//! 跨 crate 共享会引入不必要的依赖方向）。

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use easytidy_protocol::Frame;
use tokio::sync::{mpsc, Mutex as TokioMutex};

/// 输出回放缓冲上限（128KB，约覆盖 1000+ 行终端输出；同 server RING_MAX）。
pub const RING_MAX: usize = 128 * 1024;

/// exec stdin 写侧类型（close 时换 sink → 原 writer drop → shell stdin EOF）。
type StdinWriter = Arc<TokioMutex<Pin<Box<dyn tokio::io::AsyncWrite + Send>>>>;

/// 输出环形缓冲（超限丢最旧；2026 同步输出包裹供单测）。
#[derive(Debug, Clone)]
pub struct ReplayRing {
    data: VecDeque<u8>,
    max: usize,
}

impl ReplayRing {
    pub fn new(max: usize) -> Self {
        Self { data: VecDeque::with_capacity(max), max }
    }

    /// 追加输出，超限从最旧端裁剪。
    pub fn push(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.data.push_back(*b);
            if self.data.len() > self.max {
                self.data.pop_front();
            }
        }
    }

    /// 全量回放字节（未包裹）。
    pub fn replay_bytes(&self) -> Vec<u8> {
        self.data.iter().copied().collect()
    }

    /// 2026 同步输出包裹的回放帧：`?2026h` + 清屏 + ring + `?2026l`。
    ///
    /// 与 server `attach_to_session` 同构：ring 是字节流中段截取，直接回放
    /// 会让 xterm 从错误状态渲染 → 光标漂移。DECSET 2026 包裹使 xterm 6.0
    /// 原子渲染整帧，消除逐块重绘闪烁（调研 2026-08-07 实测结论）。
    pub fn replay_wrapped(&self) -> Vec<u8> {
        const SYNC_START: &[u8] = b"\x1b[?2026h";
        const SYNC_END: &[u8] = b"\x1b[?2026l";
        const PREFIX: &[u8] = b"\x1b[2J\x1b[H";
        let ring = self.replay_bytes();
        let mut out = Vec::with_capacity(SYNC_START.len() + PREFIX.len() + ring.len() + SYNC_END.len());
        out.extend_from_slice(SYNC_START);
        out.extend_from_slice(PREFIX);
        out.extend_from_slice(&ring);
        out.extend_from_slice(SYNC_END);
        out
    }
}

/// 共享 root 会话（单 root shell + 输出缓冲 + 订阅者）。
pub struct RootSession {
    /// exec stdin 写侧（连接任务写入；close 时换 sink）。
    writer: StdinWriter,
    exec_id: String,
    alive: AtomicBool,
    ring: Mutex<ReplayRing>,
    /// 订阅者：(连接 token, 输出通道)。连接断开按 token 退订。
    subs: Mutex<Vec<(u64, mpsc::UnboundedSender<Frame>)>>,
}

impl RootSession {
    pub fn new(writer: StdinWriter, exec_id: String) -> Self {
        Self {
            writer,
            exec_id,
            alive: AtomicBool::new(true),
            ring: Mutex::new(ReplayRing::new(RING_MAX)),
            subs: Mutex::new(Vec::new()),
        }
    }

    /// exec id（resize 经 podman resize_exec 用）。
    pub fn exec_id(&self) -> &str {
        &self.exec_id
    }

    /// root shell 是否存活。
    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// 标记死亡（exec 流 EOF / close）。
    pub fn set_dead(&self) {
        self.alive.store(false, Ordering::SeqCst);
    }

    /// 追加输出并 fan-out 给全部订阅者。
    pub fn push_output(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.ring.lock().unwrap().push(bytes);
        let frame = Frame::Raw {
            stream_id: easytidy_protocol::rc::ROOT_STREAM_ID,
            data: bytes.to_vec(),
        };
        let mut dead = Vec::new();
        for (token, tx) in self.subs.lock().unwrap().iter() {
            if tx.send(frame.clone()).is_err() {
                dead.push(*token);
            }
        }
        if !dead.is_empty() {
            let mut subs = self.subs.lock().unwrap();
            subs.retain(|(t, _)| !dead.contains(t));
        }
    }

    /// 会话死亡事件（广播给订阅者；客户端据此收尾输入区）。
    pub fn broadcast_exited(&self) {
        let frame = Frame::Json(easytidy_protocol::Message {
            id: 0,
            kind: easytidy_protocol::MsgKind::Evt,
            op: "rc.exited".to_string(),
            payload: serde_json::Value::Null,
            err: None,
        });
        let dead = self.subs.lock().unwrap().iter().filter(|(_, tx)| tx.send(frame.clone()).is_err()).map(|(t, _)| *t).collect::<Vec<u64>>();
        if !dead.is_empty() {
            let mut subs = self.subs.lock().unwrap();
            subs.retain(|(t, _)| !dead.contains(t));
        }
    }

    /// 2026 包裹的全量回放帧（新 attach 客户端恢复屏幕）。
    pub fn replay(&self) -> Vec<u8> {
        self.ring.lock().unwrap().replay_wrapped()
    }

    /// 登记订阅者，返回其专属输出通道接收端。
    pub fn subscribe(&self, token: u64) -> mpsc::UnboundedReceiver<Frame> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.subs.lock().unwrap().push((token, tx));
        rx
    }

    /// 按 token 退订（连接断开 = detach，仅退订）。
    pub fn unsubscribe(&self, token: u64) {
        self.subs.lock().unwrap().retain(|(t, _)| *t != token);
    }

    /// 写入 exec stdin（客户端输入）。
    pub async fn write_input(&self, data: &[u8]) {
        use tokio::io::AsyncWriteExt;
        let mut w = self.writer.lock().await;
        if let Err(e) = w.write_all(data).await {
            tracing::warn!("root 会话 stdin 写入失败：{e}");
        }
    }

    /// 杀死 shell：写侧换 sink → 原 writer drop → stdin EOF → shell 退出。
    pub fn kill(&self) {
        if let Ok(mut w) = self.writer.try_lock() {
            *w = Box::pin(tokio::io::sink());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ring_truncates_oldest() {
        let mut r = ReplayRing::new(4);
        r.push(&[1, 2, 3, 4, 5, 6]);
        // 超限（6 > 4）→ 保留最旧裁剪后的 [3,4,5,6]
        assert_eq!(r.replay_bytes(), vec![3, 4, 5, 6]);
    }

    #[test]
    fn test_ring_empty() {
        let r = ReplayRing::new(8);
        assert!(r.replay_bytes().is_empty());
    }

    #[test]
    fn test_ring_wrapped_byte_order() {
        let mut r = ReplayRing::new(64);
        r.push(b"hello");
        let w = r.replay_wrapped();
        assert!(w.starts_with(b"\x1b[?2026h"));
        assert!(w.ends_with(b"\x1b[?2026l"));
        // 中间 = 清屏 + ring
        let inner = &w[b"\x1b[?2026h".len()..w.len() - b"\x1b[?2026l".len()];
        assert_eq!(inner, b"\x1b[2J\x1b[Hhello");
    }
}
