//! 内置脚本注入：编译期打包 `crates/gui/scripts/*`，容器准备阶段复制进
//! 容器 `${HOME}/.easytidy/`。
//!
//! 机制（零 podman CLI，复用既有 libpod 直连栈）：
//! 1. 脚本内容 `include_str!` 编译期打进二进制（同 conf/appdata 种子模式）
//! 2. 内存构建 tar（含 `.easytidy/` 目录条目），条目 uid/gid = 容器用户
//! 3. `PUT /containers/{id}/archive?path=<home>` 提取到容器内 home
//!    （podman archive PUT 语义：tar 条目按原路径解出，目录自动创建）
//!
//! 目标 home 由 [`crate::env::incontainer::resolve_identity`] 解析——与
//! dock ensure-home 单一事实源同源（配置用户名 → passwd 条目 → uid<uid>），
//! 必然与 prepare 建出的 home 一致。
//!
//! 时序：在 `prepare_container` 的 exec（建号/ensure-home）之后执行——home
//! 必须已存在。幂等：每次准备都覆盖刷新（脚本更新后重建/重启即生效）。
//! best-effort：失败仅落日志不阻断容器准备主链路。

use tar::{Builder, Header};

use crate::error::{Error, Result};
use crate::models::ContainerParams;

use super::Podman;

/// 内置脚本表（容器内文件名 → 编译期内容）。
///
/// 新增脚本：把文件放进 `crates/gui/scripts/` 后在此登记一行。
/// 模式 0755（`sudoer.sh` 由容器内 root 以 bash 执行，保持可执行）。
const SCRIPTS: &[(&str, &str)] = &[("sudoer.sh", include_str!("../../../gui/scripts/sudoer.sh"))];

/// 容器内目标目录（相对 home）。
pub const SCRIPTS_DIR: &str = ".easytidy";

/// 构建注入 tar（纯函数，单测点）：`.easytidy/` 目录 + 各脚本文件。
///
/// 条目属主 = 容器用户 uid/gid，目录 0755 / 文件 0755。
fn scripts_tar(uid: u32, gid: u32) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    {
        let mut tar = Builder::new(&mut buf);

        let mut dir_header = Header::new_gnu();
        dir_header.set_size(0);
        dir_header.set_mode(0o755);
        dir_header.set_uid(uid.into());
        dir_header.set_gid(gid.into());
        dir_header.set_entry_type(tar::EntryType::Directory);
        dir_header.set_mtime(0);
        tar.append_data(&mut dir_header, format!("{SCRIPTS_DIR}/"), std::io::empty())
            .map_err(|e| Error::Config(format!("构建脚本 tar 目录条目失败：{e}")))?;

        for (name, content) in SCRIPTS {
            let mut header = Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o755);
            header.set_uid(uid.into());
            header.set_gid(gid.into());
            header.set_mtime(0);
            tar.append_data(
                &mut header,
                format!("{SCRIPTS_DIR}/{name}"),
                content.as_bytes(),
            )
            .map_err(|e| Error::Config(format!("构建脚本 tar 条目 {name} 失败：{e}")))?;
        }
        tar.finish()
            .map_err(|e| Error::Config(format!("构建脚本 tar 收尾失败：{e}")))?;
    }
    Ok(buf)
}

impl Podman {
    /// 把内置脚本复制进运行中容器 `${HOME}/.easytidy/`（best-effort 调用点）。
    ///
    /// home 经镜像/容器 passwd + 配置用户名解析（[`resolve_identity`] 语义）；
    /// archive PUT 到 home，tar 内含 `.easytidy/` 前缀自动落位。
    pub async fn install_scripts(&self, name: &str, params: &ContainerParams) -> Result<()> {
        let (uid, gid) =
            crate::podman::resolve_container_user(params, crate::userenv::host_user().as_ref())?;
        let passwd = self.read_container_file(name, "/etc/passwd").await?;
        let identity = crate::env::incontainer::resolve_identity(
            &passwd,
            uid,
            gid,
            params.user_name.as_deref(),
        );
        let body = scripts_tar(identity.uid, identity.gid)?;
        let url = format!(
            "/containers/{}/archive?path={}",
            Self::urlquery_encode(name),
            // archive 路径保留 `/`（urlquery_encode 不转义），`?` 等需编码
            Self::urlquery_encode(&identity.home)
        );
        let (status, resp) = self
            .http
            .request_bytes("PUT", &url, Some(body.into()))
            .await
            .map_err(|e| Error::Connect(format!("脚本注入 archive PUT 失败：{e}")))?;
        if !(200..300).contains(&status) {
            return Err(Error::Connect(format!(
                "脚本注入失败（→ {}:{}）：HTTP {status}：{}",
                identity.home,
                SCRIPTS_DIR,
                String::from_utf8_lossy(&resp).trim()
            )));
        }
        tracing::info!(
            "内置脚本已注入容器 {name}（{}:/{}，{} 个文件）",
            identity.home,
            SCRIPTS_DIR,
            SCRIPTS.len()
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(bytes: &[u8]) -> Vec<(String, u64, u64, Option<u64>)> {
        let mut ar = tar::Archive::new(bytes);
        let mut out = Vec::new();
        for e in ar.entries().unwrap() {
            let e = e.unwrap();
            out.push((
                e.path().unwrap().to_string_lossy().into_owned(),
                e.header().uid().unwrap(),
                e.header().gid().unwrap(),
                e.header().size().ok(),
            ));
        }
        out
    }

    #[test]
    fn tar_layout_ownership_mode() {
        let bytes = scripts_tar(1000, 1000).unwrap();
        let es = entries(&bytes);
        assert_eq!(es.len(), 1 + SCRIPTS.len());
        assert_eq!(es[0], (".easytidy/".to_string(), 1000, 1000, Some(0)));
        for (i, (name, content)) in SCRIPTS.iter().enumerate() {
            assert_eq!(es[i + 1].0, format!(".easytidy/{name}"));
            assert_eq!(es[i + 1].1, 1000);
            assert_eq!(es[i + 1].2, 1000);
            assert_eq!(es[i + 1].3, Some(content.len() as u64));
        }
    }

    /// 实测（需真实 podman + 运行中容器 desk_pilot）：注入后容器内
    /// $HOME/.easytidy/sudoer.sh 存在且属主 = 容器用户。
    #[tokio::test]
    #[ignore]
    async fn test_install_scripts_smoke() {
        let podman = Podman::connect().await.unwrap();
        podman
            .install_scripts(
                "desk_pilot",
                &serde_json::from_str::<ContainerParams>(r#"{"image":"ubuntu:25.10"}"#).unwrap(),
            )
            .await
            .unwrap();
    }
}
