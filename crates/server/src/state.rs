//! 服务端共享状态定义。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64};
use std::sync::Arc;

use portable_pty::MasterPty;
use tokio::sync::{mpsc, RwLock};

/// Server state（连接/服务共享，经 Arc 传递）。
pub(crate) struct ServerState {
    /// PTY sessions: stream_id -> session
    pub(crate) sessions: Arc<RwLock<HashMap<u32, Arc<PtySession>>>>,

    /// 常驻终端（每容器按身份各一个，attach 语义）：key → stream_id
    /// （key: "user"=node 常规终端 / "root"=root 终端）
    /// （std RwLock：reader 线程（非 async）结束时需清除，不能用 tokio RwLock；
    /// 包 Arc 供 Clone（reader 线程与连接任务共享））
    pub(crate) default_terminal: Arc<std::sync::RwLock<HashMap<String, u32>>>,

    /// Child processes: pid -> ChildInfo
    pub(crate) children: Arc<RwLock<HashMap<u32, ChildInfo>>>,

    /// Next stream ID
    pub(crate) next_stream_id: Arc<AtomicU32>,

    /// Next connection token（PTY 订阅退订标识）
    pub(crate) next_conn_id: Arc<AtomicU64>,

    /// Next message ID
    pub(crate) next_msg_id: Arc<AtomicU32>,

    /// Shutdown flag
    pub(crate) shutting_down: Arc<AtomicBool>,
}

/// PTY session
pub(crate) struct PtySession {
    pub(crate) writer: Arc<std::sync::Mutex<Box<dyn std::io::Write + Send>>>,
    pub(crate) master: Arc<std::sync::Mutex<Box<dyn MasterPty + Send>>>,
    /// 输出环形缓冲（新 attach 客户端回放当前屏幕；上限见 RING_MAX）
    pub(crate) ring: Arc<std::sync::Mutex<std::collections::VecDeque<u8>>>,
    /// 订阅连接的输出通道（reader 广播；连接断开时按连接 token 退订——
    /// UnboundedSender 无 PartialEq，用连接级唯一 token 标识）
    pub(crate) subs: Arc<Subscribers>,
    /// 常驻标志：不随连接断开清理（attach 终端）；连接断开仅退订
    pub(crate) persistent: std::sync::atomic::AtomicBool,
    /// spawn 的进程 pid（pty.cwd 经 /proc/<pid>/cwd 查询实时工作目录）
    pub(crate) spawn_pid: u32,
    /// 最近一次 cwd（事件驱动检测：输入回车时比较，变化才推送 cwdChanged）
    pub(crate) last_cwd: std::sync::Mutex<Option<String>>,
    /// 显示命令（pty.open 的 cmd；空 = 默认登录 shell；pty.list 展示用）
    pub(crate) cmd: String,
}

impl PtySession {
    /// 显式终结：把 PTY 写侧换成 sink 并 drop 原 writer → 写侧关闭 → bash stdin
    /// 得 EOF → 退出 → reader 线程收到 EOF 回收子进程。
    ///
    /// 为什么必须主动关：session 被 reader 线程以 `Arc<PtySession>` 持有，
    /// `pty.close` 仅从 sessions map 移除**不会**释放 writer（Arc 计数未归零），
    /// bash 收不到 EOF 永不退出（实测 pty.close 后进程残留、reader 永不到 EOF）。
    pub(crate) fn shutdown_writer(&self) {
        if let Ok(mut w) = self.writer.lock() {
            *w = Box::new(std::io::sink());
        }
    }
}

pub(crate) type Subscribers = std::sync::Mutex<Vec<(u64, mpsc::UnboundedSender<easytidy_protocol::Frame>)>>;

/// 常驻终端输出回放缓冲上限（128KB，约覆盖 1000+ 行终端输出）
pub(crate) const RING_MAX: usize = 128 * 1024;

/// 托管进程 stdio 捕获上限（64KB；超出丢最旧——调试/排障足够，防内存膨胀）
pub(crate) const APP_LOG_MAX: usize = 64 * 1024;

/// 托管进程状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProcessStatus {
    /// 仍在运行
    Running,
    /// 已退出（退出码 + 退出时刻 unix millis）
    Exited { code: i32, at: u64 },
}

impl ProcessStatus {
    /// 退出码（Running = None）
    pub(crate) fn exit_code(&self) -> Option<i32> {
        match self {
            ProcessStatus::Running => None,
            ProcessStatus::Exited { code, .. } => Some(*code),
        }
    }
}

/// 托管进程信息（server 拉起并全权管理的应用：entry / passthrough）。
///
/// server 负责其生命周期：spawn 时捕获 stdio（stdout+stderr 合并进有界
/// 环形缓冲）、后台 wait 记录退出状态；GUI 经 `apps.ps` / `apps.logs`
/// / `apps.kill` 查询与管控。
#[derive(Debug, Clone)]
pub(crate) struct ChildInfo {
    /// 进程类型（"entry" / "passthrough"）
    pub(crate) kind: String,
    /// 展示名（entry id 或应用名）
    pub(crate) name: String,
    /// 实际执行命令串
    pub(crate) cmd: String,
    /// 启动时刻（unix millis）
    pub(crate) started_at: u64,
    /// 运行状态（退出码经 wait 记录）
    pub(crate) status: ProcessStatus,
    /// 捕获的 stdio（stdout+stderr 合并，有界环形缓冲）
    pub(crate) stdio: Arc<std::sync::Mutex<std::collections::VecDeque<u8>>>,
}

impl Clone for ServerState {
    fn clone(&self) -> Self {
        ServerState {
            sessions: self.sessions.clone(),
            default_terminal: self.default_terminal.clone(),
            children: self.children.clone(),
            next_stream_id: self.next_stream_id.clone(),
            next_conn_id: self.next_conn_id.clone(),
            next_msg_id: self.next_msg_id.clone(),
            shutting_down: self.shutting_down.clone(),
        }
    }
}
