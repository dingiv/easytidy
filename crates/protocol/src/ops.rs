//! 操作载荷定义（各消息族的请求/响应/事件结构体）。
//!
//! 所有结构体都需要实现 Serialize + Deserialize，以便作为 Message::payload 传输。

use serde::{Deserialize, Serialize};

// ============================================================================
// pty: 终端操作族
// ============================================================================

/// 打开 PTY 会话
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PtyOpen {
    /// 要执行的命令（如 "/bin/bash"）
    pub cmd: String,
    /// 命令参数（如 ["bash", "-l"]）
    pub argv: Vec<String>,
    /// 环境变量（键值对）
    pub env: std::collections::HashMap<String, String>,
    /// 工作目录
    pub cwd: String,
    /// 终端列数
    pub cols: u16,
    /// 终端行数
    pub rows: u16,
    /// 接线"常驻终端"（默认 false）——**与 [`PtyOpen::attach_stream`] 是两码事**：
    /// 本字段是「是否使用 server 持有的每容器唯一常驻交互终端」的开关。
    /// true = 复用已有常驻终端（无则新建并设为常驻，输出回放）；false =
    /// 独立会话（CLI 执行命令传 false）。命名沿用历史；此处 "attach" 指
    /// "接常驻终端"，**不是**"按 stream_id 重连"（后者是 `attach_stream`）。
    #[serde(default)]
    pub attach: bool,
    /// 多终端实例：true = 新建**独立持久会话**（server 持有，不随连接断开
    /// 清理，也不登记为身份默认终端；GUI 多开终端用，配合 pty.list 恢复）
    #[serde(default)]
    pub persistent: bool,
    /// 按 **stream_id** 重连到指定已有会话（输出回放 + 订阅，返回其 stream_id；
    /// GUI 重开窗口恢复多终端面板用；会话不存在时回退新建路径）。
    /// ⚠️ 与 `attach`（常驻终端开关）无关：本字段是"重连某个具体会话"。
    #[serde(default)]
    pub attach_stream: Option<u32>,
}

/// PtyOpen 响应：返回分配的 stream_id
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PtyOpenResp {
    /// 流 ID（后续的 Raw 帧和 PtyResize/PtyClose 都需要）
    pub stream_id: u32,
}

/// 列出当前所有活跃 PTY 会话（GUI get_terminals；多终端面板恢复用）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PtyList;

/// 单个终端会话信息（pty.list 项）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PtyTerminalInfo {
    /// 流 ID（pty.open{attach_stream} 重连）
    pub stream_id: u32,
    /// 显示命令（pty.open 的 cmd；空 = 默认登录 shell）
    pub cmd: String,
    /// 常驻会话（server 持有，不随连接断开清理）
    pub persistent: bool,
    /// 最近一次工作目录（pty.cwd 查询过才有）
    pub cwd: Option<String>,
}

/// PtyList 响应：所有活跃会话
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PtyListResp {
    pub terminals: Vec<PtyTerminalInfo>,
}

/// 查询 PTY 会话主进程的实时工作目录（文件浏览器"跟随终端"用；
/// 经 /proc/<pid>/cwd 读取，反映 cd 后的实际目录）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PtyCwd {
    /// 流 ID
    pub stream_id: u32,
}

/// PtyCwd 响应：当前工作目录
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PtyCwdResp {
    pub cwd: String,
}

/// 关闭 PTY 会话
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PtyClose {
    /// 流 ID
    pub stream_id: u32,
}

/// 调整 PTY 大小
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PtyResize {
    /// 流 ID
    pub stream_id: u32,
    /// 新的列数
    pub cols: u16,
    /// 新的行数
    pub rows: u16,
}

/// PTY 进程退出事件
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PtyExited {
    /// 流 ID
    pub stream_id: u32,
    /// 退出码
    pub code: i32,
}

// ============================================================================
// fs: 文件系统操作族
// ============================================================================

/// 列出目录内容
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FsList {
    /// 目录路径
    pub path: String,
}

/// FsList 响应：目录条目列表
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FsListResp {
    /// 条目列表
    pub entries: Vec<FsEntry>,
}

/// 文件系统条目
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FsEntry {
    /// 文件名
    pub name: String,
    /// 类型（file, dir, symlink）。符号链接按解析后的目标类型分类
    /// （目录链接 → dir、文件链接 → file），坏链接才是 symlink
    pub entry_type: FsEntryType,
    /// 是否为符号链接（与 entry_type 正交：目录/文件链接也为 true，
    /// 前端据此区分图标）
    #[serde(default)]
    pub is_symlink: bool,
    /// 大小（字节，仅文件）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// 权限（如 "0755"）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
}

