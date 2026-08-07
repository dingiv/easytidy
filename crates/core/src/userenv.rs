//! 宿主用户探测（用户一致性映射，distrobox 式）。
//!
//! 容器创建时（`create_with_config`，`user_home=true`）读取宿主用户名/uid/gid/home，
//! 经 `EASYTIDY_USER_*` 环境变量注入容器；容器内 server 据此创建同名用户并以
//! `su` 以该用户拉起应用——避免容器内 root 读写宿主挂载目录的权限问题。

/// 宿主用户信息（当前进程实际身份对应的用户）。
/// `Serialize`：GUI 配置管理器"用户"面板展示宿主身份（uid 映射语义对照表数据源）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct HostUser {
    /// 用户名（/etc/passwd 中 uid 对应的登录名）
    pub name: String,
    /// 宿主 uid
    pub uid: u32,
    /// 宿主 gid
    pub gid: u32,
    /// 宿主 home（$HOME env，缺失时回退 `dirs::home_dir()`）
    pub home: String,
}

/// 探测当前宿主用户。
///
/// - uid/gid：libc `getuid()`/`getgid()`（进程实际身份，非 setuid 伪装身份）
/// - 用户名：从 `/etc/passwd` 按 uid 解析（简单行解析，避免引入额外依赖）
/// - home：`$HOME` env，缺失时回退 `dirs::home_dir()`
///
/// 任一步失败返回 `None`（上层回退 root 容器，不做用户映射）。
pub fn host_user() -> Option<HostUser> {
    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };
    let name = username_for_uid(uid)?;
    let home = std::env::var("HOME")
        .ok()
        .filter(|h| !h.is_empty())
        .or_else(|| dirs::home_dir().map(|p| p.to_string_lossy().into_owned()))?;
    if home.is_empty() {
        return None;
    }
    Some(HostUser {
        name,
        uid,
        gid,
        home,
    })
}

/// 从 /etc/passwd 解析 uid 对应的用户名。
fn username_for_uid(uid: u32) -> Option<String> {
    username_for_uid_from_file("/etc/passwd", uid)
}

/// 从 passwd 文件路径解析（可注入测试用路径）。
fn username_for_uid_from_file(path: &str, uid: u32) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    username_for_uid_in(&content, uid)
}

/// passwd 文本解析（`name:x:uid:gid:gecos:home:shell`）。
fn username_for_uid_in(content: &str, uid: u32) -> Option<String> {
    for line in content.lines() {
        let fields: Vec<&str> = line.split(':').collect();
        if fields.len() < 3 {
            continue;
        }
        if let Ok(line_uid) = fields[2].parse::<u32>() {
            if line_uid == uid {
                return Some(fields[0].to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_host_user_matches_current_process() {
        // 测试环境应能探测到宿主用户，且与当前进程实际 uid/gid 一致
        let user = host_user().expect("测试环境应能探测到宿主用户");
        let uid = unsafe { libc::getuid() };
        let gid = unsafe { libc::getgid() };
        assert_eq!(user.uid, uid);
        assert_eq!(user.gid, gid);
        assert!(!user.name.is_empty(), "用户名不能为空");
        assert!(!user.home.is_empty(), "home 不能为空");
    }

    #[test]
    fn test_username_for_uid_in() {
        let passwd = "root:x:0:0:root:/root:/bin/sh\n\
                     jiugui5209:x:1000:1000::/home/jiugui5209:/bin/bash\n\
                     malformed-line\n";
        assert_eq!(
            username_for_uid_in(passwd, 1000).as_deref(),
            Some("jiugui5209")
        );
        assert_eq!(username_for_uid_in(passwd, 0).as_deref(), Some("root"));
        // 不存在 / 畸形行 / 非数字 uid 字段 → None
        assert_eq!(username_for_uid_in(passwd, 999), None);
        assert_eq!(username_for_uid_in("", 0), None);
    }

    #[test]
    fn test_username_for_uid_missing_file() {
        // 文件不可读 → None（host_user 由此回退 root，不映射）
        assert_eq!(username_for_uid_from_file("/nonexistent/passwd", 0), None);
    }
}
