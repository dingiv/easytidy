//! 用户环境 setup（euid 分派）：用户映射建号 / XDG 修正 / fontconfig / su 工具。

use std::fs;
use std::path::Path;
use std::sync::OnceLock;

use tokio::process::Command as TokioCommand;
use tracing::{debug, info, warn};

pub(crate) struct UserMap {
    pub(crate) name: String,
    pub(crate) uid: u32,
    pub(crate) gid: u32,
    pub(crate) home: String,
}

/// 容器内用户固定名（与宿主用户名不同，符合"名字不同、uid 相同"语义）。
pub(crate) const CONTAINER_USER: &str = "easytidy";

/// server 自身 euid（/proc/self/status 解析，零依赖）。
///
/// euid 分派依据：容器默认用户 node 化后 server 以 node 运行（无建号/
/// su 能力）；旧容器（User=root）以 root 运行——同一二进制双行为。
pub(crate) fn current_euid() -> Option<u32> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find(|l| l.starts_with("Uid:"))
        .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
}

/// 用户映射全局态：`setup_user_mapping` 成功后才写入。
/// `user_map()` 返回 `None` 表示映射未生效（PTY/entry 回退 root /bin/sh，与旧版一致）。
pub(crate) static USER_MAP: OnceLock<UserMap> = OnceLock::new();

/// 当前生效的用户映射。
///
/// 默认 shell/应用的常规身份恒为容器内 node 用户（uid/gid 与宿主真实对齐，
/// 名字不同）。keep-id 语义（实测文件属主）：node（uid 1000）= 宿主登录
/// 用户（读宿主 /run/user/1000 显示 socket、写宿主 home 属主 1000）；
/// 容器 root（uid 0）= 容器层文件属主（宿主侧 subuid 100000，**不是宿主
/// 默认用户**）——装包身份，`run --root` 或免密 sudo 进入。
pub(crate) fn user_map() -> Option<&'static UserMap> {
    USER_MAP.get()
}

/// 从环境变量读取用户映射（EASYTIDY_USER_UID/GID，缺失返回 None）。
///
/// 容器内用户固定名为 `node`（与宿主用户名不同），home 为容器内
/// `/home/node`（用户add -m 创建，应用数据存容器层）——rootless podman 的
/// userns 偏移（容器 uid N → 宿主 100000+N）使"uid 对齐"仅为名义一致；
/// 宿主挂载 home 的写操作经免密 sudo 完成（见 setup_user_mapping 第 4 步）。
pub(crate) fn user_map_from_env() -> Option<UserMap> {
    let uid = std::env::var("EASYTIDY_USER_UID").ok()?.parse().ok()?;
    let gid = std::env::var("EASYTIDY_USER_GID").ok()?.parse().ok()?;
    Some(UserMap {
        name: CONTAINER_USER.to_string(),
        uid,
        gid,
        home: format!("/home/{CONTAINER_USER}"),
    })
}