/// 条目类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FsEntryType {
    /// 文件
    File,
    /// 目录
    Dir,
    /// 符号链接
    Symlink,
}

/// 获取文件/目录元数据
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FsStat {
    /// 路径
    pub path: String,
}

/// FsStat 响应：详细元数据
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FsStatResp {
    /// 条目基本信息
    pub entry: FsEntry,
    /// 最后修改时间（Unix 时间戳）
    pub mtime: i64,
    /// 最后访问时间（Unix 时间戳）
    pub atime: i64,
    /// 创建时间（Unix 时间戳）
    pub ctime: i64,
}

/// 读取文件内容
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FsRead {
    /// 文件路径
    pub path: String,
    /// 起始偏移（可选，默认 0）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<u64>,
    /// 读取长度（可选，默认读全部）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub len: Option<u64>,
}

/// FsRead 响应：文件数据（Base64 编码）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FsReadResp {
    /// 文件数据（Base64 编码）
    pub data_b64: String,
}

/// 写入文件内容
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FsWrite {
    /// 文件路径
    pub path: String,
    /// 写入数据（Base64 编码）
    pub data_b64: String,
    /// 写入偏移（分块上传续写用；None = 全量覆盖创建）
    #[serde(default)]
    pub offset: Option<u64>,
}

/// ServerInfo 响应（`server.info`；请求侧无 payload——server 不解析请求体，
/// 客户端发 `Null` 即可）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerInfoResp {
    /// HTTP 静态文件服务端口（0 = 未启用）
    pub http_port: u16,
    /// 容器默认用户 home（server 即容器默认用户；宿主侧据此定位容器内
    /// 用户可写资源，如 ~/.local/share/icons/easytidy 自定义应用图标目录）。
    /// `#[serde(default)]`：旧 server 无此字段 → 空串，前后端版本兼容。
    #[serde(default)]
    pub home_dir: String,
}

/// 容器内创建目录（文件夹拖入上传时递归建目录;create_dir_all 幂等）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FsMkdir {
    /// 目录路径
    pub path: String,
}

/// FsMkdir 响应
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FsMkdirResp;

/// 容器内复制文件（复制/粘贴菜单;server 直接 fs::copy,大文件无 IPC 负担）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FsCopy {
    /// 源路径
    pub src: String,
    /// 目标路径
    pub dst: String,
}

/// FsCopy 响应：复制结果
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FsCopyResp {
    /// 复制的字节数
    pub bytes_copied: u64,
}

/// FsWrite 响应：写入字节数
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FsWriteResp {
    /// 写入的字节数
    pub bytes_written: u64,
}

// ============================================================================
// apps: 桌面应用枚举族
// ============================================================================

/// 列出桌面应用
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppsList;

/// AppsList 响应：应用列表
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppsListResp {
    /// 应用列表
    pub apps: Vec<AppInfo>,
}

/// 桌面应用信息
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppInfo {
    /// .desktop 文件路径
    pub desktop_file: String,
    /// 应用名称
    pub name: String,
    /// 图标路径（可选）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon_path: Option<String>,
    /// 执行命令
    pub exec: String,
    /// 描述（可选）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// Categories（.desktop，逗号分隔；passthrough 导出时原样带出）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub categories: Option<String>,
    /// StartupNotify（.desktop，默认 false）
    #[serde(default)]
    pub startup_notify: bool,
    /// StartupWMClass（.desktop；Wayland 窗口匹配用）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub startup_wm_class: Option<String>,
}

/// 拉起一组应用（passthrough auto-start：容器启动时 server 直接 spawn，
/// 不绑定客户端连接——宿主一次性 CLI 无法保活 PTY 会话，实测）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppsLaunch {
    /// 要拉起的应用列表（逐条独立成败）
    pub apps: Vec<AppsLaunchItem>,
}

/// 单条拉起请求
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppsLaunchItem {
    /// 应用名（仅日志/跟踪用）
    pub name: String,
    /// 实际执行命令串（如 "google-chrome-stable --disable-dev-shm-usage"）
    pub cmd: String,
}

/// AppsLaunch 响应：逐条结果
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppsLaunchResp {
    pub results: Vec<AppsLaunchResult>,
}

/// 单条拉起结果（成功给 pid，失败给 error——单条失败不阻断其余）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppsLaunchResult {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 获取应用图标数据
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppGetIcon {
    /// 图标路径
    pub path: String,
}

