//! 挂载路径变量展开（宿主侧 / 容器侧**上下文相关**变量）。
//!
//! conf 模板 / 实例配置里的挂载路径可用 `${VAR}` 表达，运行时按「宿主侧 /
//! 容器侧」上下文展开——同一变量名在 `host_path`（宿主侧）与 `container_path`
//! （容器侧）取不同值：
//!
//! ```yaml
//! mounts:
//!   - host_path: ${HOME}/workspace        # → 宿主 home（如 /home/div）
//!     container_path: ${HOME}/workspace   # → 容器用户 home（如 /home/ubuntu）
//!     read_only: false
//! ```
//!
//! 支持四个变量（语法仅 `${VAR}`，不支持 `~` / 无括号 `$VAR`）：
//! - `HOME`：宿主侧 = 宿主 home；容器侧 = 容器用户 home（镜像 /etc/passwd 解析）
//! - `USER`：宿主侧 = 宿主机名；容器侧 = 容器用户名（镜像 /etc/passwd 解析）
//! - `UID`：宿主侧 = 宿主 uid；容器侧 = 容器 uid
//! - `GID`：宿主侧 = 宿主 gid；容器侧 = 容器 gid
//!
//! 设计原则：**存占位符、运行时展开**。`expand_mounts` 是纯函数（占位符 + 两侧
//! `PathVars` → 具体路径）。容器侧 HOME/USER 且未配 user_name 时需要的镜像
//! /etc/passwd，由调用方（`create_with_config`）探测后经 `container_path_vars`
//! 注入——本模块不触网、不碰 podman，保持纯函数可单测。

use std::collections::HashMap;

use crate::error::{Error, Result};
use crate::env::resolve_identity;
use crate::models::MountConfig;
use crate::userenv::HostUser;

/// 一侧（宿主或容器）解析后的路径变量取值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PathVars {
    pub home: String,
    pub user: String,
    pub uid: u32,
    pub gid: u32,
}

impl PathVars {
    /// 空占位（该侧路径不含变量、且宿主用户不可得时用——`expand_path` 不会
    /// 触碰这些值，仅占位让调用方类型齐整）。
    pub(crate) fn empty() -> Self {
        PathVars {
            home: String::new(),
            user: String::new(),
            uid: 0,
            gid: 0,
        }
    }
}

/// 宿主侧变量——直接取自宿主登录用户（[`HostUser`]）。
pub(crate) fn host_path_vars(host: &HostUser) -> PathVars {
    PathVars {
        home: host.home.clone(),
        user: host.name.clone(),
        uid: host.uid,
        gid: host.gid,
    }
}

/// 容器侧变量。
///
/// `image_passwd`：镜像 /etc/passwd 文本（容器侧 `${HOME}`/`${USER}` 且未配
/// `user_name` 时由调用方探测注入）。
/// - 有镜像 passwd → 复用单一事实源 [`resolve_identity`]，与容器内 server/ctool
///   算出的运行时 `$HOME` 完全一致（如 ubuntu 镜像 uid 1000 → `/home/ubuntu`）。
/// - 无镜像 passwd → 退化：`user_name` 有 → `/home/<name>`；无 → `/home/uid<uid>`。
pub(crate) fn container_path_vars(
    uid: u32,
    gid: u32,
    user_name: Option<&str>,
    image_passwd: Option<&str>,
) -> PathVars {
    let (home, user) = match image_passwd {
        Some(pw) => {
            let id = resolve_identity(pw, uid, gid, user_name);
            (id.home, id.name)
        }
        None => match user_name {
            Some(n) => (format!("/home/{n}"), n.to_string()),
            None => (format!("/home/uid{uid}"), format!("uid{uid}")),
        },
    };
    PathVars {
        home,
        user,
        uid,
        gid,
    }
}

