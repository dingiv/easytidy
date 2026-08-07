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
    /// 以 root 运行（默认 false：用户映射生效时经 su 以容器用户运行；
    /// setup/包管理场景传 true 跳过 su）
    #[serde(default)]
    pub as_root: bool,
}

/// PtyOpen 响应：返回分配的 stream_id
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PtyOpenResp {
    /// 流 ID（后续的 Raw 帧和 PtyResize/PtyClose 都需要）
    pub stream_id: u32,
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
    /// 类型（file, dir, symlink）
    pub entry_type: FsEntryType,
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

/// Entry 启动事件
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntryStarted {
    /// 进程 PID
    pub pid: u32,
}

/// 关闭确认事件
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShutdownAck;

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
            as_root: false,
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