/// AppGetIcon 响应：图标数据
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppGetIconResp {
    /// 图标格式（如 "png", "svg"）
    pub format: String,
    /// 图标数据（Base64 编码）
    pub data_b64: String,
}

// ============================================================================
// apps: 托管进程管理族（server 全权管理拉起的应用：stdio + 生命周期）
// ============================================================================

/// 托管进程摘要（server 拉起的 entry / passthrough 应用）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManagedProcess {
    /// 进程 PID
    pub pid: u32,
    /// 展示名（entry id 或应用名）
    pub name: String,
    /// 进程类型（"entry" / "passthrough"）
    pub kind: String,
    /// 实际执行命令串
    pub cmd: String,
    /// 启动时刻（unix millis）
    pub started_at: u64,
    /// 状态："running" / "exited"
    pub status: String,
    /// 退出码（running 时 None）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// 已捕获 stdio 字节数（u64 而非 usize——wire 类型须平台无关宽度）
    pub stdio_len: u64,
}

/// 列出托管进程（apps.ps）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppsPs;

/// apps.ps 响应
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppsPsResp {
    pub processes: Vec<ManagedProcess>,
}

/// 获取某进程捕获的 stdio（apps.logs）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppLogs {
    pub pid: u32,
}

/// apps.logs 响应
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppLogsResp {
    pub pid: u32,
    /// 捕获的 stdout+stderr（有损 UTF-8；超出上限丢弃最旧）
    pub stdio: String,
}

/// 终止托管进程（apps.kill；SIGTERM）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppKill {
    pub pid: u32,
}

// ============================================================================
// passthrough: Passthrough 管理族
// ============================================================================

/// 导出桌面应用到宿主
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PtExport {
    /// .desktop 文件路径
    pub desktop_file: String,
    /// 是否添加到菜单
    pub menu: bool,
    /// 是否开机自启
    pub silent_boot: bool,
}

/// PtExport 响应：导出结果
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PtExportResp {
    /// 宿主侧生成的 .desktop 文件路径
    pub generated_path: String,
}

/// 列出所有 passthrough 应用
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PtState;

/// PtState 响应：passthrough 状态列表
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PtStateResp {
    /// 导出的应用列表
    pub entries: Vec<PtEntry>,
}

/// Passthrough 条目
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PtEntry {
    /// 容器内 .desktop 路径
    pub desktop_file: String,
    /// 是否在菜单中
    pub menu: bool,
    /// 是否开机自启
    pub silent_boot: bool,
    /// 宿主侧生成路径
    pub generated_path: String,
}

/// 撤销 passthrough 导出
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PtRevoke {
    /// .desktop 文件路径
    pub desktop_file: String,
}

/// PtRevoke 响应：撤销成功
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PtRevokeResp {
    /// 已删除的宿主侧路径
    pub removed_path: String,
}

/// 配置中的应用条目（容器内 passthrough 配置：应用列表 + auto_start）。
///
/// 存在**容器内** `{home}/.easytidy/passthrough.toml`（随容器层/快照
/// 持久，容器自包含）；server 启动自读并拉起 auto_start 应用。宿主只留收藏(pinned)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PtConfiguredApp {
    pub id: String,
    pub name: String,
    pub cmd: String,
    /// 容器内 .desktop 路径（custom 应用为 None）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop_file: Option<String>,
    #[serde(default)]
    pub auto_start: bool,
    /// 图标：扫描应用 = 容器内路径；custom = 宿主 ~/.easytidy/icons 路径（仅宿主
    /// 展示/导出用，server 忽略）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

/// 读容器内 passthrough 配置（passthrough.list；不存在返回空列表）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PassthroughList;

/// PassthroughList 响应
///
/// `pinned`（收藏/pin 到工具栏）与 `apps`（auto-start/自定义应用）同存容器内配置，
/// 容器自包含——容器删除/同名重建即随之清空，不再泄漏到宿主。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PassthroughListResp {
    pub apps: Vec<PtConfiguredApp>,
    #[serde(default)]
    pub pinned: Vec<PtConfiguredApp>,
}

/// 写容器内 passthrough 配置（passthrough.set；整份覆盖 + 建目录）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PassthroughSet {
    pub apps: Vec<PtConfiguredApp>,
    #[serde(default)]
    pub pinned: Vec<PtConfiguredApp>,
}

// ============================================================================
// config: 配置管理族
// ============================================================================

/// 获取完整配置
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CfgGet;

/// CfgGet 响应：完整配置 JSON
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CfgGetResp {
    /// 配置内容
    pub config: serde_json::Value,
}