/// 展开单个路径串中的 `${VAR}`（按一侧的 [`PathVars`] 取值）。
///
/// 无变量 → 原样返回；`$` 后非 `{`（如 `$HOME`、`$ foo`）→ 字面量（不支持无括号
/// `$VAR`）；`${` 无闭合 `}` → 剩余整体作字面量；未知变量 → [`Error::Config`]
///（fail-fast，不静默留字面量）。
pub(crate) fn expand_path(s: &str, pv: &PathVars) -> Result<String> {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        // 是否命中 "${"（两个 ASCII 字符）
        if bytes[i] == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            let open = i + 2;
            match s[open..].find('}') {
                Some(rel_close) => {
                    let close = open + rel_close;
                    let var = &s[open..close];
                    let value = match var {
                        "HOME" => pv.home.clone(),
                        "USER" => pv.user.clone(),
                        "UID" => pv.uid.to_string(),
                        "GID" => pv.gid.to_string(),
                        _ => {
                            return Err(Error::Config(format!(
                                "未知路径变量 ${{{var}}}（挂载路径仅支持 ${{HOME}} / ${{USER}} / ${{UID}} / ${{GID}}）"
                            )));
                        }
                    };
                    out.push_str(&value);
                    i = close + 1;
                }
                // 无闭合 } → 剩余（含 "${"）整体作字面量
                None => {
                    out.push_str(&s[i..]);
                    break;
                }
            }
        } else {
            let ch = s[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    Ok(out)
}

/// 展开一组挂载的 `host_path`（宿主侧）与 `container_path`（容器侧）。
///
/// 纯函数：输入占位符 + 两侧 [`PathVars`]，输出具体路径的 `MountConfig` 副本
///（`read_only` 原样保留）。
pub(crate) fn expand_mounts(
    mounts: &[MountConfig],
    host: &PathVars,
    container: &PathVars,
) -> Result<Vec<MountConfig>> {
    mounts
        .iter()
        .map(|m| {
            let host_path = expand_path(&m.host_path, host)?;
            let container_path = expand_path(&m.container_path, container)?;
            Ok(MountConfig {
                host_path,
                container_path,
                read_only: m.read_only,
            })
        })
        .collect()
}

/// 容器侧展开是否需要镜像 /etc/passwd 探测。
///
/// 仅当「某 `container_path` 用到 `${HOME}` 或 `${USER}`」**且**「未配
/// `user_name`」时返回 true（其余——无变量 / 仅 `${UID}``${GID}` / 已配
/// `user_name`——都可直接算，零探测）。
pub(crate) fn needs_image_passwd(mounts: &[MountConfig], user_name: Option<&str>) -> bool {
    if user_name.is_some() {
        return false;
    }
    mounts
        .iter()
        .any(|m| contains_var(&m.container_path, "HOME") || contains_var(&m.container_path, "USER"))
}

/// 路径串中是否含 `${NAME}`。
fn contains_var(s: &str, name: &str) -> bool {
    s.contains(&format!("${{{name}}}"))
}

// ── 镜像 /etc/passwd 探测缓存（进程内，按 image 名）──────────────────────────
//
// 镜像 passwd 对给定 image 在会话内视为不可变；image 更新（同 tag 换 digest）
// 属罕见操作，缓存不主动失效（重启进程即刷新）。探测本身（建一次性容器 + 读
// archive）在 podman 客户端侧，本处仅缓存其文本结果。

static IMAGE_PASSWD_CACHE: std::sync::LazyLock<std::sync::Mutex<HashMap<String, String>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

pub(crate) fn cached_image_passwd(image: &str) -> Option<String> {
    IMAGE_PASSWD_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(image)
        .cloned()
}

pub(crate) fn store_image_passwd(image: &str, passwd: String) {
    IMAGE_PASSWD_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(image.to_string(), passwd);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::MountConfig;
    use crate::userenv::HostUser;

    fn host() -> HostUser {
        HostUser {
            name: "div".into(),
            uid: 1000,
            gid: 1000,
            home: "/home/div".into(),
        }
    }

    const UBUNTU_PASSWD: &str = "root:x:0:0:root:/root:/bin/bash\nubuntu:x:1000:1000:Ubuntu:/home/ubuntu:/bin/bash\n";

    #[test]
    fn expand_host_side() {
        let h = host_path_vars(&host());
        assert_eq!(expand_path("${HOME}/workspace", &h).unwrap(), "/home/div/workspace");
        assert_eq!(expand_path("${USER}", &h).unwrap(), "div");
        assert_eq!(expand_path("${UID}", &h).unwrap(), "1000");
        assert_eq!(expand_path("${GID}", &h).unwrap(), "1000");
    }

    #[test]
    fn expand_container_side_no_user_name_uses_image_passwd() {
        // chrome 场景：uid 1000、无 user_name、ubuntu 镜像 → /home/ubuntu
        let c = container_path_vars(1000, 1000, None, Some(UBUNTU_PASSWD));
        assert_eq!(expand_path("${HOME}/workspace", &c).unwrap(), "/home/ubuntu/workspace");
        assert_eq!(expand_path("${USER}", &c).unwrap(), "ubuntu");
        assert_eq!(expand_path("${UID}", &c).unwrap(), "1000");
        assert_eq!(expand_path("${GID}", &c).unwrap(), "1000");
    }

    #[test]
    fn expand_container_side_with_user_name_no_passwd_needed() {
        let c = container_path_vars(1000, 1000, Some("alice"), None);
        assert_eq!(expand_path("${HOME}", &c).unwrap(), "/home/alice");
        assert_eq!(expand_path("${USER}", &c).unwrap(), "alice");
    }

    #[test]
    fn expand_container_side_no_passwd_falls_back_to_uid_home() {
        let c = container_path_vars(1000, 1000, None, None);
        assert_eq!(expand_path("${HOME}", &c).unwrap(), "/home/uid1000");
        assert_eq!(expand_path("${USER}", &c).unwrap(), "uid1000");
    }

    #[test]
    fn expand_no_var_passthrough() {
        let h = host_path_vars(&host());
        assert_eq!(expand_path("/run/user/1000", &h).unwrap(), "/run/user/1000");
        assert_eq!(expand_path("/tmp/.X11-unix", &h).unwrap(), "/tmp/.X11-unix");
    }

    #[test]
    fn expand_multiple_vars_and_mixed() {
        let h = host_path_vars(&host());
        assert_eq!(
            expand_path("${HOME}/.config/${USER}/${UID}", &h).unwrap(),
            "/home/div/.config/div/1000"
        );
    }

    #[test]
    fn expand_no_brace_dollar_is_literal() {
        let h = host_path_vars(&host());
        assert_eq!(expand_path("$HOME/x", &h).unwrap(), "$HOME/x");
        assert_eq!(expand_path("a$b${HOME}", &h).unwrap(), "a$b/home/div");
    }

    #[test]
    fn expand_unclosed_brace_is_literal() {
        let h = host_path_vars(&host());
        assert_eq!(expand_path("${HOME/x", &h).unwrap(), "${HOME/x");
    }

    #[test]
    fn expand_unknown_var_errors() {
        let h = host_path_vars(&host());
        let err = expand_path("${FOO}/x", &h).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("${FOO}"), "应点名未知变量：{msg}");
        assert!(msg.contains("${HOME}"), "应列出支持的变量：{msg}");
    }

    #[test]
    fn expand_mounts_contextual() {
        let h = host_path_vars(&host());
        let c = container_path_vars(1000, 1000, None, Some(UBUNTU_PASSWD));
        let mounts = vec![MountConfig {
            host_path: "${HOME}/workspace".into(),
            container_path: "${HOME}/workspace".into(),
            read_only: false,
        }];
        let out = expand_mounts(&mounts, &h, &c).unwrap();
        assert_eq!(out[0].host_path, "/home/div/workspace");
        assert_eq!(out[0].container_path, "/home/ubuntu/workspace");
        assert!(!out[0].read_only);
    }

    #[test]
    fn needs_image_passwd_logic() {
        let no_user: Option<&str> = None;
        let with_user: Option<&str> = Some("alice");

        let home_mount = vec![MountConfig {
            host_path: "/x".into(),
            container_path: "${HOME}/w".into(),
            read_only: false,
        }];
        let uid_mount = vec![MountConfig {
            host_path: "/x".into(),
            container_path: "/home/uid${UID}".into(),
            read_only: false,
        }];
        let plain_mount = vec![MountConfig {
            host_path: "/x".into(),
            container_path: "/run/user/1000".into(),
            read_only: false,
        }];

        // 无 user_name + 容器侧 ${HOME} → 需探测
        assert!(needs_image_passwd(&home_mount, no_user));
        // 配了 user_name → 不需探测（即便有 ${HOME}）
        assert!(!needs_image_passwd(&home_mount, with_user));
        // 容器侧仅 ${UID} → 不需探测
        assert!(!needs_image_passwd(&uid_mount, no_user));
        // 纯字面量 → 不需探测
        assert!(!needs_image_passwd(&plain_mount, no_user));
    }
}
