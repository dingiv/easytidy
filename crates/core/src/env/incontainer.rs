//! 容器内 root 一次性准备（`easytidy-dock prepare` 执行，宿主侧 exec --user 0）。
//!
//! 纯 Rust 实现，**零容器内命令依赖**（无 sh/useradd/sed/awk/getent/chown）：
//! 二进制由宿主 bind-mount 进容器，与 server 同前提——容器能跑 server
//! 就能跑 easytidy-dock，不依赖镜像里是否存在任何特定工具。
//!
//! 内容（全部幂等）：
//! - fontconfig 宿主字体接入（写 `/etc/fonts/local.conf`）
//! - 建号（行式读写 `/etc/passwd`、`/etc/group`——取代旧脚本的
//!   debian/busybox 双分支 useradd/adduser）
//! - 家目录补齐（mkdir + chown uid:gid + chmod 750）
//!
//! 分层（plan/apply）：
//! - [`resolve_identity`] / [`plan_prepare`] 纯函数——**身份语义单一事实源**
//!   （server 身份自发现与 dock ensure-home 共用，同一 uid/passwd 必然
//!   同结果）；宿主侧可单测
//! - [`apply_plan`] / [`prepare_in_container`] IO——容器内 root 执行

use std::ffi::CString;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

// 容器内固定路径（仅 [`prepare_in_container`] 使用；下层函数一律参数化
// 接收路径，单测可指到临时目录）
const PASSWD: &str = "/etc/passwd";
const GROUP: &str = "/etc/group";
const HOST_FONTS: &str = "/mnt/host";
const FONTCONF_DIR: &str = "/etc/fonts";

const FONTCONF_XML: &str = "<?xml version=\"1.0\"?>\n\
<!DOCTYPE fontconfig SYSTEM \"fonts.dtd\">\n\
<fontconfig>\n\
  <dir>/mnt/host/fonts</dir>\n\
  <dir>/mnt/host/.local/share/fonts</dir>\n\
</fontconfig>\n";

/// ets 命令软链（prepare 幂等维护）：目标 = 宿主 bind-mount 的容器内二进制，
/// 链 = /usr/local/bin/ets（PATH 上可直接敲 ets）。
const ETS_BIN_TARGET: &str = "/run/easytidy-bin/ets";
const ETS_LINK: &str = "/usr/local/bin/ets";

// ── 身份（单一事实源：server 身份自发现 + dock ensure-home 共用）────────────

/// 容器默认用户身份。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub home: String,
}

/// 由输入构造身份（纯函数，单测点）。
///
/// 名字优先级：配置用户名 → passwd 反查 → `uid<uid>`。
///
/// HOME 优先级（与名字同源，2026-08-28 定案）：
/// 1. 配置用户名 → `/home/<name>`（dock 建号/ensure-home 建目录，不依赖
///    建号执行时序）
/// 2. 未配置 + passwd 条目 home **有效**（非 `/`）→ 该条目 home（容器
///    默认用户自身家目录，如 ubuntu 的 /home/ubuntu）
/// 3. 未配置 + 无条目 **或占位条目**（home=`/`：运行时数字占位、keep-id
///    宿主用户名占位、系统用户）→ `uid<uid>` + `/home/uid<uid>`
///    （ensure-home 创建并 chown 到 uid:gid）
///
/// home=`/` 作「无可用 home」哨兵——占位条目的 home 字段恒为 `/`，
/// 据此统一识别，不依赖条目名形态。
pub fn resolve_identity(passwd: &str, uid: u32, gid: u32, user_name: Option<&str>) -> Identity {
    let user_name = user_name.filter(|n| !n.is_empty());
    let entry = parse_passwd(passwd).into_iter().find(|e| e.uid == uid);

    let (name, home) = match user_name {
        Some(n) => (n.to_string(), format!("/home/{n}")),
        None => match entry {
            // 真实用户条目（home 有效）→ 跟随容器默认用户
            Some(e) if !e.home.is_empty() && e.home != "/" => (e.name, e.home),
            // 无条目或占位（home=/）→ uid<uid> 命名
            _ => (format!("uid{uid}"), format!("/home/uid{uid}")),
        },
    };
    Identity {
        name,
        uid,
        gid,
        home,
    }
}

// ── 容器运行时 env 探测（纯函数，server 启动期用）────────────────────────
//
// 容器内 server（= 容器默认用户）启动时的 env 适配纯逻辑——与 [`resolve_identity`]
// 同属「容器侧身份/env 单一事实源」。纯函数（无进程副作用），server 据此做
// `std::env::set_var` 等副作用薄壳。从 `server/src/setup.rs` 下沉（2026-09-01
// env 模块族收敛）：探测逻辑归 core，进程级 set_var / static 留 server。

/// 当前进程 uid/gid（读 `/proc/self/status`，零依赖）。
///
/// server 即容器默认用户——容器 `User` 字段直指配置 uid:gid（宿主 create_with_config
/// 设），server 无需建号/降权，自身 uid/gid 即身份。
pub fn self_uid_gid() -> (u32, u32) {
    let status = fs::read_to_string("/proc/self/status").unwrap_or_default();
    let mut uid = 0u32;
    let mut gid = 0u32;
    for line in status.lines() {
        if let Some(v) = line.strip_prefix("Uid:") {
            uid = v
                .split_whitespace()
                .nth(1)
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
        } else if let Some(v) = line.strip_prefix("Gid:") {
            gid = v
                .split_whitespace()
                .nth(1)
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
        }
    }
    (uid, gid)
}