/// 启动期用户映射 setup（main 初始化后、listen 前调用）。
///
/// 1. 组：`getent group <gid>` 未命中则 `groupadd -g <gid> <name>`（缺失回退
///    `addgroup -g <gid> <name>`，alpine/busybox 系）
/// 2. 用户（容器内固定名 `node`，uid/gid 取宿主值，三种情形）：
///    - uid 未占用：`useradd -m -u <uid> -g <gid> -s <shell> node`（容器内 home
///      `/home/node`，应用数据存容器层；缺失回退 adduser，alpine 系）
///    - uid 已存在且同名：无需操作
///    - uid 被镜像默认用户占用（如 ubuntu 镜像 uid 1000 = `ubuntu`）：`usermod -l`
///      改名 + `-d /home/node` + `-g <gid>` 对齐（仅 debian 系有 usermod）
/// 3. 容器内 home（/home/node）存在性 + 属主（容器内目录，chown 安全）
/// 4. ⚠️ 绝不 chown 宿主挂载的 home：会改写宿主文件属主、致宿主用户失权
///    （2026-08-06 实测事故）
/// 5. 免密 sudo：`/etc/sudoers.d/easytidy-node`（宿主 home 写操作/包管理经此提升）
///
/// rootless 说明：userns 偏移（容器 uid N → 宿主 100000+N）使"uid 对齐"为名义一致；
/// 应用常态化权限 = 容器内 node 用户权限；宿主挂载目录读可达、写经免密 sudo。
///
/// 返回用户映射是否生效（env 齐全且用户创建成功）；失败回退 root 运行。
pub(crate) async fn setup_user_mapping() -> bool {
    let Some(user) = user_map_from_env() else {
        debug!("未收到 EASYTIDY_USER_* 环境变量，跳过用户映射（root 容器）");
        return false;
    };

    // euid 分派：容器默认用户 node 化（新模型）下 server 以 node 运行——
    // 用户已由 init 镜像烘焙预置（创建链路临时 root 容器），server 无权
    // 也无需 useradd；校验存在即采用。root（旧容器）走下方完整建号逻辑。
    if current_euid().unwrap_or(0) != 0 {
        if user_exists(&user).await {
            info!(
                "node 容器：烘焙用户 {}({}:{}) 生效（免建号）",
                user.name, user.uid, user.gid
            );
            let _ = USER_MAP.set(user);
            return true;
        }
        warn!(
            "server 以非 root 运行但用户 {} 不存在（init 烘焙未执行？），跳过映射",
            user.name
        );
        return false;
    }

    // 1. 确保组存在
    if !group_gid_exists(user.gid).await {
        let gid = user.gid.to_string();
        let group_ok = if command_available("groupadd") {
            run_cmd(&["groupadd", "-g", &gid, &user.name]).await
        } else if command_available("addgroup") {
            run_cmd(&["addgroup", "-g", &gid, &user.name]).await
        } else {
            warn!("容器内缺少 groupadd/addgroup，无法创建组 {}（gid {}）", user.name, gid);
            false
        };
        if !group_ok {
            warn!("组创建未成功（可能已存在），继续：{}（gid {}）", user.name, gid);
        }
    }

    // 2. 确保用户存在（同名/同 uid/gid）
    match username_for_uid(user.uid).await {
        // uid 未占用 → 创建（debian 系 useradd；alpine/busybox 系 adduser）
        None => {
            let uid = user.uid.to_string();
            let gid = user.gid.to_string();
            let shell = user_shell_path();
            let user_ok = if command_available("useradd") {
                // -m：容器内 home（/home/node），应用数据存容器层（重启/重建保留）；
                // 宿主挂载 home 只读可达，写操作经免密 sudo
                run_cmd(&["useradd", "-m", "-u", &uid, "-g", &gid, "-s", shell, &user.name]).await
            } else if command_available("adduser") {
                run_cmd(&[
                    "adduser", "-D", "-u", &uid, "-G", &user.name, "-s", shell, "-h", &user.home,
                    &user.name,
                ])
                .await
            } else {
                warn!("容器内缺少 useradd/adduser，无法创建用户 {}", user.name);
                false
            };
            if !user_ok {
                warn!("用户创建失败，回退 root 运行：{}", user.name);
                return false;
            }
        }
        // uid 已存在且同名 → 无需操作
        Some(existing) if existing == user.name => {}
        // uid 被镜像默认用户占用（ubuntu 镜像 uid 1000 = ubuntu）→ usermod 改名对齐
        Some(existing) => {
            if !command_available("usermod") {
                warn!(
                    "uid {} 已被镜像用户 {} 占用且容器内无 usermod，无法对齐，回退 root 运行",
                    user.uid, existing
                );
                return false;
            }
            let gid = user.gid.to_string();
            let renamed = run_cmd(&["usermod", "-l", &user.name, &existing]).await;
            let home_ok = renamed && run_cmd(&["usermod", "-d", &user.home, &user.name]).await;
            let gid_ok = home_ok && run_cmd(&["usermod", "-g", &gid, &user.name]).await;
            if !gid_ok {
                warn!(
                    "usermod 对齐用户失败（{} → {}），回退 root 运行",
                    existing, user.name
                );
                return false;
            }
            info!(
                "镜像默认用户 {} 已重命名为 {}（uid {}）",
                existing, user.name, user.uid
            );
        }
    }

    // 3. 确保容器内 home 存在并归用户所有（容器内目录，非宿主挂载，chown 安全）
    if !Path::new(&user.home).exists() {
        run_cmd(&["mkdir", "-p", &user.home]).await;
    }
    let uid_gid = format!("{}:{}", user.uid, user.gid);
    run_cmd(&["chown", &uid_gid, &user.home]).await;

    // 4. ⚠️ 绝不 chown 宿主挂载的 home！
    //    宿主 home 是 bind mount，chown 会改写宿主机文件属主，导致宿主用户失去访问权
    //    （2026-08-06 实测事故：宿主环境崩溃）。权限一致性靠 uid/gid 对齐实现——
    //    容器用户与宿主用户同 uid/gid，天然拥有相同权限，无需也不能动属主。

    // 5. 免密 sudo：容器用户提升权限的通道（宿主 home 写操作、包管理等）。
    //    直接写 /etc/sudoers.d（root 写文件不依赖 sudo 二进制是否已装；
    //    flavor setup 可能在 server 启动后才安装 sudo——文件先就位，装好即生效）。
    //    alpine/busybox 系无 sudo：文件写了无害，装 sudo 后自然生效。
    {
        let sudoers = format!("/etc/sudoers.d/easytidy-{}", user.name);
        let rule = format!("{} ALL=(ALL) NOPASSWD: ALL\n", user.name);
        // 基础镜像装 sudo 前可能没有 /etc/sudoers.d（apt 装 sudo 才创建）——先建目录
        let _ = std::fs::create_dir_all("/etc/sudoers.d");
        let write_result = std::fs::write(&sudoers, rule);
        let chmod_ok = write_result.is_ok() && run_cmd(&["chmod", "440", &sudoers]).await;
        if chmod_ok {
            info!("免密 sudo 已配置：{}", user.name);
        } else if let Err(e) = write_result {
            warn!("免密 sudo 配置失败（写 {} 出错：{e}）", sudoers);
        } else {
            warn!("免密 sudo 配置失败（chmod 440 失败：{}）", sudoers);
        }
    }

    // 6. 校验 + 记录
    if !user_exists(&user).await {
        warn!("用户映射校验失败（用户未创建成功），回退 root 运行：{}", user.name);
        return false;
    }

    info!("用户映射：{}({}:{}) home={}", user.name, user.uid, user.gid, user.home);
    let _ = USER_MAP.set(user);
    true
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

/// 宿主字体接入 fontconfig。
///
/// flavor `gui=true` 把宿主 `/usr/share/fonts` 与 `~/.local/share/fonts` 只读挂载到
/// `/usr/share/easytidy-host/`（非覆盖容器自身目录，避免破坏字体/图标包安装）。
/// 此处写 `/etc/fonts/local.conf` 把这些目录接入 fontconfig（容器需有 fontconfig，
/// 否则跳过——多数发行版镜像自带）。
pub(crate) fn setup_fontconfig() {
    const HOST_ROOT: &str = "/usr/share/easytidy-host";
    if !Path::new(HOST_ROOT).exists() {
        return;
    }
    if !Path::new("/etc/fonts").is_dir() {
        info!("容器无 /etc/fonts（未装 fontconfig），跳过宿主字体接入");
        return;
    }
    let conf = format!(
        r#"<?xml version="1.0"?>
<!DOCTYPE fontconfig SYSTEM "fonts.dtd">
<fontconfig>
  <dir>{HOST_ROOT}/fonts</dir>
  <dir>{HOST_ROOT}/.local/share/fonts</dir>
</fontconfig>
"#
    );
    match std::fs::write("/etc/fonts/local.conf", conf) {
        Ok(()) => info!("宿主字体已接入 fontconfig（{HOST_ROOT}）"),
        Err(e) => warn!("写入 /etc/fonts/local.conf 失败：{e}"),
    }
}

/// 探测 PATH 中是否存在可执行命令（区分镜像系：debian/ubuntu 用 useradd/groupadd，
/// alpine/busybox 用 adduser/addgroup）。
pub(crate) fn command_available(cmd: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| dir.join(cmd).is_file())
    })
}

