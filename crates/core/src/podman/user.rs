//! 容器内用户准备（宿主侧 root exec，非 tty 一次性）。
//!
//! 新身份模型下容器直接以配置的 uid/gid 运行（无 init 镜像烘焙）：
//! - **默认**（未配置 `user_name`）：容器仅按 uid 运行，passwd 条目可不存在
//!   （whoami 显示 uid 数字或镜像恰好同 uid 的既有用户）——无需任何准备
//! - **配置 `user_name`**：创建/重建后经 [`Podman::prepare_container`] 以
//!   root exec 执行幂等 useradd，建立 name/uid/gid/home/登录 shell
//!
//! 幂等脚本 + 退出码校验全部走 [`Podman::exec_oneshot`]（exec.rs），零 podman CLI。

use crate::error::{Error, Result};
use crate::models::ContainerParams;

use super::Podman;

/// 生成幂等 useradd 脚本（容器内 root 执行，`/bin/sh -c`）。
///
/// 幂等语义：
/// - 同名用户已存在 → no-op（passwd 里有该名字即认为就位）
/// - uid 被**其他**用户占用 → 不覆盖（容器仍以该 uid 运行，passwd 反查到
///   镜像原用户名，HOME/USER 由 env 提示兜底），仅向 stderr 提示
///
/// 工具探测在脚本内完成（`command -v`）：debian 系 useradd/groupadd，
/// alpine/busybox 回退 adduser/addgroup——宿主无法预知容器内工具。
pub fn build_useradd_script(name: &str, uid: u32, gid: u32, home: &str) -> String {
    format!(
        r#"set -u
if grep -q "^{name}:" /etc/passwd; then
  exit 0
fi
if [ -n "$(getent passwd {uid})" ]; then
  echo "warning: uid {uid} already owned by another user; skipping useradd" >&2
  exit 0
fi
SHELL="$(command -v bash || echo /bin/sh)"
if command -v groupadd >/dev/null 2>&1; then
  getent group "{gid}" >/dev/null || groupadd -g {gid} {name} 2>/dev/null || addgroup -g {gid} {name} 2>/dev/null
  useradd -m -u {uid} -g {gid} -d {home} -s "$SHELL" {name}
else
  getent group "{gid}" >/dev/null || addgroup -g {gid} {name}
  adduser -D -u {uid} -G {gid} -h {home} -s "$SHELL" {name}
fi
mkdir -p "{home}"
chown {uid}:{gid} "{home}"
"#
    )
}

/// 生成 fontconfig 宿主字体接入脚本段（容器内 root 执行，幂等覆写）。
///
/// flavor `gui=true` 把宿主字体/图标只读挂到 `/usr/share/easytidy-host/`；
/// server 非 root 后无法写 `/etc/fonts/local.conf`，改由此处完成
/// （原 server `setup_fontconfig` 的迁移）。
pub fn build_fontconfig_script() -> String {
    r#"if [ -d /usr/share/easytidy-host ] && [ -d /etc/fonts ]; then
  printf '%s\n' \
    '<?xml version="1.0"?>' \
    '<!DOCTYPE fontconfig SYSTEM "fonts.dtd">' \
    '<fontconfig>' \
    '  <dir>/usr/share/easytidy-host/fonts</dir>' \
    '  <dir>/usr/share/easytidy-host/.local/share/fonts</dir>' \
    '</fontconfig>' > /etc/fonts/local.conf
fi
"#
    .to_string()
}

impl Podman {
    /// 容器 running 时执行容器内准备（root，一次性非 tty）：
    /// - fontconfig 接入（恒执行，幂等）
    /// - useradd 建号（仅 `params.user_name` 有值；home = `/home/<name>`，
    ///   登录 shell 容器内探测）
    ///
    /// 调用点：容器创建启动后 / 重建后 / 模板应用后。幂等，重复调用无害
    /// （覆盖"用户改了 user_name 后重建"场景）。
    pub async fn prepare_container(&self, name: &str, params: &ContainerParams) -> Result<()> {
        let (uid, gid) =
            crate::podman::resolve_container_user(params, crate::userenv::host_user().as_ref())?;
        let script = build_fontconfig_script();
        let script = match &params.user_name {
            Some(user) => format!("{script}\n{}", build_useradd_script(user, uid, gid, &format!("/home/{user}"))),
            None => script,
        };
        let out = self
            .exec_oneshot(name, "0", vec!["/bin/sh".to_string(), "-c".to_string(), script])
            .await
            .map_err(|e| Error::Connect(format!("容器内准备 exec 失败：{e}")))?;
        if out.code != 0 {
            return Err(Error::Connect(format!(
                "容器内准备失败（退出码 {}）：\n{}",
                out.code, out.stderr
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_useradd_script_shape() {
        let s = build_useradd_script("tidy", 1000, 1000, "/home/tidy");
        // 幂等前缀：同名已存在即退出
        assert!(s.contains("grep -q \"^tidy:\" /etc/passwd"));
        // uid 冲突保护：不覆盖他人 uid
        assert!(s.contains("getent passwd 1000"));
        // 参数化正确
        assert!(s.contains("useradd -m -u 1000 -g 1000 -d /home/tidy -s \"$SHELL\" tidy"));
        assert!(s.contains("chown 1000:1000 \"/home/tidy\""));
        // 双工具回退分支存在
        assert!(s.contains("command -v groupadd"));
        assert!(s.contains("adduser -D -u 1000"));
    }

    #[test]
    fn test_build_fontconfig_script_shape() {
        let s = build_fontconfig_script();
        assert!(s.contains("/usr/share/easytidy-host/fonts"));
        assert!(s.contains("/etc/fonts/local.conf"));
        // 条件执行：无宿主挂载或无 fontconfig 时静默跳过
        assert!(s.contains("[ -d /usr/share/easytidy-host ]"));
        assert!(s.contains("[ -d /etc/fonts ]"));
    }
}