/// 修正 XDG_DATA_DIRS 值，确保包含系统默认数据目录（纯字符串变换）。
///
/// 背景：旧版 flavor 注入 `XDG_DATA_DIRS=/usr/share/easytidy-host`（纯覆盖），
/// gdk-pixbuf 2.42 经 `$XDG_DATA_DIRS/gdk-pixbuf-2.0/2.10.0/loaders.cache` 查
/// loader 注册表，覆盖后系统 cache 不可达 → 容器内 PNG 图标解码失败 → GTK 文件
/// 选择器断言崩溃（实测 Chrome 保存图片）。追加 glib 默认的
/// `/usr/local/share:/usr/share`（容器内缺失路径无害）。
pub fn fixup_xdg_data_dirs_value(v: &str) -> String {
    let mut merged = v.to_string();
    for p in ["/usr/local/share", "/usr/share"] {
        if !merged.split(':').any(|c| c == p) {
            if !merged.is_empty() {
                merged.push(':');
            }
            merged.push_str(p);
        }
    }
    merged
}

/// 在 `$XDG_RUNTIME_DIR` 下探测 X11 auth 文件（纯探测，**不做 set_var**）。
///
/// 路径含随机后缀（compositor 登录会话随机生成），不可由用户配置——故运行时
/// 探测。探针模式（取第一个匹配，剥掉可能的首个 `.` 前缀后再判，兼容有无点
/// 前缀两种形态）：
///   1. `[.]mutter-Xwaylandauth.*`（GNOME/Mutter 启动 Xwayland 生成，**实际点前缀**）
///   2. `[.]xauth_*`（Xorg 原生或老会话）
///
/// 取不到 → `None`（非 GUI 容器 / 未挂 XDG_RUNTIME_DIR 时正常）。server 据返回值
/// 决定 `set_var("XAUTHORITY", ...)` 与记录。
pub fn probe_xauthority(runtime_dir: &Path) -> Option<String> {
    let Ok(read_dir) = fs::read_dir(runtime_dir) else {
        return None;
    };
    for entry in read_dir.flatten() {
        let name = entry.file_name();
        let Some(n) = name.to_str() else { continue };
        let stripped = n.strip_prefix('.').unwrap_or(n);
        if stripped.starts_with("mutter-Xwaylandauth.") || stripped.starts_with("xauth_") {
            return Some(format!("{}/{}", runtime_dir.display(), n));
        }
    }
    None
}

// ── /etc/passwd、/etc/group 行式解析（纯）──────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PasswdLine {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub home: String,
}

/// 解析 /etc/passwd（`name:password:uid:gid:gecos:home:shell`；
/// 字段不足的行跳过——malformed 容错）。
pub(crate) fn parse_passwd(passwd: &str) -> Vec<PasswdLine> {
    passwd
        .lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.splitn(7, ':').collect();
            if f.len() < 6 {
                return None;
            }
            let uid = f[2].parse().ok()?;
            let gid = f[3].parse().ok()?;
            Some(PasswdLine {
                name: f[0].to_string(),
                uid,
                gid,
                home: f[5].to_string(),
            })
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GroupLine {
    pub name: String,
    pub gid: u32,
}

/// 解析 /etc/group（`name:password:gid:members`；字段不足的行跳过）。
pub(crate) fn parse_group(group: &str) -> Vec<GroupLine> {
    group
        .lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.splitn(4, ':').collect();
            if f.len() < 3 {
                return None;
            }
            let gid = f[2].parse().ok()?;
            Some(GroupLine {
                name: f[0].to_string(),
                gid,
            })
        })
        .collect()
}

// ── 准备计划（纯数据，单测点）──────────────────────────────────────────────

/// 准备输入。
#[derive(Debug, Clone)]
pub struct PrepareSpec {
    pub uid: u32,
    pub gid: u32,
    /// 配置用户名（None = 不建号，仅 fontconfig + ensure-home）
    pub user_name: Option<String>,
    /// 登录 shell（调用方容器内探测：/bin/bash 存在优先，否则 /bin/sh）
    pub shell: String,
}

/// 准备计划：把「输入 + 现状（passwd/group 内容）」变成一组明确的文件
/// 变更 + ensure-home 目标。apply 只消费计划，不再做决策。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreparePlan {
    pub uid: u32,
    pub gid: u32,
    /// 删除的 /etc/passwd 条目名（运行时占位条目）
    pub remove_passwd: Vec<String>,
    /// 删除的 /etc/group 条目名（占位同名组）
    pub remove_group: Vec<String>,
    /// 新增 /etc/passwd 行（None = 同名已存在或 uid 被他人占用不建号）
    pub add_passwd: Option<String>,
    /// 新增 /etc/group 行（None = 该 gid 已有组或跳过建号）
    pub add_group: Option<String>,
    /// ensure-home 目标目录（恒执行：缺失则创建 + chown uid:gid + chmod 750）
    pub home: String,
    /// 建号跳过原因（uid 被他人占用等；easytidy-dock 输出 stderr 提示）
    pub skip_reason: Option<String>,
}

