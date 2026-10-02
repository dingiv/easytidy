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
//! 名字/HOME 解析的**单一事实源**在 `easytidy_core::env::
//! resolve_identity`——与容器内 ctool 的 ensure-home 共用同一函数，
//! 保证 server 的 HOME 与 ctool 实际补齐的目录必然一致（2026-08-28
//! 定案：家目录跟随容器默认用户 passwd home，容器层持久）。
//!
//! 旧 root 容器（User=0:0）不再支持完整功能：检测到 euid==0 仅告警，
//! 按 uid 0 身份继续（存量测试容器能跑即可，装包走宿主 root 通道）。
//! 容器内建号/sudoers/fontconfig 等 root 操作已全部迁到宿主侧
//! （`core::podman::user::prepare_container` → 容器内 easytidy-dock prepare）。

use std::fs;
use std::sync::OnceLock;

use tracing::{info, warn};

/// 当前身份（`easytidy_core::env::Identity` 的本地别名——
/// 名字/HOME 解析与容器内 ctool 共用 core 的单一事实源）。
pub(crate) type UserMap = easytidy_core::env::Identity;

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
            note: "X11 auth 稳定路径（真实文件由 server 软链维护，podman exec 等进程同样生效）",
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
/// 名字/HOME 解析委托 `easytidy_core::env::resolve_identity`
/// （与容器内 ctool 的 ensure-home 同一事实源）。
pub(crate) fn setup_user_identity() -> UserMap {
    let (uid, gid) = easytidy_core::env::self_uid_gid();
    if uid == 0 {
        warn!(
            "server 以 root（uid 0）运行——旧形态容器，新模型不再支持 \
             （装包/root 操作走宿主 root 通道）；按 uid 0 身份继续"
        );
    }
    let passwd = fs::read_to_string("/etc/passwd").unwrap_or_default();
    let env_name = std::env::var("EASYTIDY_USER_NAME").ok();
    let identity = easytidy_core::env::resolve_identity(&passwd, uid, gid, env_name.as_deref());
    let _ = USER_MAP.set(identity.clone());
    info!(
        "身份自发现：{}({}:{}) home={}",
        identity.name, identity.uid, identity.gid, identity.home
    );
    identity
}

/// 修正 server 进程自身的 XDG_DATA_DIRS（子进程继承）——薄壳：值计算（纯）委托
/// `easytidy_core::env::fixup_xdg_data_dirs_value`，本处只读 env + set_var
/// （进程副作用）。
///
/// 返回修正后的新值（**确实发生**修正时）；未修正（值已含系统默认 / 未设
/// XDG_DATA_DIRS）返回 `None`——供 `finalize_injected_env` 记录「easytidy 注入」。
pub(crate) fn fixup_xdg_data_dirs() -> Option<String> {
    let Ok(v) = std::env::var("XDG_DATA_DIRS") else {
        return None;
    };
    let merged = easytidy_core::env::fixup_xdg_data_dirs_value(&v);
    if merged != v {
        info!("XDG_DATA_DIRS 已修正（追加系统默认）: {merged}");
        std::env::set_var("XDG_DATA_DIRS", &merged);
        Some(merged)
    } else {
        None
    }
}