/// 设置配置项
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CfgSet {
    /// 配置键（如 "mounts.0.path"）
    pub key: String,
    /// 配置值
    pub value: serde_json::Value,
}

/// CfgSet 响应：设置成功
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CfgSetResp {
    /// 是否成功
    pub success: bool,
}

// ============================================================================
// lifecycle: 生命周期管理族
// ============================================================================

/// 关闭容器
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LifecycleShutdown;

/// 启动 entry 应用
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LifecycleEntryLaunch {
    /// entry ID（如 "desktop", "code"）
    pub entry_id: String,
}

/// 子进程退出事件
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChildExited {
    /// 子进程 PID
    pub pid: u32,
    /// 退出码
    pub code: i32,
    /// 进程类型（entry, passthrough）
    pub kind: String,
}

/// 关闭确认事件
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShutdownAck;

// ============================================================================
// server: 服务器自信息族
// ============================================================================

/// 查询 server 运行时注入的环境变量（server.env）。
///
/// server 启动时探测/修正的 session 耦合值——**配置里定义不了、宿主侧也无法
/// 预知最终值**（路径含随机后缀 / 取决于容器运行时环境）：如 `XAUTHORITY`
/// （自动探测 $XDG_RUNTIME_DIR 下 X11 auth 文件）、`XDG_DATA_DIRS`（追加系统
/// 默认数据目录）。宿主配置管理器据此展示「easytidy 注入」只读 env 行。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerEnv;

/// 单个 server 运行时注入项
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerEnvItem {
    /// 环境变量名
    pub key: String,
    /// 注入的值（当前生效）
    pub value: String,
    /// 注入原因（展示用）
    pub note: String,
}

