//! 容器内 root 一次性准备（`easytidy-ctool` 二进制执行，宿主侧 exec --user 0）。
//!
//! 纯 Rust 实现，**零容器内命令依赖**（无 sh/useradd/sed/awk/getent/chown）：
//! 二进制由宿主 bind-mount 进容器，与 server 同前提——容器能跑 server
//! 就能跑 ctool，不依赖镜像里是否存在任何特定工具。
//!
//! 内容（全部幂等）：
//! - fontconfig 宿主字体接入（写 `/etc/fonts/local.conf`）
//! - 建号（行式读写 `/etc/passwd`、`/etc/group`——取代旧脚本的
//!   debian/busybox 双分支 useradd/adduser）
//! - 家目录补齐（mkdir + chown uid:gid + chmod 750）
//!
//! 分层（plan/apply）：
//! - [`resolve_identity`] / [`plan_prepare`] 纯函数——**身份语义单一事实源**
//!   （server 身份自发现与 ctool ensure-home 共用，同一 uid/passwd 必然
//!   同结果）；宿主侧可单测
//! - [`apply_plan`] / [`prepare_in_container`] IO——容器内 root 执行

use std::ffi::CString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

// 容器内固定路径（仅 [`prepare_in_container`] 使用；下层函数一律参数化
// 接收路径，单测可指到临时目录）
const PASSWD: &str = "/etc/passwd";
const GROUP: &str = "/etc/group";
const HOST_FONTS: &str = "/usr/share/easytidy-host";
const FONTCONF_DIR: &str = "/etc/fonts";

const FONTCONF_XML: &str = "<?xml version=\"1.0\"?>\n\
<!DOCTYPE fontconfig SYSTEM \"fonts.dtd\">\n\
<fontconfig>\n\
  <dir>/usr/share/easytidy-host/fonts</dir>\n\
  <dir>/usr/share/easytidy-host/.local/share/fonts</dir>\n\
</fontconfig>\n";

// ── 身份（单一事实源：server 身份自发现 + ctool ensure-home 共用）────────────

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
/// 1. 配置用户名 → `/home/<name>`（ctool 建号/ensure-home 建目录，不依赖
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
    /// 建号跳过原因（uid 被他人占用等；ctool 输出 stderr 提示）
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
    let mut plan = PreparePlan {
        uid: spec.uid,
        gid: spec.gid,
        home: resolve_identity(passwd, spec.uid, spec.gid, spec.user_name.as_deref()).home,
        ..Default::default()
    };
    let Some(name) = spec.user_name.clone() else {
        return plan;
    };
    let entries = parse_passwd(passwd);

    // 幂等：同名用户已存在 → 不建号（原条目原样保留）
    if entries.iter().any(|e| e.name == name) {
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
    /// 宿主字体/图标挂载根（/usr/share/easytidy-host）
    pub host_fonts: PathBuf,
    /// fontconfig 目录（/etc/fonts；写其下 local.conf）
    pub fontconf_dir: PathBuf,
}

/// 准备结果报告（ctool 据此输出提示）。
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

/// 容器内准备入口（ctool `prepare` 子命令；容器内 root 执行）。
///
/// 读 /etc/passwd + /etc/group → [`plan_prepare`]（纯）→ [`apply_plan`]（IO）。
/// 幂等，重复调用无害。
pub fn prepare_in_container(uid: u32, gid: u32, user_name: Option<&str>) -> Result<PrepareReport> {
    // 登录 shell 容器内探测（/bin/bash 存在优先；ctool 本身不依赖任何 shell）
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
    Ok(PrepareReport {
        user_created: plan.add_passwd.is_some(),
        skip_reason: plan.skip_reason.clone(),
    })
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

/// fontconfig 宿主字体接入（幂等覆写）。
///
/// flavor `gui=true` 把宿主字体/图标只读挂到 `/usr/share/easytidy-host/`；
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
        // 未配置用户名：不建号，仅 ensure-home（home 按身份解析）
        let plan = plan_prepare(PASSWD, "", &spec(1000, 1000, None));
        assert!(plan.add_passwd.is_none());
        assert!(plan.add_group.is_none());
        assert!(plan.remove_passwd.is_empty());
        assert_eq!(plan.home, "/home/tidy");
        // 无条目 → /home/uid<uid>
        let plan = plan_prepare("", "", &spec(2000, 2000, None));
        assert_eq!(plan.home, "/home/uid2000");
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

    #[test]
    fn test_write_fontconfig_skips_without_mount() {
        let (tmp, _paths) = tmp_env();
        let r = write_fontconfig(
            &tmp.path().join("no-such-host"),
            &tmp.path().join("no-such-fonts"),
        );
        assert!(r.is_ok(), "无挂载/无 fontconfig 目录应静默跳过");
    }

    #[test]
    fn test_write_fontconfig_writes_conf() {
        let (tmp, _paths) = tmp_env();
        let host_fonts = tmp.path().join("host");
        let fonts_dir = tmp.path().join("fonts");
        fs::create_dir_all(&host_fonts).unwrap();
        fs::create_dir_all(&fonts_dir).unwrap();
        write_fontconfig(&host_fonts, &fonts_dir).unwrap();
        let conf = fs::read_to_string(fonts_dir.join("local.conf")).unwrap();
        assert!(conf.contains("<fontconfig>"));
        assert!(conf.contains("/usr/share/easytidy-host/fonts"));
        assert!(conf.contains("/usr/share/easytidy-host/.local/share/fonts"));
    }
}