/// 纯函数：由输入与现状计算准备计划。
///
/// 占位条目判定（2026-08-27/28 实测两种形态，home 字段恒为 `/`）：
/// - 数字型：容器以数字 `<uid>:<gid>` 启动 → `<uid>:*:<uid>:<gid>:container user:/:/bin/sh`
/// - keep-id 型：容器以 keep-id 启动 → 条目名是宿主用户名（`div:*:1000:1000:div:/:/bin/sh`）
///
/// 占位必须先清除：否则 uid 保护分支把占位误判为「他人占用」跳过建号。
pub fn plan_prepare(passwd: &str, group: &str, spec: &PrepareSpec) -> PreparePlan {
    let identity = resolve_identity(passwd, spec.uid, spec.gid, spec.user_name.as_deref());
    let mut plan = PreparePlan {
        uid: spec.uid,
        gid: spec.gid,
        home: identity.home,
        ..Default::default()
    };
    // 恒保证 uid 有 passwd 条目（未配置 user_name 时用 identity.name 兜底：
    // 镜像真实条目跟随其名，无条目/占位条目 → `uid<uid>`）。
    // VS Code Dev Containers attach 靠 getent 解析容器用户 home；条目缺失
    // 时 getent 落空 → Server 回退装到 `/` 触发权限错误（2026-09-12 实测）。
    let name = identity.name;
    let entries = parse_passwd(passwd);

    // 幂等：未配置用户名且该 uid 已有真实条目（home 有效）→ 跟随条目，不动
    // （配置了 user_name 时要继续走 uid 保护给出 skip_reason，语义不变）
    if spec.user_name.is_none()
        && entries
            .iter()
            .any(|e| e.uid == spec.uid && e.home != "/" && !e.home.is_empty())
    {
        return plan;
    }

    // 占位条目（该 uid 且 home=/）先清除；占位同名组一并清
    // （否则组文件里占位组占着 gid）
    if let Some(ph) = entries.iter().find(|e| e.uid == spec.uid && e.home == "/") {
        plan.remove_passwd.push(ph.name.clone());
        plan.remove_group.push(ph.name.clone());
    }

    // uid 保护：uid 被其他真实条目占用（home 有效）→ 不覆盖
    if entries.iter().any(|e| e.uid == spec.uid && e.home != "/") {
        plan.skip_reason = Some(format!(
            "warning: uid {} already owned by another user; skipping user creation",
            spec.uid
        ));
        return plan;
    }

    plan.add_passwd = Some(format!(
        "{name}:x:{}:{}:{}:{}:{}",
        spec.uid, spec.gid, name, plan.home, spec.shell
    ));
    // 组：仅当该 gid 无**幸存**组条目时创建（占位组将被删除 → 视为空）
    let groups = parse_group(group);
    let gid_free = !groups
        .iter()
        .filter(|g| !plan.remove_group.contains(&g.name))
        .any(|g| g.gid == spec.gid);
    if gid_free {
        plan.add_group = Some(format!("{name}:x:{gid}:", gid = spec.gid));
    }
    plan
}

// ── 应用计划（IO：容器内 root）──────────────────────────────────────────────

/// 容器内路径集（参数化，单测指临时目录）。
#[derive(Debug, Clone)]
pub struct IncontainerPaths {
    pub passwd: PathBuf,
    pub group: PathBuf,
    /// 宿主字体/图标挂载根（/mnt/host）
    pub host_fonts: PathBuf,
    /// fontconfig 目录（/etc/fonts；写其下 local.conf）
    pub fontconf_dir: PathBuf,
}

/// 准备结果报告（easytidy-dock 据此输出提示）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrepareReport {
    /// 是否新增 passwd 条目（false = 同名已存在或被跳过）
    pub user_created: bool,
    /// 建号跳过提示（如 uid 被他人占用）
    pub skip_reason: Option<String>,
}

/// 应用计划：改写 /etc/passwd、/etc/group（临时文件 + rename），
/// 补齐家目录，接入 fontconfig。全部幂等。
pub fn apply_plan(plan: &PreparePlan, paths: &IncontainerPaths) -> Result<()> {
    if !plan.remove_passwd.is_empty() || plan.add_passwd.is_some() {
        rewrite_file(
            &paths.passwd,
            &plan.remove_passwd,
            plan.add_passwd.as_deref(),
        )?;
    }
    if !plan.remove_group.is_empty() || plan.add_group.is_some() {
        rewrite_file(&paths.group, &plan.remove_group, plan.add_group.as_deref())?;
    }
    ensure_home(&plan.home, plan.uid, plan.gid)?;
    write_fontconfig(&paths.host_fonts, &paths.fontconf_dir)?;
    Ok(())
}

/// 容器内准备入口（easytidy-dock `prepare` 子命令；容器内 root 执行）。
///
/// 读 /etc/passwd + /etc/group → [`plan_prepare`]（纯）→ [`apply_plan`]（IO）。
/// 幂等，重复调用无害。
pub fn prepare_in_container(
    uid: u32,
    gid: u32,
    user_name: Option<&str>,
    first_run: bool,
) -> Result<PrepareReport> {
    // 登录 shell 容器内探测（/bin/bash 存在优先；dock 本身不依赖任何 shell）
    let shell = if Path::new("/bin/bash").exists() {
        "/bin/bash"
    } else {
        "/bin/sh"
    }
    .to_string();
    let spec = PrepareSpec {
        uid,
        gid,
        user_name: user_name.filter(|n| !n.is_empty()).map(str::to_string),
        shell,
    };
    let passwd = fs::read_to_string(PASSWD).unwrap_or_default();
    let group = fs::read_to_string(GROUP).unwrap_or_default();
    let plan = plan_prepare(&passwd, &group, &spec);
    let paths = IncontainerPaths {
        passwd: PathBuf::from(PASSWD),
        group: PathBuf::from(GROUP),
        host_fonts: PathBuf::from(HOST_FONTS),
        fontconf_dir: PathBuf::from(FONTCONF_DIR),
    };
    apply_plan(&plan, &paths)?;
    // 首次创建专属：家目录内 root 属主条目归位（pull 压平修复）。纯净镜像
    // rootless pull 时 blob 属主统一压平为 puller，用户家目录里的骨架文件
    // （.bashrc 等，blob 属 ubuntu）落成 root——仅首次创建执行一次归位，
    // 重建/重启（first_run=false）不重复执行。
    if first_run {
        // 已知属主集合 = passwd 账号 uid，**剔除 root(0)**：家目录里 root 属主
        // 的条目没有合法来源（pull 属主压平：blob root 与 blob 用户一起被压平
        // 后由 create 翻译成 c0），与悬空属主（如旧映射代次的 999，passwd 无
        // 对应账号）一样归位到容器用户。
        let mut known: std::collections::HashSet<u32> = passwd
            .lines()
            .filter_map(|l| l.split(':').nth(2))
            .filter_map(|u| u.parse::<u32>().ok())
            .collect();
        known.remove(&0);
        repair_home_dangling_ownership(Path::new(&plan.home), plan.uid, plan.gid, &known);
    }
    // ets 命令软链（幂等；宿主未挂载 ets 时静默跳过）
    ensure_ets_symlink();
    // GNOME 窗口按钮（幂等；环境不满足时静默跳过）
    ensure_button_layout(&plan.home, uid, gid);
    Ok(PrepareReport {
        user_created: plan.add_passwd.is_some(),
        skip_reason: plan.skip_reason.clone(),
    })
}

