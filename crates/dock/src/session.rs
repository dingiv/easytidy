//! root 会话管理（easytidy-dock daemon 用）。
//!
//! 每个 session = 一个容器内 root bash + PTY + 输出环形缓冲 + 多客户端订阅。
//! daemon 持有 0..N 个 session，每个 session 可被 0..N 个 client attach。
//!
//! 与 server Pty.rs 的 PtySession 同构（共享 ring/fan-out/replay 模式），
//! 但本会话是 easytidy-dock 私有不依赖 server 的最小子集。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use easytidy_protocol::Frame;
use tokio::sync::mpsc;

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};

/// 输出回放缓冲上限（128KB，约覆盖 1000+ 行终端输出；与 server RING_MAX 一致）。
pub const RING_MAX: usize = 128 * 1024;

/// 全局 session id 分配器（daemon 内单实例）。
static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);

/// 分配新 session id（自增 1）。
pub fn next_session_id() -> u64 {
    NEXT_SESSION_ID.fetch_add(1, Ordering::SeqCst)
}

/// 输出环形缓冲。
#[derive(Debug, Clone)]
pub struct ReplayRing {
    data: VecDeque<u8>,
    max: usize,
}

impl ReplayRing {
    pub fn new(max: usize) -> Self {
        Self {
            data: VecDeque::with_capacity(max),
            max,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.data.push_back(*b);
            if self.data.len() > self.max {
                self.data.pop_front();
            }
        }
    }

    pub fn replay_bytes(&self) -> Vec<u8> {
        self.data.iter().copied().collect()
    }

    /// 2026 同步输出包裹：`?2026h` + 清屏 + ring + `?2026l`。
    /// 让 xterm 6.0 原子渲染整帧，避免重连时光标漂移/闪烁。
    pub fn replay_wrapped(&self) -> Vec<u8> {
        const SYNC_START: &[u8] = b"\x1b[?2026h";
        const SYNC_END: &[u8] = b"\x1b[?2026l";
        const PREFIX: &[u8] = b"\x1b[2J\x1b[H";
        let ring = self.replay_bytes();
        let mut out = Vec::with_capacity(
            SYNC_START.len() + PREFIX.len() + ring.len() + SYNC_END.len(),
        );
        out.extend_from_slice(SYNC_START);
        out.extend_from_slice(PREFIX);
        out.extend_from_slice(&ring);
        out.extend_from_slice(SYNC_END);
        out
    }
}

/// 单 root bash session（容器内 root PTY + 输出缓冲 + 多客户端 fan-out）。
pub struct RootSession {
    /// session id（daemon 内唯一）
    pub id: u64,
    /// 容器内 PTY master（resize 用，cloned 自 session 创建时）
    pub master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    /// PTY master 的写侧（client → bash 输入）
    pub writer: Arc<Mutex<Box<dyn std::io::Write + Send>>>,
    /// 容器内 bash 的 spawn PID（仅供日志 / SIGCHLD 关联，daemon 不 wait）
    pub spawn_pid: u32,
    /// session 是否存活（bash exit → false）
    pub alive: AtomicBool,
    /// 输出环形缓冲（attach 时回放）
    pub ring: Mutex<ReplayRing>,
    /// 订阅者：(连接 token, 输出通道)。client 断开按 token 退订（detach）。
    pub subs: Mutex<Vec<(u64, mpsc::UnboundedSender<Frame>)>>,
}

impl RootSession {
    pub fn new(master: Box<dyn MasterPty + Send>, writer: Box<dyn std::io::Write + Send>, spawn_pid: u32) -> Self {
        Self {
            id: next_session_id(),
            master: Arc::new(Mutex::new(master)),
            writer: Arc::new(Mutex::new(writer)),
            spawn_pid,
            alive: AtomicBool::new(true),
            ring: Mutex::new(ReplayRing::new(RING_MAX)),
            subs: Mutex::new(Vec::new()),
        }
    }

    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    pub fn set_dead(&self) {
        self.alive.store(false, Ordering::SeqCst);
    }

    /// 追加 bash 输出 + fan-out 给所有订阅者。
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

