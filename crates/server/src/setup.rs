//! 用户身份 setup（身份自发现）+ XDG 修正 + XAUTHORITY 探测。
//!
//! 新模型（2026-08-27）：server 即容器默认用户——容器 `User` 字段直接
//! 指向配置 uid:gid（宿主侧 create_with_config），server 无需建号/降权。
//! 身份来源（零 getent 依赖，server 与自身 uid 同权限）：
//! 1. `/proc/self/status`（Uid/Gid 行）
//! 2. `/etc/passwd`（uid 反查名字与 home；**名字与 HOME 同源**——
//!    容器默认用户自身的条目，如 ubuntu 的 /home/ubuntu）
//! 3. 宿主侧注入的提示 env：`EASYTIDY_USER_NAME`（配置用户名，与
//!    容器内建号命名一致；镜像无该 uid 真实条目且建号尚未执行时
//!    的名字/home 来源）
//!
//! 名字/HOME 解析的**单一事实源**在 `easytidy_core::incontainer::
//! resolve_identity`——与容器内 ctool 的 ensure-home 共用同一函数，
//! 保证 server 的 HOME 与 ctool 实际补齐的目录必然一致（2026-08-28
//! 定案：家目录跟随容器默认用户 passwd home，容器层持久）。
//!
//! 旧 root 容器（User=0:0）不再支持完整功能：检测到 euid==0 仅告警，
//! 按 uid 0 身份继续（存量测试容器能跑即可，装包走宿主 root 通道）。
//! 容器内建号/sudoers/fontconfig 等 root 操作已全部迁到宿主侧
//! （`core::podman::user::prepare_container` → 容器内 easytidy-ctool）。

use std::fs;
use std::sync::OnceLock;

use tracing::{info, warn};

/// 当前身份（`easytidy_core::incontainer::Identity` 的本地别名——
/// 名字/HOME 解析与容器内 ctool 共用 core 的单一事实源）。
pub(crate) type UserMap = easytidy_core::incontainer::Identity;

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

/// server 运行时注入的环境变量（启动期 setup 探测/修正的 session 耦合值）。
///
/// 与 create-time 注入（宿主侧 flavor 展开，配置可预知）相对：这些值取决于
/// 容器运行时环境（$XDG_RUNTIME_DIR 下随机后缀 auth 文件等），配置里定义不了、
/// 宿主侧也无法预知最终值。宿主配置管理器经 `server.env` 查询后展示为
/// 「easytidy 注入」只读 env 行。
pub(crate) struct InjectedEnv {
    pub key: &'static str,
    pub value: String,
    pub note: &'static str,
}

/// 启动期记录的注入项（`finalize_injected_env` 后写入，listen 前）。
pub(crate) static INJECTED_ENV: OnceLock<Vec<InjectedEnv>> = OnceLock::new();

/// 汇总 server 运行时注入的环境变量（启动期 setup 完成后、listen 前调用）。
///
/// `xdg_fixed`：XDG_DATA_DIRS 被修正时的新值（未修正 = None）；
/// `xauth`：XAUTHORITY 探测到的 auth 文件路径（未探测到 = None）。
/// 仅记录**确实发生**的注入——未修正/未探测到时不产生条目。
pub(crate) fn finalize_injected_env(xdg_fixed: Option<String>, xauth: Option<String>) {
    let mut items = Vec::new();
    if let Some(v) = xdg_fixed {
        items.push(InjectedEnv {
            key: "XDG_DATA_DIRS",
            value: v,
            note: "追加系统默认数据目录（/usr/local/share、/usr/share）",
        });
    }
    if let Some(v) = xauth {
        items.push(InjectedEnv {
            key: "XAUTHORITY",
            value: v,
            note: "自动探测 X11 auth 文件（$XDG_RUNTIME_DIR 下 [.]mutter-Xwaylandauth.* / xauth_*）",
        });
    }
    let _ = INJECTED_ENV.set(items);
}

/// 当前记录的 server 注入项（供 `server.env` 查询；未初始化返回空）。
pub(crate) fn injected_env() -> &'static [InjectedEnv] {
    INJECTED_ENV.get().map(|v| v.as_slice()).unwrap_or(&[])
}