/// 建/刷新 `/usr/local/bin/ets` → `/run/easytidy-bin/ets` 软链（幂等）。
///
/// 目标不存在（宿主未装 ets / 旧容器未挂载）→ 跳过；失败仅 warn（不阻断
/// prepare——ets 是增强项，非核心路径）。以 root 执行（dock prepare 身份）。
fn ensure_ets_symlink() {
    use std::os::unix::fs::symlink;
    if !Path::new(ETS_BIN_TARGET).exists() {
        return;
    }
    let _ = fs::remove_file(ETS_LINK); // 清旧链/旧文件（不存在则忽略）
    if let Err(e) = symlink(ETS_BIN_TARGET, ETS_LINK) {
        tracing::warn!("创建 {ETS_LINK} 软链失败：{e}");
    }
}

/// 保证 GNOME 窗口三按钮（最小化/最大化/关闭）。
///
/// GNOME 上游默认 `button-layout = ":close"`（只有关闭按钮）；libdecor-gtk 等
/// GTK 标题栏读取同一 GSettings 键，于是容器内 GUI 直通应用"没有最小化/最大化
/// 按钮"（2026-09-12 labwc/libdecor 嵌套场景实测，详见
/// container-gui-docs/README.md）。
///
/// 修复：以目标用户身份起临时 dbus 会话写一次 GSettings 键（幂等，落
/// `~/.config/dconf/user`）。约束：
/// - 需要 `dbus-run-session` + `gsettings` + dconf 后端（debian 系桌面镜像
///   自带；alpine 等精简镜像缺任一 → 静默跳过，GUI 功能不受影响）；
/// - 失败仅 warn，不阻断 prepare。
fn ensure_button_layout(home: &str, uid: u32, gid: u32) {
    use std::os::unix::process::CommandExt;
    use std::process::Command;

    const KEY: &str = "org.gnome.desktop.wm.preferences";
    const LAYOUT: &str = ":minimize,maximize,close";
    for bin in ["/usr/bin/dbus-run-session", "/usr/bin/gsettings"] {
        if !Path::new(bin).exists() {
            tracing::debug!("prepare: {bin} 不存在，跳过 button-layout 配置");
            return;
        }
    }
    // dconf 库落 ~/.config/dconf，HOME 必须指向目标用户家目录；dbus 会话
    // socket 需要 XDG_RUNTIME_DIR（不存在则退 /tmp）
    let runtime_dir = if Path::new("/run/user").join(uid.to_string()).exists() {
        format!("/run/user/{uid}")
    } else {
        "/tmp".to_string()
    };
    // 根治 exec 会话挂起：dbus-run-session 的孙子进程（dbus-daemon/dconf）
    // 若继承任何会话管道，进程退出后输出流迟迟不 EOF，exec_oneshot 会等几分钟。
    // 因此 stdio 全部接 null（不引用 dock exec 会话管道），错误详情不再逐字
    // 捕获——失败仅以退出码告警（button-layout 是增强项，失败不影响其他功能）。
    let mut run = Command::new("/usr/bin/dbus-run-session");
    run.arg("--")
        .arg("/usr/bin/gsettings")
        .arg("set")
        .arg(KEY)
        .arg("button-layout")
        .arg(LAYOUT)
        .env("HOME", home)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .uid(uid)
        .gid(gid)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    match run.status() {
        Ok(st) if st.success() => {
            tracing::info!("prepare: button-layout = {LAYOUT}（uid {uid}）");
        }
        Ok(st) => tracing::warn!(
            "prepare: 写 button-layout 失败（GUI 无最小化/最大化按钮，不影响其他功能）：退出码 {:?}",
            st.code()
        ),
        Err(e) => tracing::warn!("prepare: 启动 dbus-run-session 失败：{e}"),
    }
}

// ── IO 细节 ─────────────────────────────────────────────────────────────────

/// 按行改写 passwd/group：删除指定条目名（首字段匹配）的行 + 追加新行。
fn rewrite_file(path: &Path, remove_names: &[String], add_line: Option<&str>) -> Result<()> {
    let original = fs::read_to_string(path).unwrap_or_default();
    let mut out = Vec::new();
    for line in original.lines() {
        let name = line.split(':').next().unwrap_or_default();
        if remove_names.iter().any(|n| n == name) {
            continue;
        }
        out.push(line.to_string());
    }
    if let Some(l) = add_line {
        out.push(l.to_string());
    }
    let mut content = out.join("\n");
    content.push('\n');
    atomic_write(path, &content)
}

/// 临时文件 + rename 写入（避免中途崩溃留下半截 /etc/passwd）。
fn atomic_write(path: &Path, content: &str) -> Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("etc-file");
    let tmp = dir.join(format!(".{file_name}.easytidy-tmp"));
    fs::write(&tmp, content)?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o644))?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// 家目录补齐：缺失则创建，属主/权限纠正为 uid:gid / 0750（覆盖