/// 容器内可用 shell：/bin/bash 优先（ubuntu 系），缺失回退 /bin/sh（alpine/busybox）。
pub(crate) fn user_shell_path() -> &'static str {    if Path::new("/bin/bash").exists() {
        "/bin/bash"
    } else {
        "/bin/sh"
    }
}

/// 运行命令（无输出捕获，返回成功与否）。
pub(crate) async fn run_cmd(args: &[&str]) -> bool {
    if args.is_empty() {
        return false;
    }
    match TokioCommand::new(args[0]).args(&args[1..]).status().await {
        Ok(s) => s.success(),
        Err(e) => {
            warn!("执行命令失败（{} {}）：{e}", args[0], args.join(" "));
            false
        }
    }
}

/// `getent <database> <key>` 查询（glibc 与 busybox 系均支持）。
pub(crate) async fn run_getent(database: &str, key: &str) -> bool {
    command_available("getent") && run_cmd(&["getent", database, key]).await
}

/// 组 gid 是否已存在（getent 优先；容器无 getent 时解析 /etc/group 兜底）。
pub(crate) async fn group_gid_exists(gid: u32) -> bool {
    if run_getent("group", &gid.to_string()).await {
        return true;
    }
    std::fs::read_to_string("/etc/group").ok().is_some_and(|content| {
        content.lines().any(|line| {
            let f: Vec<&str> = line.split(':').collect();
            f.len() >= 3 && f[2].parse::<u32>().ok() == Some(gid)
        })
    })
}

/// 用户（uid + name）是否已存在：/etc/passwd 直接解析（不依赖 getent）。
pub(crate) async fn user_exists(user: &UserMap) -> bool {
    username_for_uid(user.uid).await.as_deref() == Some(user.name.as_str())
}

/// /etc/passwd 中 uid 对应的用户名（不依赖 getent，容器无 getent 时兜底）。
pub(crate) async fn username_for_uid(uid: u32) -> Option<String> {
    let content = std::fs::read_to_string("/etc/passwd").ok()?;
    content.lines().find_map(|line| {
        let f: Vec<&str> = line.split(':').collect();
        if f.len() >= 3 && f[2].parse::<u32>().ok() == Some(uid) {
            Some(f[0].to_string())
        } else {
            None
        }
    })
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