/// 启动期身份初始化（main 初始化后、listen 前调用；同步，无 IO 阻塞点）。
///
/// 名字/HOME 解析委托 `easytidy_core::incontainer::resolve_identity`
/// （与容器内 ctool 的 ensure-home 同一事实源）。
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
    let identity = easytidy_core::incontainer::resolve_identity(&passwd, uid, gid, env_name.as_deref());
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
///
/// 返回修正后的新值（**确实发生**修正时）；未修正（值已含系统默认 / 未设
/// XDG_DATA_DIRS）返回 `None`——供 `finalize_injected_env` 记录「easytidy 注入」。
pub(crate) fn fixup_xdg_data_dirs() -> Option<String> {
    let Ok(v) = std::env::var("XDG_DATA_DIRS") else {
        return None;
    };
    let merged = fixup_xdg_data_dirs_value(&v);
    if merged != v {
        info!("XDG_DATA_DIRS 已修正（追加系统默认）: {merged}");
        std::env::set_var("XDG_DATA_DIRS", &merged);
        Some(merged)
    } else {
        None
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
/// **探针模式**(取第一个匹配,剥掉可能的首个 `.` 前缀后再判):
///   1. `[.]mutter-Xwaylandauth.*`(GNOME/Mutter 启动 Xwayland 时生成——**实际是
///      点前缀** `.mutter-Xwaylandauth.<rand>`)
///   2. `[.]xauth_*`(Xorg 原生或老会话)
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
        // GNOME/Mutter 实际生成**点前缀**文件（`.mutter-Xwaylandauth.<rand>`），
        // 剥掉可能的首个 `.` 后匹配，兼容有无点前缀两种形态；`xauth_*` 同理。
        let stripped = n.strip_prefix('.').unwrap_or(n);
        if stripped.starts_with("mutter-Xwaylandauth.") || stripped.starts_with("xauth_") {
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
        "未在 {runtime} 找到 X11 auth 文件([.]mutter-Xwaylandauth.* 或 xauth_*);\
         X GUI 透传可能受限——headless 容器或 host 未挂载 XDG_RUNTIME_DIR 时正常"
    );
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // 身份解析（resolve_identity）的测试随单一事实源迁到
    // easytidy_core::incontainer::tests（server 与 ctool 共用）。

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

    /// 回归:GNOME/Mutter 实际生成**点前缀**文件 `.mutter-Xwaylandauth.<rand>`
    /// (2026-08-31 实测,Chrome "Authorization required" 根因——旧探针只匹配
    /// 无前缀 `mutter-Xwaylandauth.*`,漏了点前缀形态)。
    #[test]
    fn test_ensure_xauthority_discovers_dot_prefixed_mutter() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let _g = RuntimeDirGuard::new(tmp.path());

        let auth_path = tmp.path().join(".mutter-Xwaylandauth.DE23U3");
        std::fs::write(&auth_path, b"mock-cookie").unwrap();

        let result = ensure_xauthority();
        assert_eq!(result.as_deref(), Some(auth_path.to_str().unwrap()));
        assert_eq!(
            std::env::var("XAUTHORITY").ok().as_deref(),
            Some(auth_path.to_str().unwrap()),
            "点前缀 .mutter-Xwaylandauth.* 必须被探测到"
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

    /// 记录逻辑：只记录**确实发生**的注入——XAUTHORITY 探测到 → 记录；
    /// XDG_DATA_DIRS 未修正(None) → 不记录。`server.env` 据此返回。
    #[test]
    fn test_finalize_injected_env_records_only_actual() {
        // 注意：INJECTED_ENV 是全局 OnceLock，进程内 set 一次后不可再 set——
        // 其它测试未调用 finalize_injected_env，故此测试独占该 static。
        finalize_injected_env(None, Some("/run/user/1000/.mutter-Xwaylandauth.DE23U3".to_string()));
        let items = injected_env();
        assert_eq!(items.len(), 1, "未修正的 XDG_DATA_DIRS 不应产生条目");
        assert_eq!(items[0].key, "XAUTHORITY");
        assert_eq!(items[0].value, "/run/user/1000/.mutter-Xwaylandauth.DE23U3");
        assert!(!items[0].note.is_empty());
    }
}