/// ServerEnv 响应
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerEnvResp {
    pub env: Vec<ServerEnvItem>,
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pty_open_serde() {
        let op = PtyOpen {
            cmd: "/bin/bash".to_string(),
            argv: vec!["bash".to_string(), "-l".to_string()],
            env: [("TERM".to_string(), "xterm-256color".to_string())].into(),
            cwd: "/home/user".to_string(),
            cols: 80,
            rows: 24,
            attach: false,
            persistent: false,
            attach_stream: None,
        };

        let json = serde_json::to_string(&op).expect("serialize failed");
        let decoded: PtyOpen = serde_json::from_str(&json).expect("deserialize failed");

        assert_eq!(op, decoded);
    }

    #[test]
    fn test_fs_entry_serde() {
        let entry = FsEntry {
            name: "test.txt".to_string(),
            entry_type: FsEntryType::File,
            is_symlink: false,
            size: Some(1024),
            mode: Some("0644".to_string()),
        };

        let json = serde_json::to_string(&entry).expect("serialize failed");
        let decoded: FsEntry = serde_json::from_str(&json).expect("deserialize failed");

        assert_eq!(entry, decoded);
    }

    #[test]
    fn test_app_info_serde() {
        let app = AppInfo {
            desktop_file: "/usr/share/applications/firefox.desktop".to_string(),
            name: "Firefox".to_string(),
            icon_path: Some("/usr/share/icons/hicolor/48x48/apps/firefox.png".to_string()),
            exec: "firefox %u".to_string(),
            comment: Some("Web Browser".to_string()),
            categories: Some("Network;WebBrowser;".to_string()),
            startup_notify: true,
            startup_wm_class: Some("firefox".to_string()),
        };

        let json = serde_json::to_string(&app).expect("serialize failed");
        let decoded: AppInfo = serde_json::from_str(&json).expect("deserialize failed");

        assert_eq!(app, decoded);
    }

    #[test]
    fn test_apps_launch_serde() {
        let req = AppsLaunch {
            apps: vec![
                AppsLaunchItem {
                    name: "Chrome".to_string(),
                    cmd: "google-chrome-stable --disable-dev-shm-usage".to_string(),
                },
                AppsLaunchItem {
                    name: "Broken".to_string(),
                    cmd: String::new(),
                },
            ],
        };
        let json = serde_json::to_string(&req).expect("serialize failed");
        let decoded: AppsLaunch = serde_json::from_str(&json).expect("deserialize failed");
        assert_eq!(req, decoded);

        let resp = AppsLaunchResp {
            results: vec![
                AppsLaunchResult {
                    name: "Chrome".to_string(),
                    pid: Some(1234),
                    error: None,
                },
                AppsLaunchResult {
                    name: "Broken".to_string(),
                    pid: None,
                    error: Some("empty cmd".to_string()),
                },
            ],
        };
        let json = serde_json::to_string(&resp).expect("serialize failed");
        let decoded: AppsLaunchResp = serde_json::from_str(&json).expect("deserialize failed");
        assert_eq!(resp, decoded);
    }

    #[test]
    fn test_pt_export_serde() {
        let op = PtExport {
            desktop_file: "/usr/share/applications/code.desktop".to_string(),
            menu: true,
            silent_boot: false,
        };

        let json = serde_json::to_string(&op).expect("serialize failed");
        let decoded: PtExport = serde_json::from_str(&json).expect("deserialize failed");

        assert_eq!(op, decoded);
    }

    #[test]
    fn test_pt_configured_app_serde() {
        let app = PtConfiguredApp {
            id: "desktop:/usr/share/applications/google-chrome.desktop".to_string(),
            name: "Google Chrome".to_string(),
            cmd: "google-chrome-stable".to_string(),
            desktop_file: Some("/usr/share/applications/google-chrome.desktop".to_string()),
            auto_start: true,
            icon: Some("/usr/share/icons/64x64/apps/google-chrome.png".to_string()),
        };
        let json = serde_json::to_string(&app).unwrap();
        let decoded: PtConfiguredApp = serde_json::from_str(&json).unwrap();
        assert_eq!(app, decoded);

        // 旧/精简格式缺省字段兼容
        let minimal: PtConfiguredApp =
            serde_json::from_str(r#"{"id":"custom:x","name":"X","cmd":"x"}"#).unwrap();
        assert_eq!(minimal.desktop_file, None);
        assert!(!minimal.auto_start);
        assert_eq!(minimal.icon, None);
    }

    #[test]
    fn test_cfg_set_serde() {
        let op = CfgSet {
            key: "mounts.0.path".to_string(),
            value: serde_json::json!("/home/user/data"),
        };

        let json = serde_json::to_string(&op).expect("serialize failed");
        let decoded: CfgSet = serde_json::from_str(&json).expect("deserialize failed");

        assert_eq!(op, decoded);
    }

    #[test]
    fn test_fs_write_offset_serde() {
        // 旧格式（无 offset）→ None（全量覆盖）
        let old: FsWrite =
            serde_json::from_str(r#"{"path":"/tmp/a","data_b64":"aGk="}"#).unwrap();
        assert_eq!(old.offset, None);
        // 新格式带 offset（分块续写）
        let op = FsWrite {
            path: "/tmp/a".to_string(),
            data_b64: "aGk=".to_string(),
            offset: Some(4096),
        };
        let json = serde_json::to_string(&op).unwrap();
        let decoded: FsWrite = serde_json::from_str(&json).unwrap();
        assert_eq!(op, decoded);
        assert_eq!(decoded.offset, Some(4096));
    }

    #[test]
    fn test_server_env_serde() {
        let resp = ServerEnvResp {
            env: vec![
                ServerEnvItem {
                    key: "XAUTHORITY".to_string(),
                    value: "/run/user/1000/.mutter-Xwaylandauth.DE23U3".to_string(),
                    note: "自动探测 X11 auth 文件".to_string(),
                },
                ServerEnvItem {
                    key: "XDG_DATA_DIRS".to_string(),
                    value: "/usr/local/share:/usr/share".to_string(),
                    note: "追加系统默认数据目录".to_string(),
                },
            ],
        };
        let json = serde_json::to_string(&resp).unwrap();
        let decoded: ServerEnvResp = serde_json::from_str(&json).unwrap();
        assert_eq!(resp, decoded);
    }

    #[test]
    fn test_fs_copy_serde() {
        let op = FsCopy {
            src: "/tmp/a".to_string(),
            dst: "/tmp/b".to_string(),
        };
        let json = serde_json::to_string(&op).unwrap();
        let decoded: FsCopy = serde_json::from_str(&json).unwrap();
        assert_eq!(op, decoded);
    }

    #[test]
    fn test_fs_entry_type() {
        use serde_json::json;
        assert_eq!(serde_json::to_value(FsEntryType::File).unwrap(), json!("file"));
        assert_eq!(serde_json::to_value(FsEntryType::Dir).unwrap(), json!("dir"));
        assert_eq!(serde_json::to_value(FsEntryType::Symlink).unwrap(), json!("symlink"));

        assert_eq!(
            serde_json::from_str::<FsEntryType>(r#""file""#).unwrap(),
            FsEntryType::File
        );
        assert_eq!(
            serde_json::from_str::<FsEntryType>(r#""dir""#).unwrap(),
            FsEntryType::Dir
        );
        assert_eq!(
            serde_json::from_str::<FsEntryType>(r#""symlink""#).unwrap(),
            FsEntryType::Symlink
        );
    }
}
