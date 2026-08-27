//! 用户身份 setup（身份自发现）+ XDG 修正 + XAUTHORITY 探测。
//!
//! 新模型（2026-08-27）：server 即容器默认用户——容器 `User` 字段直接
//! 指向配置 uid:gid（宿主侧 create_with_config），server 无需建号/降权。
//! 身份来源（零 getent 依赖，server 与自身 uid 同权限）：
//! 1. `/proc/self/status`（Uid/Gid 行）
//! 2. `/etc/passwd`（uid 反查名字与 home；无条目 = 镜像未预置且宿主
//!    未配置用户名，正常形态——whoami 显示 uid 数字）
//! 3. 宿主侧注入的提示 env：`EASYTIDY_USER_NAME`（配置用户名，与
//!    `prepare_container` 的 useradd 命名一致）、`EASYTIDY_HOME`
//!    （未配用户名时 server 的 HOME 回退值——keep-id 容器期望应用
//!    数据落宿主 home）
//!
//! 旧 root 容器（User=0:0）不再支持完整功能：检测到 euid==0 仅告警，
//! 按 uid 0 身份继续（存量测试容器能跑即可，装包走宿主 root 通道）。
//! 容器内建号/sudoers/fontconfig 等 root 操作已全部迁到宿主侧
//! （`core::podman::user::prepare_container`）。

use std::fs;
use std::sync::OnceLock;

use tracing::{info, warn};

#[derive(Clone)]
pub(crate) struct UserMap {
    pub(crate) name: String,
    pub(crate) uid: u32,
    pub(crate) gid: u32,
    pub(crate) home: String,
}

/// 自身 uid/gid（/proc/self/status 解析，零依赖）。
pub(crate) fn self_uid_gid() -> (u32, u32) {
    let status = fs::read_to_string("/proc/self/status").unwrap_or_default();
    let mut uid = 0u32;
    let mut gid = 0u32;
    for line in status.lines() {
        if let Some(v) = line.strip_prefix("Uid:") {
            uid = v.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        } else if let Some(v) = line.strip_prefix("Gid:") {
            gid = v.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        }
    }
    (uid, gid)
}

/// 身份全局态：`setup_user_identity` 后写入。
/// `user_map()` 理论上恒有值（main 在 listen 前初始化）；保留 Option
/// 仅为防御（初始化前的极早期调用）。
pub(crate) static USER_MAP: OnceLock<UserMap> = OnceLock::new();

/// 当前生效的身份（容器默认用户 = server 自身）。
pub(crate) fn user_map() -> Option<&'static UserMap> {
    USER_MAP.get()
}

/// /etc/passwd 条目（name/home 提取；`name:x:uid:gid:gecos:home:shell`）。
struct PasswdEntry {
    name: String,
    home: String,
}

/// 纯解析：passwd 文本中 uid 对应的条目。
fn passwd_entry_for_uid(passwd: &str, uid: u32) -> Option<PasswdEntry> {
    for line in passwd.lines() {
        let f: Vec<&str> = line.split(':').collect();
        if f.len() >= 6 && f[2].parse::<u32>().ok() == Some(uid) {
            return Some(PasswdEntry {
                name: f[0].to_string(),
                home: f[5].to_string(),
            });
        }
    }
    None
}

/// 由输入构造身份（纯函数，单测点）。
///
/// 名字优先级：`EASYTIDY_USER_NAME` → passwd 反查 → `uid<uid>`（无 passwd
/// 条目的正常形态）。
///
/// HOME 优先级：配置用户名 → `/home/<name>`（prepare_container 的
/// useradd -m 建目录，不依赖 useradd 执行时序）；未配用户名 →
/// `EASYTIDY_HOME`（keep-id 容器 = 宿主 home）→ passwd 条目 home → `/`。
/// 绝不造 `/home/<uid>` 这类无意义路径。
pub(crate) fn identity_from_inputs(
    passwd: &str,
    uid: u32,
    gid: u32,
    env_name: Option<&str>,
    env_home: Option<&str>,
) -> UserMap {
    let env_name = env_name.filter(|n| !n.is_empty());
    let env_home = env_home.filter(|h| !h.is_empty());
    let entry = passwd_entry_for_uid(passwd, uid);

    let name = match env_name {
        Some(n) => n.to_string(),
        None => entry.as_ref().map(|e| e.name.clone()).unwrap_or_else(|| format!("uid{uid}")),
    };
    let home = match env_name {
        Some(n) => format!("/home/{n}"),
        None => match env_home {
            Some(h) => h.to_string(),
            None => entry.as_ref().map(|e| e.home.clone()).unwrap_or_else(|| "/".to_string()),
        },
    };
    UserMap { name, uid, gid, home }
}