/// 探测 X11 auth 文件并按**三态语义**处理 `XAUTHORITY`（server 内置 GUI 透传）。
///
/// **三态语义（2026-09-17 用户显式配置支持）**，按容器 spec Env 的 `XAUTHORITY`
/// 取值分派：
/// 1. **空/未设**：`gui_x11` 关，或用户显式声明空值（用户声明空 = 显式不要
///    X11 auth，引擎不注入、server 也不做感知）→ 直接返回 `None`（不探测 /
///    不建软链 / 不 set_var）。
/// 2. **core 稳定路径**（`<auth_dir>/xauthority`）：引擎注入的常规 X11 透传 →
///    探测真实 auth 文件并维护稳定软链，进程 env 保持稳定路径不变。
/// 3. **用户显式指定的其他路径**：用户配置优先——**不覆盖**进程 env（保持
///    用户值）；感知改为维护「用户指定路径 → 真实 auth 文件」软链（会话
///    轮换时随 [`xauthority_watch_task`] 重链），让用户路径跟住 session 变化。
///
/// **用户显式配置的路径风险（用户须知）**：真实 auth 文件路径含随机后缀
/// （典型 `/run/user/$uid/mutter-Xwaylandauth.<random>`，compositor 登录
/// 会话时随机生成）。用户写的固定路径无法直接跟住 session 变化——模式 3
/// 用「用户路径作软链位」适配，而非硬指向。
///
/// **稳定间接路径模型**（2026-09-03，模式 2）：
/// - 创建期（gui-passthrough.yaml）注入 `XAUTHORITY=<socket 目录>/xauthority`
///   ——路径稳定、进容器 spec Env，**所有**容器内进程（含 `podman exec`、
///   `ets`）都拿到同一值；
/// - server 启动时探到真实文件 → 维护软链 `<socket 目录>/xauthority → 真实文件`；
/// - [`xauthority_watch_task`] 周期重探：会话轮换（新随机名/注销）时重链，
///   进程 env 恒定不变（只换链目标）。
///
/// 返回 `Some(稳定路径)` 仅限模式 2（确实发生了注入，供 `server.env` 申报）；
/// 模式 3 返回 `None`（env 是用户自己配的，不算注入）。取不到真实文件时仅
/// warn——非 GUI 容器（纯 headless）不应被这条路径阻碍启动。
///
/// `auth_dir` = XAUTHORITY 稳定路径所在目录（server 的 socket 目录，容器内
/// 即 `/run/easytidy`，bind-mount rw）。
pub(crate) fn ensure_xauthority(auth_dir: &std::path::Path) -> Option<String> {
    // 三态门控：spec env（= 进程初始 env）的 XAUTHORITY 空/未设 → 模式 1：
    // gui_x11 关或用户显式声明空值（用户声明空 = 不要 X11 auth）——不注入、
    // 不做感知。
    let env_value = std::env::var("XAUTHORITY").ok().filter(|v| !v.is_empty())?;
    let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR") else {
        tracing::debug!("未设 XDG_RUNTIME_DIR,跳过 XAUTHORITY 自动注入");
        return None;
    };
    let Some(path_str) = easytidy_core::env::probe_xauthority(std::path::Path::new(&runtime))
    else {
        tracing::warn!(
            "未在 {runtime} 找到 X11 auth 文件([.]mutter-Xwaylandauth.* 或 xauth_*);\
             X GUI 透传可能受限——headless 容器或 host 未挂载 XDG_RUNTIME_DIR 时正常"
        );
        return None;
    };
    let path = std::path::PathBuf::from(path_str);
    let stable = auth_dir.join(XAUTHORITY_STABLE_FILE);
    // 模式 3（用户显式指定非稳定路径）→ 用户配置优先：软链位用用户路径，
    // 进程 env 保持用户值不覆盖；模式 2（core 稳定路径）→ 间接路径模型。
    let user_specified = std::path::Path::new(&env_value) != stable;
    let link_path = if user_specified {
        tracing::info!(
            "XAUTHORITY 用户显式指定（{}）：用户配置优先，感知软链维护到用户路径",
            env_value
        );
        std::path::PathBuf::from(&env_value)
    } else {
        stable.clone()
    };
    if link_path == path {
        // 用户路径即真实 auth 文件本身：无需软链，保持原样（也不申报注入）
        return None;
    }
    write_xauthority_link(&link_path, &path);
    tracing::info!(
        "XAUTHORITY 软链 {} → 真实文件 {}",
        link_path.display(),
        path.display()
    );
    if user_specified {
        // 用户值已在 env 里，不覆盖；返回 None = 不向 `server.env` 申报注入
        //（env 是用户自己配的，不是 server 注入的）
        return None;
    }
    // 覆盖进程 env——后续 pty.open / apps.launch 经 std::env::vars() 取到的
    // 就是稳定路径。std::env::set_var 在多线程下是 unsafe(race),server 此时
    // 仍单线程(未启动 listener / accept 循环),安全。
    let stable_str = stable.to_string_lossy().into_owned();
    std::env::set_var("XAUTHORITY", &stable_str);
    Some(stable_str)
}

