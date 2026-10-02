//! 容器内用户准备（宿主侧以 root 一次性 exec 容器内 easytidy-dock）。
//!
//! 新身份模型下容器直接以配置的 uid/gid 运行（无 init 镜像烘焙）。
//! 准备逻辑本身是**纯 Rust**（[`crate::env::incontainer`]）：容器内的
//! `easytidy-dock` 二进制（ro bind-mount，musl 静态）执行 fontconfig
//! 接入 / 建号（行式读写 /etc/passwd、/etc/group）/ 家目录补齐——
//! **零容器内命令依赖**（无 sh/useradd/sed/awk/getent，alpine/busybox
//! 与 debian 同一路径）。
//!
//! 本模块只负责宿主侧 exec：[`Podman::prepare_container`] 经
//! [`Podman::exec_oneshot`]（零 podman CLI）以 root 运行
//! `easytidy-dock prepare`，校验退出码。

use crate::error::{Error, Result};
use crate::models::ContainerParams;

use super::Podman;

impl Podman {
    /// 容器 running 时执行容器内准备（root，一次性非 tty）：
    /// - fontconfig 接入（恒执行，幂等）
    /// - 建号（**恒执行**：配置 `user_name` → `/home/<name>`；未配置 → 镜像
    ///   真实条目跟随，无条目/占位 → `uid<uid>` + `/home/uid<uid>`。VS Code
    ///   Dev Containers attach 靠 getent 解析容器用户，条目必须存在）
    /// - 家目录补齐（**恒执行**：缺失则创建、属主/权限纠正为 uid:gid/750）
    ///
    /// 实现：exec 容器内 `/run/easytidy-bin/easytidy-dock prepare --uid --gid
    /// [--name]`（argv 直传，不经 shell）——逻辑见
    /// [`crate::env::incontainer::prepare_in_container`]。
    ///
    /// 调用点：容器创建启动后 / 重建后 / 模板应用后（GUI 与 CLI 均调）。
    /// 幂等，重复调用无害（覆盖"用户改了 user_name 后重建"场景）。
    pub async fn prepare_container(
        &self,
        name: &str,
        params: &ContainerParams,
        first_run: bool,
    ) -> Result<()> {
        let (uid, gid) =
            crate::podman::resolve_container_user(params, crate::userenv::host_user().as_ref())?;
        let mut argv = vec![
            Self::DOCK_TARGET.to_string(),
            "prepare".to_string(),
            "--uid".to_string(),
            uid.to_string(),
            "--gid".to_string(),
            gid.to_string(),
        ];
        if let Some(n) = &params.user_name {
            argv.push("--name".to_string());
            argv.push(n.clone());
        }
        if first_run {
            argv.push("--first-run".to_string());
        }
        let argv2 = argv.clone();
        let exec = async {
            self.exec_oneshot(name, "0", argv2)
                .await
                .map_err(|e| Error::Connect(format!("容器内准备 exec 失败：{e}")))
        };
        // 兑底：exec 会话可能因 prepare 的孙子进程（dbus-daemon 等）短期继承
        // 会话管道而不 EOF，历史实测最长 ~2m39s。prepare 幂等且自愈，超时不应
        // 阻塞重建主链路——超时视为已完成，下次启动自会补齐。
        let out = match tokio::time::timeout(std::time::Duration::from_secs(60), exec).await {
            Ok(res) => res?,
            Err(_) => {
                tracing::warn!(
                    "容器内准备 exec 超过 60s 未返回（可能被残留子进程拖住输出流），放行不阻塞；下次启动自会补齐"
                );
                return Ok(());
            }
        };
        if out.code != 0 {
            return Err(Error::Connect(format!(
                "容器内准备失败（退出码 {}）：\n{}",
                out.code, out.stderr
            )));
        }
        if !out.stderr.trim().is_empty() {
            tracing::warn!("容器内准备提示（{}）：{}", name, out.stderr.trim());
        }
        // 内置脚本注入（→ ${HOME}/.easytidy/）：best-effort，失败不阻断准备主链路。
        // home 由 exec 阶段 ensure-home 保证已存在（上一步已返回成功）。
        if let Err(e) = self.install_scripts(name, params).await {
            tracing::warn!("内置脚本注入失败（忽略，{name}）：{e}");
        }
        Ok(())
    }
}