/// 启动期身份初始化（main 初始化后、listen 前调用；同步，无 IO 阻塞点）。
pub(crate) fn setup_user_identity() -> UserMap {
    let (uid, gid) = self_uid_gid();
    if uid == 0 {
        warn!(
            "server 以 root（uid 0）运行——旧形态容器，新模型不再支持 \
             （装包/root 操作走宿主 root 通道）；按 uid 0 身份继续"
        );
    }
    let passwd = fs::read_to_string("/etc/passwd").unwrap_or_default();
    let env_name = std::env::var("EASYTIDY_USER_NAME").ok();
    let env_home = std::env::var("EASYTIDY_HOME").ok();
    let identity =
        identity_from_inputs(&passwd, uid, gid, env_name.as_deref(), env_home.as_deref());
    let _ = USER_MAP.set(identity.clone());
    info!("身份自发现：{}({}:{}) home={}", identity.name, identity.uid, identity.gid, identity.home);
    identity
}

/// 修正 XDG_DATA_DIRS 值，确保包含系统默认数据目录。
///
/// 背景：旧版 flavor 注入 `XDG_DATA_DIRS=/usr/share/easytidy-host`（纯覆盖），
/// gdk-pixbuf 2.42 经 `$XDG_DATA_DIRS/gdk-pixbuf-2.0/2.10.0/loaders.cache`
/// 查找 loader 注册表，覆盖后系统 cache 不可达 → 容器内 PNG 图标解码失败
/// （"Unrecognized image file format"）→ GTK 文件选择器断言崩溃（实测 Chrome
/// 保存图片）。mime 数据库（$XDG_DATA_DIRS/mime）同理受影响。追加 glib 默认
/// 的 /usr/local/share:/usr/share（容器内缺失路径无害）。
pub(crate) fn fixup_xdg_data_dirs_value(v: &str) -> String {
    let mut merged = v.to_string();
    for p in ["/usr/local/share", "/usr/share"] {
        if !merged.split(':').any(|c| c == p) {
            merged.push(':');
            merged.push_str(p);
        }
    }
    merged
}

/// 修正 server 进程自身的 XDG_DATA_DIRS（子进程继承）。
pub(crate) fn fixup_xdg_data_dirs() {
    let Ok(v) = std::env::var("XDG_DATA_DIRS") else {
        return;
    };
    let merged = fixup_xdg_data_dirs_value(&v);
    if merged != v {
        std::env::set_var("XDG_DATA_DIRS", &merged);
        info!("XDG_DATA_DIRS 已修正（追加系统默认）: {merged}");
    }
}

