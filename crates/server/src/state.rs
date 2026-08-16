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
    /// 以 root 运行（身份标签；pty.list 展示用）
    pub(crate) as_root: bool,
}

pub(crate) type Subscribers = std::sync::Mutex<Vec<(u64, mpsc::UnboundedSender<easytidy_protocol::Frame>)>>;

/// 常驻终端输出回放缓冲上限（128KB，约覆盖 1000+ 行终端输出）
pub(crate) const RING_MAX: usize = 128 * 1024;

/// Child process info
#[derive(Debug, Clone)]
pub(crate) struct ChildInfo {
    #[allow(dead_code)]
    pub(crate) pid: u32,
    pub(crate) kind: String,
    #[allow(dead_code)]
    pub(crate) entry_id: Option<String>,
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
