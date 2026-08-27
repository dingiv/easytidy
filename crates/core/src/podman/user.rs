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
///
/// **运行时/podman 数字占位条目（2026-08-27 实测）**：容器以数字
/// `<uid>:<gid>` 启动时，运行时自动向可写层写入占位条目——
/// `/etc/passwd`：`<uid>:*:<uid>:<gid>:container user:/:/bin/sh`、
/// `/etc/group`：`<gid>:x:<gid>:<gid>`（名字即数字）。若不先清除：
/// - uid 保护分支会把占位误判为"他人占用" → 静默跳过建号
/// - busybox adduser 建组首选 gid=uid，占位组占着 gid 会拿不到
/// 占位判定 = 条目名恰为数字 uid/gid（真实用户名等于数字 uid 的极端情形
/// 不在保护范围，可接受）。
///
/// **busybox adduser 坑（2026-08-27 实测）**：
/// - `-G` 取**组名**而非数字 gid（数字报 "unknown group 1011"）
/// - 让 `adduser` 自动建同名组（首选 gid=uid，占位清除后恰好可用）
pub fn build_useradd_script(name: &str, uid: u32, gid: u32, home: &str) -> String {
    format!(
        r#"set -u
if grep -q "^{name}:" /etc/passwd; then
  exit 0
fi
# 运行时数字占位条目（条目名 == uid/gid）先清除，再继续正式建号
if [ "$(getent passwd {uid} | cut -d: -f1)" = "{uid}" ]; then
  sed -i '/^{uid}:/d' /etc/passwd
fi
if [ "$(getent group {gid} | cut -d: -f1)" = "{gid}" ]; then
  sed -i '/^{gid}:/d' /etc/group
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
  if [ "{uid}" = "{gid}" ]; then
    # busybox：不预建组，adduser 自动建同名组（首选 gid=uid）
    adduser -D -u {uid} -h {home} -s "$SHELL" {name}
  else
    getent group "{gid}" >/dev/null || addgroup -g {gid} {name}
    adduser -D -u {uid} -G {name} -h {home} -s "$SHELL" {name}
  fi
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
    fn test_build_useradd_script_shape_uid_eq_gid() {
        let s = build_useradd_script("tidy", 1000, 1000, "/home/tidy");
        // 幂等前缀：同名已存在即退出
        assert!(s.contains("grep -q \"^tidy:\" /etc/passwd"));
        // 运行时数字占位条目清除（条目名 == uid/gid）
        assert!(s.contains(r#"sed -i '/^1000:/d' /etc/passwd"#));
        assert!(s.contains(r#"sed -i '/^1000:/d' /etc/group"#));
        // uid 冲突保护：不覆盖他人 uid
        assert!(s.contains("getent passwd 1000"));
        // debian 分支：useradd -g 数字 gid
        assert!(s.contains("useradd -m -u 1000 -g 1000 -d /home/tidy -s \"$SHELL\" tidy"));
        assert!(s.contains("chown 1000:1000 \"/home/tidy\""));
        // 双工具回退分支存在
        assert!(s.contains("command -v groupadd"));
        // busybox uid==gid：不预建组，adduser 自动建同名组（-G 数字会失败）
        assert!(s.contains("[ \"1000\" = \"1000\" ]"));
        assert!(s.contains("adduser -D -u 1000 -h /home/tidy -s \"$SHELL\" tidy"));
    }

    #[test]
    fn test_build_useradd_script_shape_gid_differs() {
        let s = build_useradd_script("tidy", 1001, 1002, "/home/tidy");
        // 运行时数字占位条目清除（条目名 == gid）
        assert!(s.contains(r#"sed -i '/^1002:/d' /etc/group"#));
        // busybox gid!=uid：预建组（组名=name）+ adduser -G 组名（非数字）
        assert!(s.contains("addgroup -g 1002 tidy"));
        assert!(s.contains("adduser -D -u 1001 -G tidy -h /home/tidy -s \"$SHELL\" tidy"));
        assert!(s.contains("chown 1001:1002 \"/home/tidy\""));
        // -G 不得跟数字 gid（busybox 把 -G 当组名）
        assert!(!s.contains("-G 1002"));
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