/// 探测 X11 auth 文件并强制覆盖进程 `XAUTHORITY`(server 内置 GUI 透传)。
///
/// **为什么不让用户配 XAUTHORITY**:
/// - 路径含随机后缀(典型 `/run/user/$uid/mutter-Xwaylandauth.<random>` 或
///   `xauth_<random>`,由 compositor 在登录会话时随机生成,会变)。
/// - 用户写在 YAML/容器配置里的字面值无法跟住 session 变化——历史上因
///   `.mutter-Xwaylandauth.XXXXXX` 字面占位符 + 文件不存在 → Chrome "Authorization
///   required" 的 bug 链就是这条。
/// - 这是 session 耦合运行时数据,不是用户配置。把它做进 server:
///   容器每次启动 server 时,**忽略** podman create 时可能注入的任何
///   `XAUTHORITY`(包括 host 注入 + 用户模板声明),由 server 自己探 $XDG_RUNTIME_DIR
///   下已知模式,覆盖写进程 env。后续 pty.open / apps.launch 经 `std::env::vars()`
///   取到的就是 server 持有值。
///
/// **探针模式**(按优先序取第一个匹配):
///   1. `mutter-Xwaylandauth.*`(GNOME/Mutter 启动 Xwayland 时生成)
///   2. `xauth_*`(Xorg 原生或老会话)
///
/// 取不到时仅 warn——非 GUI 容器(纯 headless)不应被这条路径阻碍启动。
pub(crate) fn ensure_xauthority() -> Option<String> {
    let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR") else {
        tracing::debug!("未设 XDG_RUNTIME_DIR,跳过 XAUTHORITY 自动注入");
        return None;
    };
    let Ok(read_dir) = std::fs::read_dir(&runtime) else {
        tracing::debug!("XDG_RUNTIME_DIR ({runtime}) 不可读,跳过 XAUTHORITY 自动注入");
        return None;
    };
    for entry in read_dir.flatten() {
        let name = entry.file_name();
        let Some(n) = name.to_str() else { continue };
        if n.starts_with("mutter-Xwaylandauth.") || n.starts_with("xauth_") {
            let path = format!("{runtime}/{n}");
            tracing::info!("server 自动注入 XAUTHORITY={path} (覆盖 podman create 时可能注入的旧值)");
            // 覆盖进程 env——后续 pty.open 经 std::env::vars() 取到的就是这个值。
            // std::env::set_var 在多线程下是 unsafe(race),server 此时仍单线程
            // (未启动 listener / accept 循环),安全。
            std::env::set_var("XAUTHORITY", &path);
            return Some(path);
        }
    }
    tracing::warn!(
        "未在 {runtime} 找到 X11 auth 文件(mutter-Xwaylandauth.* 或 xauth_*);\
         X GUI 透传可能受限——headless 容器或 host 未挂载 XDG_RUNTIME_DIR 时正常"
    );
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    const PASSWD: &str = "root:x:0:0:root:/root:/bin/sh\n\
                     tidy:x:1000:1000::/home/tidy:/bin/bash\n\
                     other:x:1001:1001::/home/other:/bin/sh\n";

    #[test]
    fn test_identity_named_user() {
        // 配置用户名：名字 = env 值，home = /home/<name>（不依赖 passwd 条目）
        let id = identity_from_inputs(PASSWD, 1000, 1000, Some("tidy"), Some("/home/div"));
        assert_eq!(id.name, "tidy");
        assert_eq!(id.home, "/home/tidy");
        // useradd 尚未执行（无 passwd 条目）也成立
        let id = identity_from_inputs("", 1000, 1000, Some("tidy"), None);
        assert_eq!(id.name, "tidy");
        assert_eq!(id.home, "/home/tidy");
    }

    #[test]
    fn test_identity_passwd_lookup() {
        // 未配用户名 + passwd 有条目 → 反查名字与 home
        let id = identity_from_inputs(PASSWD, 1000, 1000, None, Some("/home/div"));
        assert_eq!(id.name, "tidy");
        assert_eq!(id.home, "/home/div", "EASYTIDY_HOME 优先于 passwd home");
    }

    #[test]
    fn test_identity_passwd_home_fallback() {
        // 未配用户名 + 无 HOME 提示 → passwd 条目 home
        let id = identity_from_inputs(PASSWD, 1000, 1000, None, None);
        assert_eq!(id.name, "tidy");
        assert_eq!(id.home, "/home/tidy");
    }

    #[test]
    fn test_identity_no_passwd_entry() {
        // 未配用户名 + 无 passwd 条目（正常形态）→ uid<uid> + EASYTIDY_HOME
        let id = identity_from_inputs(PASSWD, 2000, 2000, None, Some("/home/div"));
        assert_eq!(id.name, "uid2000");
        assert_eq!(id.home, "/home/div");
        // 连 HOME 提示都没有 → "/"
        let id = identity_from_inputs(PASSWD, 2000, 2000, None, None);
        assert_eq!(id.name, "uid2000");
        assert_eq!(id.home, "/");
    }

    #[test]
    fn test_identity_empty_env_ignored() {
        // 空串 env 按缺失处理
        let id = identity_from_inputs(PASSWD, 1000, 1000, Some(""), Some(""));
        assert_eq!(id.name, "tidy");
        assert_eq!(id.home, "/home/tidy");
    }

    #[test]
    fn test_self_uid_gid_reads_proc() {
        // 真实进程身份：非负且与 libc 一致（Linux 容器环境）
        let (uid, gid) = self_uid_gid();
        assert_eq!(uid, unsafe { libc::getuid() });
        assert_eq!(gid, unsafe { libc::getgid() });
    }

    /// 测试串行化:`ensure_xauthority` 修改的是**进程全局 env**(XDG_RUNTIME_DIR +
    /// XAUTHORITY),并行跑会让测试互相污染(一个测试设的目录会被另一个读到)。
    /// 全局锁串行执行——env 测试套件本来就该跑得很快。
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// 临时把 XDG_RUNTIME_DIR 指向 tempfile,模拟用户登录会话的 runtime 目录。
    /// 还原旧值:测后无论成功失败都恢复 XDG_RUNTIME_DIR 与 XAUTHORITY,
    /// 避免污染后续测试(包括 cargo test 并行运行的其他 crate 测试)。
    struct RuntimeDirGuard {
        prev: Option<String>,
        xauthority_prev: Option<String>,
    }
    impl RuntimeDirGuard {
        fn new(dir: &std::path::Path) -> Self {
            let prev = std::env::var("XDG_RUNTIME_DIR").ok();
            std::env::set_var("XDG_RUNTIME_DIR", dir);
            let xauthority_prev = std::env::var("XAUTHORITY").ok();
            Self { prev, xauthority_prev }
        }
    }
    impl Drop for RuntimeDirGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
                None => std::env::remove_var("XDG_RUNTIME_DIR"),
            }
            match &self.xauthority_prev {
                Some(v) => std::env::set_var("XAUTHORITY", v),
                None => std::env::remove_var("XAUTHORITY"),
            }
        }
    }

    #[test]
    fn test_ensure_xauthority_discovers_mutter() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let _g = RuntimeDirGuard::new(tmp.path());

        // 模拟 Mutter 生成的 Xwayland auth 文件
        let auth_path = tmp.path().join("mutter-Xwaylandauth.hzQT2z");
        std::fs::write(&auth_path, b"mock-cookie").unwrap();

        let result = ensure_xauthority();
        assert_eq!(result.as_deref(), Some(auth_path.to_str().unwrap()));
        assert_eq!(
            std::env::var("XAUTHORITY").ok().as_deref(),
            Some(auth_path.to_str().unwrap()),
            "ensure_xauthority 应覆盖进程 env"
        );
    }

    #[test]
    fn test_ensure_xauthority_discovers_xauth_prefix() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let _g = RuntimeDirGuard::new(tmp.path());

        let auth_path = tmp.path().join("xauth_abc123");
        std::fs::write(&auth_path, b"mock-cookie").unwrap();

        let result = ensure_xauthority();
        assert_eq!(result.as_deref(), Some(auth_path.to_str().unwrap()));
    }

    #[test]
    fn test_ensure_xauthority_overrides_user_config() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let _g = RuntimeDirGuard::new(tmp.path());

        // 用户在 YAML 里写的字面占位符(历史 bug 链)
        std::env::set_var(
            "XAUTHORITY",
            "/run/user/1000/.mutter-Xwaylandauth.XXXXXX",
        );

        // server 启动后自动发现真实 auth 文件,覆盖用户配的占位符
        let real_auth = tmp.path().join("mutter-Xwaylandauth.real");
        std::fs::write(&real_auth, b"cookie").unwrap();

        ensure_xauthority();
        assert_eq!(
            std::env::var("XAUTHORITY").ok().as_deref(),
            Some(real_auth.to_str().unwrap()),
            "server 必须覆盖用户配的字面值"
        );
    }

    #[test]
    fn test_ensure_xauthority_no_xdg_returns_none() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _g = RuntimeDirGuard {
            prev: std::env::var("XDG_RUNTIME_DIR").ok(),
            xauthority_prev: std::env::var("XAUTHORITY").ok(),
        };
        std::env::remove_var("XDG_RUNTIME_DIR");
        assert_eq!(ensure_xauthority(), None);
    }

    #[test]
    fn test_ensure_xauthority_no_auth_file_returns_none() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let _g = RuntimeDirGuard::new(tmp.path());
        // 空目录:无 auth 文件,headless 容器场景
        assert_eq!(ensure_xauthority(), None);
    }
}