/// 「镜像自带目录属主 root」形态）。
fn ensure_home(home: &str, uid: u32, gid: u32) -> Result<()> {
    // 兜底：容器根不得作家目录（resolve_identity 已保证不产生，
    // 双保险防把 / 整个 chown 掉）
    if home.is_empty() || home == "/" {
        return Err(Error::Config(format!(
            "非法家目录（拒绝 chown 容器根）：{home:?}"
        )));
    }
    fs::create_dir_all(home)?;
    let cpath = CString::new(home)
        .map_err(|e| Error::Io(std::io::Error::new(std::io::ErrorKind::InvalidInput, e)))?;
    chown(&cpath, uid, gid)?;
    fs::set_permissions(home, fs::Permissions::from_mode(0o750))?;
    Ok(())
}

fn chown(path: &CString, uid: u32, gid: u32) -> Result<()> {
    let rc = unsafe { libc::chown(path.as_ptr(), uid as libc::uid_t, gid as libc::gid_t) };
    if rc != 0 {
        return Err(Error::Io(std::io::Error::last_os_error()));
    }
    Ok(())
}

/// 家目录悬空属主归位（首次创建专属）。
///
/// 递归扫描 `home` 子树，把属主不在 `known_uids`（容器 passwd 全部账号）
/// 中的文件/目录 chown 为 uid:gid。两类来源：
/// - rootless pull 属主压平：纯净镜像（骨架文件 blob 属 ubuntu(1000)）解包
///   后统一压平为 puller，create 翻译后显示为 root；
/// - 跨映射代次化石：旧映射代次落盘的编码（如 999）在新映射下视图错位，
///   元数据层面不可翻译（bijection 恒等），只能按语义归位。
///
/// 判定可证明：属主不是容器 passwd 任何账号的文件，在用户家目录里没有
/// 合法来源（合法属主必然对应某个容器账号）。范围严格限定在用户自己的
/// home 子树（非 /home 全局）、跳过挂载点（宿主机 bind 内容不可触碰）与
/// 符号链接；仅在首次创建（first_run）执行一次。失败逐条容错。
fn repair_home_dangling_ownership(
    home: &Path,
    uid: u32,
    gid: u32,
    known_uids: &std::collections::HashSet<u32>,
) {
    let mut fixed = 0usize;
    let mounts = mount_points_under(home);
    if !mounts.is_empty() {
        tracing::debug!(
            "prepare: 家目录归位排除 {0} 个挂载点（宿主机映射内容不可触碰）",
            mounts.len()
        );
    }
    repair_home_dangling_ownership_walk(home, uid, gid, known_uids, &mounts, &mut fixed);
    tracing::debug!("prepare: 家目录悬空属主归位完成（home={home:?}，修正 {fixed} 项）");
}

/// 解析 /proc/self/mountinfo，返回落在 `home` 子树内的挂载点集合。
///
/// 宿主机 bind-mount（如 `${HOME}/spark_notes`、`${HOME}/media`）的文件属主
/// 是宿主机语义，归位绝不可触碰；walk 遇到这些路径整棵跳过（不下行）。
fn mount_points_under(home: &Path) -> std::collections::HashSet<PathBuf> {
    match fs::read_to_string("/proc/self/mountinfo") {
        Ok(raw) => mount_points_under_raw(home, &raw),
        Err(_) => std::collections::HashSet::new(),
    }
}

/// 纯函数（便于测试）：从 mountinfo 文本解析落在 `home` 子树内的挂载点。
fn mount_points_under_raw(home: &Path, raw: &str) -> std::collections::HashSet<PathBuf> {
    let mut out = std::collections::HashSet::new();
    for line in raw.lines() {
        // mountinfo 字段：36ab 814 259:8 /root-of-mount /mount-point options ...
        let fields: Vec<&str> = line.split_whitespace().collect();
        let Some(fields) = fields.get(..5) else {
            continue;
        };
        // 挂载点含空格/特殊字符时以八进制转义（\040 等），还原常见空格
        let mp = fields[4].replace(r"\040", " ");
        let mp = PathBuf::from(mp);
        if mp.starts_with(home) {
            out.insert(mp);
        }
    }
    out
}

fn repair_home_dangling_ownership_walk(
    dir: &Path,
    uid: u32,
    gid: u32,
    known_uids: &std::collections::HashSet<u32>,
    mounts: &std::collections::HashSet<PathBuf>,
    fixed: &mut usize,
) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!("prepare: 家目录归位跳过 {dir:?}（读取失败：{e}）");
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // 宿主机 bind-mount：属主是宿主机语义，整棵跳过（不下行、不改属主）
        if mounts.contains(&path) {
            continue;
        }
        // 不跟随符号链接：链接本身属主不重要，目标更不能动
        let md = match entry.metadata() {
            Ok(m) if m.is_symlink() => continue,
            Ok(m) => m,
            Err(_) => continue,
        };
        // 悬空属主 = owner 不是容器 passwd 任何账号 → 逻辑上必是化石
        // （合法文件属主必然对应某个容器账号），归位到容器用户
        let dangling = !known_uids.contains(&md.uid());
        if md.is_dir() {
            if dangling {
                chown_path(&path, uid, gid);
                *fixed += 1;
            }
            repair_home_dangling_ownership_walk(&path, uid, gid, known_uids, mounts, fixed);
        } else if dangling {
            chown_path(&path, uid, gid);
            *fixed += 1;
        }
    }
}