/// 维护软链 `link_path → target`（存在则替换）。
///
/// `link_path` 为软链完整路径（core 稳定路径，或用户显式指定的路径——模式 3）。
/// 失败（目录不可写等）仅 warn 不报错——X11 透传是增强项，不应阻断 server
/// 启动。
fn write_xauthority_link(link_path: &std::path::Path, target: &std::path::Path) {
    use std::os::unix::fs::symlink;
    match std::fs::remove_file(link_path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!("清理旧 {} 失败：{e}", link_path.display()),
    }
    if let Err(e) = symlink(target, link_path) {
        tracing::warn!(
            "写 XAUTHORITY 软链 {} → {} 失败：{e}",
            link_path.display(),
            target.display()
        );
    }
}

/// XAUTHORITY 稳定文件名（与 gui-passthrough.yaml 注入的
/// `XAUTHORITY=/run/easytidy/xauthority` 尾段一致）。
pub(crate) const XAUTHORITY_STABLE_FILE: &str = "xauthority";

/// XAUTHORITY 重探任务：每 [`XAUTHORITY_WATCH_INTERVAL`] 重探一次 auth 文件，
/// 变化（会话轮换 / 注销后重登换了随机名）则重链稳定路径。
///
/// 只动软链、**不写进程 env**——稳定路径值恒定，子进程经软链读到最新内容，
/// 无 env race。探不到（注销中）保留旧链（无害，X11 本就失效）。
/// 任务随进程退出自然终止（不 join）。
pub(crate) async fn xauthority_watch_task() {
    let interval = XAUTHORITY_WATCH_INTERVAL;
    let mut last_target: Option<std::path::PathBuf> = None;
    loop {
        tokio::time::sleep(interval).await;
        // 软链位 = 当前进程 env 的 XAUTHORITY（core 稳定路径或用户显式指定
        // 的路径——三态模式 2/3 统一：env 值恒定不变，只换链目标）。
        let Some(link) = std::env::var("XAUTHORITY")
            .ok()
            .filter(|v| !v.is_empty())
            .map(std::path::PathBuf::from)
        else {
            continue; // 模式 1（空/未设）：无 X11 auth 透传，不感知
        };
        let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR") else {
            continue;
        };
        let Some(path_str) = easytidy_core::env::probe_xauthority(std::path::Path::new(&runtime))
        else {
            continue; // 探不到：保留旧链
        };
        let path = std::path::PathBuf::from(path_str);
        if link == path {
            // 用户路径即真实文件本身：无需软链
            last_target = Some(path);
            continue;
        }
        if last_target.as_ref() != Some(&path) {
            let prev = last_target
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(首次)".to_string());
            info!("XAUTHORITY 重探：auth 文件变化 {prev} → {}", path.display());
            write_xauthority_link(&link, &path);
            last_target = Some(path);
        }
    }
}