    /// 广播 session 死亡事件。
    pub fn broadcast_exited(&self) {
        let frame = Frame::Json(easytidy_protocol::Message {
            id: 0,
            kind: easytidy_protocol::MsgKind::Evt,
            op: "rc.exited".to_string(),
            payload: serde_json::Value::Null,
            err: None,
        });
        let dead = self
            .subs
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, tx)| tx.send(frame.clone()).is_err())
            .map(|(t, _)| *t)
            .collect::<Vec<u64>>();
        if !dead.is_empty() {
            let mut subs = self.subs.lock().unwrap();
            subs.retain(|(t, _)| !dead.contains(t));
        }
    }

    /// 2026 包裹的回放帧（attach 时恢复屏幕）。
    pub fn replay(&self) -> Vec<u8> {
        self.ring.lock().unwrap().replay_wrapped()
    }

    /// 登记订阅者，返回其专属输出通道接收端。
    pub fn subscribe(&self, token: u64) -> mpsc::UnboundedReceiver<Frame> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.subs.lock().unwrap().push((token, tx));
        rx
    }

    /// 按 token 退订（client 断开 = detach，仅退订）。
    pub fn unsubscribe(&self, token: u64) {
        self.subs.lock().unwrap().retain(|(t, _)| *t != token);
    }

    /// client → bash stdin。
    pub fn write_input(&self, data: &[u8]) {
        let mut w = self.writer.lock().unwrap();
        if let Err(e) = w.write_all(data) {
            tracing::warn!("root session {} stdin write failed: {e}", self.id);
        }
        let _ = w.flush();
    }

    /// 主动关闭 session：SIGHUP 到 bash 进程组（bash 是 PTY slave 的
    /// session leader，PGID==PID）+ 标记 dead。bash 退出后 reader task 读到
    /// EOF → 广播 `rc.exited` + 由 SIGCHLD 清理移除。
    pub fn kill(&self) {
        // SIGHUP 到进程组：终端关闭语义（bash 及前台进程组收到挂断 → 退出）
        if self.spawn_pid > 0 {
            if let Err(e) = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(self.spawn_pid as i32),
                nix::sys::signal::Signal::SIGHUP,
            ) {
                tracing::warn!("session {} killpg({}) failed: {e}", self.id, self.spawn_pid);
            } else {
                tracing::info!("session {} sent SIGHUP to pgid {}", self.id, self.spawn_pid);
            }
        }
        self.set_dead();
    }
}

/// 创建容器内 root bash session：开 PTY + fork bash + 返回 session。
///
/// 默认登录 shell（bash 优先，alpine 等无 bash 时回退 sh）。
/// 不设置环境（容器 server 注入的 env 与 root 无关——root 操作更接近
/// 直接登录容器，env 极简）。
pub fn spawn_root_shell(cols: u16, rows: u16) -> anyhow::Result<Arc<RootSession>> {
    use anyhow::Context;

    let shell = if std::path::Path::new("/bin/bash").exists() {
        "/bin/bash"
    } else {
        "/bin/sh"
    };
    tracing::info!("spawn_root_shell: shell={shell} {}x{}", cols, rows);

    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("openpty failed")?;

    let writer = pair
        .master
        .take_writer()
        .context("take PTY writer failed")?;

    let mut cmd = CommandBuilder::new(shell);
    cmd.arg("-l");
    cmd.env("TERM", "xterm-256color");
    // root 身份在容器内默认 home = /root（exec 已确保容器内 uid=0）
    cmd.env("HOME", "/root");

    let slave = pair
        .slave
        .spawn_command(cmd)
        .context("spawn root shell failed")?;
    let spawn_pid = slave.process_id().unwrap_or(0);

    drop(pair.slave);

    tracing::info!("root shell spawned (spawn_pid={spawn_pid})");
    Ok(Arc::new(RootSession::new(pair.master, writer, spawn_pid)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ring_truncates_oldest() {
        let mut r = ReplayRing::new(4);
        r.push(&[1, 2, 3, 4, 5, 6]);
        assert_eq!(r.replay_bytes(), vec![3, 4, 5, 6]);
    }

    #[test]
    fn test_ring_wrapped_byte_order() {
        let mut r = ReplayRing::new(64);
        r.push(b"hello");
        let w = r.replay_wrapped();
        assert!(w.starts_with(b"\x1b[?2026h"));
        assert!(w.ends_with(b"\x1b[?2026l"));
        let inner = &w[b"\x1b[?2026h".len()..w.len() - b"\x1b[?2026l".len()];
        assert_eq!(inner, b"\x1b[2J\x1b[Hhello");
    }

    #[test]
    fn test_session_id_unique() {
        let a = next_session_id();
        let b = next_session_id();
        assert_ne!(a, b);
    }
}