fn chown_path(path: &Path, uid: u32, gid: u32) {
    match CString::new(path.as_os_str().to_string_lossy().as_bytes()) {
        Ok(c) => {
            if let Err(e) = chown(&c, uid, gid) {
                tracing::warn!("prepare: 归位失败 {path:?}：{e}");
            }
        }
        Err(e) => tracing::warn!("prepare: 归位路径非法 {path:?}：{e}"),
    }
}

/// fontconfig 宿主字体接入（幂等覆写）。
///
/// flavor `gui=true` 把宿主字体/图标只读挂到 `/mnt/host/`；
/// 无挂载或无 fontconfig 目录（非 GUI 容器）时静默跳过。
fn write_fontconfig(host_fonts: &Path, fontconf_dir: &Path) -> Result<()> {
    if !host_fonts.is_dir() || !fontconf_dir.is_dir() {
        return Ok(());
    }
    atomic_write(&fontconf_dir.join("local.conf"), FONTCONF_XML)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWD: &str = "root:x:0:0:root:/root:/bin/sh\n\
                      tidy:x:1000:1000::/home/tidy:/bin/bash\n\
                      other:x:1001:1001::/home/other:/bin/sh\n";

    // ── 容器运行时 env 探测（server 启动期纯函数）──

    #[test]
    fn test_self_uid_gid_matches_libc() {
        // 真实进程身份：与 libc getuid/getgid 一致（Linux 容器环境）
        let (uid, gid) = self_uid_gid();
        assert_eq!(uid, unsafe { libc::getuid() });
        assert_eq!(gid, unsafe { libc::getgid() });
    }

    #[test]
    fn test_fixup_xdg_data_dirs_appends_system_defaults() {
        // 缺系统默认 → 追加
        assert_eq!(
            fixup_xdg_data_dirs_value("/usr/share/easytidy-host"),
            "/usr/share/easytidy-host:/usr/local/share:/usr/share"
        );
        // 已含系统默认 → 原样（幂等）
        assert_eq!(
            fixup_xdg_data_dirs_value("/mnt/host:/usr/local/share:/usr/share"),
            "/mnt/host:/usr/local/share:/usr/share"
        );
        // 空值 → 仅系统默认
        assert_eq!(fixup_xdg_data_dirs_value(""), "/usr/local/share:/usr/share");
    }

    fn write_auth(dir: &Path, name: &str) {
        fs::write(dir.join(name), b"mock-cookie").unwrap();
    }

    #[test]
    fn test_probe_xauthority_mutter() {
        let tmp = tempfile::tempdir().unwrap();
        write_auth(tmp.path(), "mutter-Xwaylandauth.hzQT2z");
        let p = probe_xauthority(tmp.path()).unwrap();
        assert!(p.ends_with("mutter-Xwaylandauth.hzQT2z"));
    }

    #[test]
    fn test_probe_xauthority_dot_prefixed_mutter() {
        // GNOME/Mutter 实际点前缀（.mutter-Xwaylandauth.<rand>）必须被探测到
        let tmp = tempfile::tempdir().unwrap();
        write_auth(tmp.path(), ".mutter-Xwaylandauth.DE23U3");
        let p = probe_xauthority(tmp.path()).unwrap();
        assert!(p.ends_with(".mutter-Xwaylandauth.DE23U3"));
    }

    #[test]
    fn test_probe_xauthority_xauth_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        write_auth(tmp.path(), "xauth_abc123");
        assert!(probe_xauthority(tmp.path())
            .unwrap()
            .ends_with("xauth_abc123"));
    }

    #[test]
    fn test_probe_xauthority_none_when_empty() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(probe_xauthority(tmp.path()), None);
    }

    // ── resolve_identity（server 身份自发现的同一事实源）──

    #[test]
    fn test_identity_named_user() {
        let id = resolve_identity(PASSWD, 1000, 1000, Some("tidy"));
        assert_eq!(id.name, "tidy");
        assert_eq!(id.home, "/home/tidy");
        // 建号尚未执行（无 passwd 条目）也成立
        let id = resolve_identity("", 1000, 1000, Some("tidy"));
        assert_eq!(id.name, "tidy");
        assert_eq!(id.home, "/home/tidy");
    }

    #[test]
    fn test_identity_passwd_lookup() {
        // 未配置 + passwd 有条目 → 名字与 home 都跟随容器默认用户
        let id = resolve_identity(PASSWD, 1000, 1000, None);
        assert_eq!(id.name, "tidy");
        assert_eq!(id.home, "/home/tidy", "HOME = passwd 条目 home");
    }

    #[test]
    fn test_identity_no_passwd_entry() {
        // 未配置 + 无条目（镜像未预置该 uid，如 alpine）→ /home/uid<uid>
        let id = resolve_identity(PASSWD, 2000, 2000, None);
        assert_eq!(id.name, "uid2000");
        assert_eq!(id.home, "/home/uid2000");
    }

    #[test]
    fn test_identity_placeholder_entry() {
        // 占位条目（home=/ 哨兵）：数字型与 keep-id 型都不得反查为身份
        let numeric = "1000:*:1000:1000:container user:/:/bin/sh\n";
        let id = resolve_identity(numeric, 1000, 1000, None);
        assert_eq!(id.name, "uid1000");
        assert_eq!(id.home, "/home/uid1000");
        let keep_id = "div:*:1000:1000:div:/:/bin/sh\n";
        let id = resolve_identity(keep_id, 1000, 1000, None);
        assert_eq!(id.name, "uid1000");
        assert_eq!(id.home, "/home/uid1000");
        // 真实用户（home 有效）→ 跟随
        let id = resolve_identity(
            "ubuntu:x:1000:1000:Ubuntu:/home/ubuntu:/bin/bash\n",
            1000,
            1000,
            None,
        );
        assert_eq!(id.name, "ubuntu");
        assert_eq!(id.home, "/home/ubuntu");
    }

    #[test]
    fn test_identity_empty_env_ignored() {
        let id = resolve_identity(PASSWD, 1000, 1000, Some(""));
        assert_eq!(id.name, "tidy");
        assert_eq!(id.home, "/home/tidy");
    }

    // ── plan_prepare ──

    fn spec(uid: u32, gid: u32, name: Option<&str>) -> PrepareSpec {
        PrepareSpec {
            uid,
            gid,
            user_name: name.map(str::to_string),
            shell: "/bin/bash".to_string(),
        }
    }

    #[test]
    fn test_plan_named_user_fresh() {
        let plan = plan_prepare("", "", &spec(1011, 1011, Some("tidy")));
        assert_eq!(
            plan.add_passwd.as_deref(),
            Some("tidy:x:1011:1011:tidy:/home/tidy:/bin/bash")
        );
        assert_eq!(plan.add_group.as_deref(), Some("tidy:x:1011:"));
        assert_eq!(plan.home, "/home/tidy");
        assert!(plan.skip_reason.is_none());
    }

    #[test]
    fn test_plan_idempotent_name_exists() {
        let plan = plan_prepare(PASSWD, "", &spec(1000, 1000, Some("tidy")));
        assert!(plan.add_passwd.is_none(), "同名已存在 → 不建号");
        assert!(plan.add_group.is_none());
        assert!(plan.remove_passwd.is_empty());
    }

    #[test]
    fn test_plan_removes_numeric_placeholder() {
        // 数字型占位（容器以数字 uid:gid 启动时运行时写入）
        let passwd = "1000:*:1000:1000:container user:/:/bin/sh\n";
        let group = "1000:x:1000:1000\n";
        let plan = plan_prepare(passwd, group, &spec(1000, 1000, Some("tidy")));
        assert_eq!(plan.remove_passwd, vec!["1000".to_string()]);
        assert_eq!(plan.remove_group, vec!["1000".to_string()]);
        assert!(plan.add_passwd.is_some(), "占位清除后应建号");
        assert!(plan.add_group.is_some(), "占位组清除后 gid 可用");
    }

    #[test]
    fn test_plan_removes_keepid_placeholder() {
        // keep-id 型占位（条目名 = 宿主用户名，非数字）
        let passwd = "div:*:1000:1000:div:/:/bin/sh\n";
        let group = "div:x:1000:div\n";
        let plan = plan_prepare(passwd, group, &spec(1000, 1000, Some("tidy")));
        assert_eq!(plan.remove_passwd, vec!["div".to_string()]);
        assert_eq!(plan.remove_group, vec!["div".to_string()]);
        assert!(plan.add_passwd.is_some());
    }

    #[test]
    fn test_plan_uid_owned_by_other() {
        // uid 被真实用户占用 → 不覆盖，仅提示
        let plan = plan_prepare(PASSWD, "", &spec(1001, 1001, Some("newuser")));
        assert!(plan.add_passwd.is_none());
        assert!(plan.add_group.is_none());
        assert!(plan.skip_reason.as_deref().unwrap().contains("1001"));
        // ensure-home 仍执行：配置用户名时 home 恒为 /home/<name>
        // （与 server 侧 resolve_identity 同一事实源，语义一致）
        assert_eq!(plan.home, "/home/newuser");
    }

    #[test]
    fn test_plan_gid_occupied_by_real_group() {
        // 该 gid 已有真实组（非占位）→ 不建组，passwd 直接用该 gid
        let group = "video:x:1011:\n";
        let plan = plan_prepare("", group, &spec(1011, 1011, Some("tidy")));
        assert!(plan.add_passwd.is_some());
        assert!(plan.add_group.is_none());
    }

    #[test]
    fn test_plan_no_user_name_only_home() {
        // 未配置用户名 + 镜像已有真实条目 → 跟随条目，不建号
        let plan = plan_prepare(PASSWD, "", &spec(1000, 1000, None));
        assert!(plan.add_passwd.is_none());
        assert!(plan.add_group.is_none());
        assert!(plan.remove_passwd.is_empty());
        assert_eq!(plan.home, "/home/tidy");
        // 未配置用户名 + 无条目 → 恒建号（uid<uid> 兜底命名；VS Code attach
        // 的 getent 靠 passwd 条目解析 home，缺失则 Server 回退装到 / 报权限错）
        let plan = plan_prepare("", "", &spec(2000, 2000, None));
        assert_eq!(plan.home, "/home/uid2000");
        let added = plan.add_passwd.as_deref().unwrap();
        assert!(added.starts_with("uid2000:x:2000:2000:uid2000:/home/uid2000:"));
        assert!(plan.add_group.is_some(), "gid 空闲时应建组");
    }

    // ── IO 细节（tempdir 模拟 /etc；chown 用测试自身 uid/gid——
    //    非 root 下 chown 他人会 EPERM）──

    fn tmp_env() -> (tempfile::TempDir, IncontainerPaths) {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = IncontainerPaths {
            passwd: tmp.path().join("passwd"),
            group: tmp.path().join("group"),
            host_fonts: tmp.path().join("host"),
            fontconf_dir: tmp.path().join("fonts"),
        };
        (tmp, paths)
    }

    fn self_id() -> (u32, u32) {
        (unsafe { libc::geteuid() }, unsafe { libc::getegid() })
    }

    #[test]
    fn test_apply_rewrites_passwd_and_group() {
        let (tmp, paths) = tmp_env();
        fs::write(&paths.passwd, "root:x:0:0:root:/root:/bin/sh\n").unwrap();
        fs::write(&paths.group, "root:x:0:\n").unwrap();
        fs::create_dir_all(&paths.fontconf_dir).unwrap();

        let (uid, gid) = self_id();
        let mut plan = plan_prepare("", "", &spec(uid, gid, Some("tidy")));
        plan.home = tmp.path().join("home/tidy").to_string_lossy().to_string();
        apply_plan(&plan, &paths).unwrap();

        let passwd = fs::read_to_string(&paths.passwd).unwrap();
        assert!(passwd
            .lines()
            .any(|l| l.starts_with(&format!("tidy:x:{uid}:{gid}:tidy:"))));
        assert!(fs::read_to_string(&paths.group)
            .unwrap()
            .lines()
            .any(|l| l.starts_with(&format!("tidy:x:{gid}:"))));
    }

    #[test]
    fn test_apply_removes_placeholder_then_creates() {
        let (tmp, paths) = tmp_env();
        fs::write(&paths.passwd, "1000:*:1000:1000:container user:/:/bin/sh\n").unwrap();
        fs::write(&paths.group, "1000:x:1000:1000\n").unwrap();
        fs::create_dir_all(&paths.fontconf_dir).unwrap();

        let mut plan = plan_prepare(
            &fs::read_to_string(&paths.passwd).unwrap(),
            &fs::read_to_string(&paths.group).unwrap(),
            &spec(1000, 1000, Some("tidy")),
        );
        // chown 用自身身份（非 root 下 chown 1000 会 EPERM）
        plan.home = tmp.path().join("h").to_string_lossy().to_string();
        let (uid, gid) = self_id();
        plan.uid = uid;
        plan.gid = gid;
        apply_plan(&plan, &paths).unwrap();

        let passwd = fs::read_to_string(&paths.passwd).unwrap();
        assert!(!passwd.contains("container user"), "占位条目应被清除");
        assert!(passwd.lines().any(|l| l.starts_with("tidy:x:1000:1000:")));
        let group = fs::read_to_string(&paths.group).unwrap();
        assert!(!group.contains("1000:x:1000:1000"), "占位组应被清除");
        assert!(group.lines().any(|l| l.starts_with("tidy:x:1000:")));
    }

    /// 家目录 root 属主归位：foreign（root）属主条目被选中并 chown，
    /// 非 foreign 条目与符号链接不动。测试以当前 euid 充当 foreign uid
    /// （chown 回自身 = 无害成功），生产调用 foreign_uid 恒传 0。
    #[test]
    fn test_repair_home_dangling_ownership_scoped() {
        let tmp = tempfile::tempdir().unwrap();
        let euid = unsafe { libc::geteuid() };
        let egid = unsafe { libc::getegid() };
        let home = tmp.path();
        std::fs::write(home.join(".bashrc"), b"fossil").unwrap();
        std::fs::create_dir(home.join("sub")).unwrap();
        std::fs::write(home.join("sub").join("nested"), b"x").unwrap();
        std::os::unix::fs::symlink("/etc/hostname", home.join("link")).unwrap();

        // 模拟 bind-mount 挂在 home 内（如 ${HOME}/media）：整棵必须跳过
        let mut mounts = std::collections::HashSet::new();
        mounts.insert(home.join("sub"));

        // 已知属主集合不含测试文件属主 → 全部视为悬空；sub/ 为挂载点整棵
        // 跳过（含其内 nested）→ 仅 .bashrc 被选中
        let known = std::collections::HashSet::from([euid.wrapping_add(1)]);
        let mut fixed = 0usize;
        repair_home_dangling_ownership_walk(home, euid, egid, &known, &mounts, &mut fixed);
        assert_eq!(fixed, 1, "挂载点内条目不可触碰，仅 .bashrc 被归位");

        // 已知属主包含全部条目 → 无悬空，零改动
        let known_all = std::collections::HashSet::from([euid]);
        let mut fixed2 = 0usize;
        repair_home_dangling_ownership_walk(home, euid, egid, &known_all, &mounts, &mut fixed2);
        assert_eq!(fixed2, 0, "属主都在 passwd 账号集合内时不应有任何改动");
    }

    /// mount_points_under：mountinfo 解析 + home 子树过滤（含八进制转义还原）
    #[test]
    fn test_mount_points_under_filters_subtree() {
        let home = Path::new("/home/ubuntu");
        let raw = concat!(
            "36 814 259:8 /mnt/docs/media /home/ubuntu/media rw,relatime - ext4 nvme rw\n",
            "40 814 259:8 /other /somewhere/else rw - ext4 nvme rw\n",
            "42 814 259:8 /mnt/docs/mydocs /home/ubuntu/my\\040docs rw - ext4 nvme rw\n",
        );
        let got = mount_points_under_raw(home, raw);
        assert!(got.contains(&PathBuf::from("/home/ubuntu/media")));
        assert!(!got.contains(&PathBuf::from("/somewhere/else")));
        // 八进制 \040 还原为空格
        assert!(got.contains(&PathBuf::from("/home/ubuntu/my docs")));
        let _ = raw;
    }

    #[test]
    fn test_ensure_home_creates_with_mode() {
        let (tmp, _paths) = tmp_env();
        let home = tmp.path().join("deep/nested/home");
        let (uid, gid) = self_id();
        ensure_home(home.to_str().unwrap(), uid, gid).unwrap();
        assert!(home.is_dir());
        assert_eq!(
            fs::metadata(&home).unwrap().permissions().mode() & 0o777,
            0o750
        );
    }

    #[test]
    fn test_ensure_home_rejects_root() {
        let (uid, gid) = self_id();
        assert!(ensure_home("/", uid, gid).is_err(), "home=/ 必须被拒绝");
        assert!(ensure_home("", uid, gid).is_err());
    }
}