/// 重探周期：auth 文件只在会话轮换时变，30s 足够且成本仅一次目录扫描。
const XAUTHORITY_WATCH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // 身份解析（resolve_identity）与 env 探测纯函数（self_uid_gid / probe_xauthority /
    // fixup_xdg_data_dirs_value）的测试随单一事实源迁到
    // easytidy_core::env::incontainer::tests（server 与 ctool 共用）。本处只留
    // ensure_xauthority / fixup_xdg_data_dirs 的**进程副作用**（set_var）测试。

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
            // 模拟 gui_x11=true 容器：core 烘焙 XAUTHORITY 稳定路径进 spec Env，
            // server 进程 env 继承它——ensure_xauthority 的三态门控据此放行。
            // 稳定路径 = <auth_dir>/xauthority（auth_dir 即传入的 tmp 目录）。
            std::env::set_var("XAUTHORITY", dir.join("xauthority"));
            Self {
                prev,
                xauthority_prev,
            }
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

        let result = ensure_xauthority(tmp.path());
        let stable = tmp.path().join(XAUTHORITY_STABLE_FILE);
        assert_eq!(
            result.as_deref(),
            Some(stable.to_str().unwrap()),
            "返回值应为稳定路径"
        );
        assert_eq!(
            std::env::var("XAUTHORITY").ok().as_deref(),
            Some(stable.to_str().unwrap()),
            "ensure_xauthority 应把进程 env 设为稳定路径"
        );
        // 稳定路径软链解析到真实 auth 文件
        assert_eq!(
            std::fs::canonicalize(&stable).unwrap(),
            std::fs::canonicalize(&auth_path).unwrap()
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

        let result = ensure_xauthority(tmp.path());
        let stable = tmp.path().join(XAUTHORITY_STABLE_FILE);
        assert_eq!(result.as_deref(), Some(stable.to_str().unwrap()));
        assert_eq!(
            std::env::var("XAUTHORITY").ok().as_deref(),
            Some(stable.to_str().unwrap()),
            "点前缀 .mutter-Xwaylandauth.* 必须被探测到"
        );
        assert_eq!(
            std::fs::canonicalize(&stable).unwrap(),
            std::fs::canonicalize(&auth_path).unwrap()
        );
    }

    #[test]
    fn test_ensure_xauthority_discovers_xauth_prefix() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let _g = RuntimeDirGuard::new(tmp.path());

        let auth_path = tmp.path().join("xauth_abc123");
        std::fs::write(&auth_path, b"mock-cookie").unwrap();

        let result = ensure_xauthority(tmp.path());
        let stable = tmp.path().join(XAUTHORITY_STABLE_FILE);
        assert_eq!(result.as_deref(), Some(stable.to_str().unwrap()));
    }

    /// 三态模式 1（2026-09-17）：用户显式声明**空值**（XAUTHORITY=""）→ 不注入、
    /// 不做感知（返回 None，不建软链、不改 env）。
    #[test]
    fn test_ensure_xauthority_empty_user_value_skips() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let prev = std::env::var("XAUTHORITY").ok();
        let prev_rt = std::env::var("XDG_RUNTIME_DIR").ok();
        std::env::set_var("XDG_RUNTIME_DIR", tmp.path());
        std::env::set_var("XAUTHORITY", "");
        // runtime 下放真实 auth 文件——空值门控应直接短路，不碰它
        std::fs::write(tmp.path().join("mutter-Xwaylandauth.real"), b"cookie").unwrap();

        assert_eq!(ensure_xauthority(tmp.path()), None, "空值应短路");
        assert_eq!(
            std::env::var("XAUTHORITY").ok().as_deref(),
            Some(""),
            "空值不应被改写"
        );
        assert!(
            !tmp.path().join(XAUTHORITY_STABLE_FILE).exists(),
            "空值不应建稳定软链（不感知）"
        );
        // 还原
        match prev_rt {
            Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
            None => std::env::remove_var("XDG_RUNTIME_DIR"),
        }
        match prev {
            Some(v) => std::env::set_var("XAUTHORITY", v),
            None => std::env::remove_var("XAUTHORITY"),
        }
    }

    /// 三态模式 3（2026-09-17）：用户显式指定非稳定路径 → 用户配置优先：
    /// **不覆盖** env（保持用户值）；感知改为维护「用户路径 → 真实文件」软链。
    #[test]
    fn test_ensure_xauthority_user_path_wins() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let _g = RuntimeDirGuard::new(tmp.path());

        // 用户显式指定自己的 auth 路径（非 core 稳定路径）
        let user_path = tmp.path().join("my-xauthority");
        std::env::set_var("XAUTHORITY", &user_path);

        let real_auth = tmp.path().join("mutter-Xwaylandauth.real");
        std::fs::write(&real_auth, b"cookie").unwrap();

        let result = ensure_xauthority(tmp.path());
        assert_eq!(result, None, "用户配置不算注入，不应向 server.env 申报");
        assert_eq!(
            std::env::var("XAUTHORITY").ok().as_deref(),
            Some(user_path.to_str().unwrap()),
            "用户显式指定的 XAUTHORITY 不应被覆盖"
        );
        // 感知软链位 = 用户路径（跟住 session 变化）
        assert_eq!(
            std::fs::canonicalize(&user_path).unwrap(),
            std::fs::canonicalize(&real_auth).unwrap(),
            "用户路径应被维护为指向真实 auth 文件的软链"
        );
        // core 稳定路径不应被动（用户优先，引擎不越权）
        assert!(
            !tmp.path().join(XAUTHORITY_STABLE_FILE).exists(),
            "用户显式指定时不应另建稳定软链"
        );
    }

    /// 三态模式 3 变体：用户路径即真实 auth 文件本身 → 无需软链，保持原样。
    #[test]
    fn test_ensure_xauthority_user_path_is_real_file() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let _g = RuntimeDirGuard::new(tmp.path());

        let real_auth = tmp.path().join("mutter-Xwaylandauth.real");
        std::fs::write(&real_auth, b"cookie").unwrap();
        std::env::set_var("XAUTHORITY", &real_auth);

        let result = ensure_xauthority(tmp.path());
        assert_eq!(result, None);
        assert_eq!(
            std::env::var("XAUTHORITY").ok().as_deref(),
            Some(real_auth.to_str().unwrap()),
            "用户直接指向真实文件时保持原样"
        );
        assert!(
            !tmp.path().join(XAUTHORITY_STABLE_FILE).exists(),
            "不应另建稳定软链"
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
        assert_eq!(
            ensure_xauthority(std::path::Path::new("/nonexistent-auth-dir")),
            None
        );
    }

    /// 门控回归（2026-09-10 双半拆分）：spec env 无 XAUTHORITY（gui_x11 关，
    /// 纯 Wayland / headless）→ 即使 XDG_RUNTIME_DIR 下有 auth 文件也不注入。
    #[test]
    fn test_ensure_xauthority_skips_when_not_requested() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let _g = RuntimeDirGuard::new(tmp.path());

        // XDG 下有 auth 文件（宿主确实是 GUI 会话）
        let auth_path = tmp.path().join(".mutter-Xwaylandauth.DE23U3");
        std::fs::write(&auth_path, b"mock-cookie").unwrap();

        // 但 spec env 无 XAUTHORITY（gui_x11 关）→ 门控拦截
        std::env::remove_var("XAUTHORITY");
        assert_eq!(
            ensure_xauthority(tmp.path()),
            None,
            "gui_x11 关时不应注入 XAUTHORITY"
        );
        assert!(
            !tmp.path().join(XAUTHORITY_STABLE_FILE).exists(),
            "gui_x11 关时不应创建稳定软链"
        );
    }

    #[test]
    fn test_ensure_xauthority_no_auth_file_returns_none() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let _g = RuntimeDirGuard::new(tmp.path());
        // 空目录:无 auth 文件,headless 容器场景
        assert_eq!(ensure_xauthority(tmp.path()), None);
        // 未探测到 → 不应创建稳定软链
        assert!(!tmp.path().join(XAUTHORITY_STABLE_FILE).exists());
    }

    /// 记录逻辑：只记录**确实发生**的注入——XAUTHORITY 探测到 → 记录；
    /// XDG_DATA_DIRS 未修正(None) → 不记录。`server.env` 据此返回。
    #[test]
    fn test_finalize_injected_env_records_only_actual() {
        // 注意：INJECTED_ENV 是全局 OnceLock，进程内 set 一次后不可再 set——
        // 其它测试未调用 finalize_injected_env，故此测试独占该 static。
        finalize_injected_env(
            None,
            Some("/run/user/1000/.mutter-Xwaylandauth.DE23U3".to_string()),
        );
        let items = injected_env();
        assert_eq!(items.len(), 1, "未修正的 XDG_DATA_DIRS 不应产生条目");
        assert_eq!(items[0].key, "XAUTHORITY");
        assert_eq!(items[0].value, "/run/user/1000/.mutter-Xwaylandauth.DE23U3");
        assert!(!items[0].note.is_empty());
    }
}
