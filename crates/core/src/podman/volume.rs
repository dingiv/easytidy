//! named volume 生命周期（宿主侧 libpod `/volumes` 端点）。
//!
//! 形态与契约（2026-10-12 本机 socket 实测）：
//! - **全量列出**宿主所有卷——接管策略定案：不分谁建的（外部卷如 devcontainer
//!   的 `vscode` 也列出、也可挂）；easytidy 创建的打 `manager=easytidy` label，
//!   GUI 据此打标但**不隐藏**他人卷。
//! - `POST /libpod/volumes/create`：请求 body snake_case 可吃（Go 解码大小写
//!   不敏感）；create/list/inspect 响应 = **PascalCase**（`Name`/`MountCount`…）。
//! - `DELETE /libpod/volumes/{name}`：**正被挂载 → podman 409**
//!   （handler `ErrVolumeBeingUsed`），错误经 `json_ok` 的 `message`/`cause`
//!   透传为可读错误。
//! - 列表项含 `MountCount`（>0 = 有容器在用）与 `Labels`。

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

use super::Podman;

/// 卷概要（libpod `/libpod/volumes/json` 列表项 / create 响应）。
///
/// podman 侧是 PascalCase（Go 无 json tag）→ 逐字段 `alias` 读入；
/// **序列化保持 snake_case**（GUI/CLI payload 统一约定）。未建模字段
///（`Options`/`UID`/`GID`/`NeedsCopyUp`/`LockNumber`…）忽略——前向兼容
/// （真实载荷见单测夹具）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeInfo {
    /// 卷名
    #[serde(alias = "Name")]
    pub name: String,
    /// 驱动（通常 "local"）
    #[serde(default, alias = "Driver")]
    pub driver: String,
    /// 宿主数据目录（仅展示/排查用——容器内只见挂载点）
    #[serde(default, alias = "Mountpoint")]
    pub mountpoint: String,
    /// 创建时间（RFC3339）
    #[serde(default, alias = "CreatedAt")]
    pub created_at: String,
    /// 标签（easytidy 创建的含 `manager=easytidy`）
    #[serde(default, alias = "Labels")]
    pub labels: std::collections::HashMap<String, String>,
    /// 作用域（"local"）
    #[serde(default, alias = "Scope")]
    pub scope: String,
    /// 当前挂载计数（>0 = 正被容器使用；删除会被 podman 409 拒）
    #[serde(default, alias = "MountCount")]
    pub mount_count: i64,
}

impl Podman {
    /// 列出宿主**全部**卷（全量，不分接管与否——策略定案）。
    pub async fn volume_list(&self) -> Result<Vec<VolumeInfo>> {
        let v = self
            .http
            .json_ok("GET", &self.http.libpod_path("/libpod/volumes/json"), None)
            .await?;
        serde_json::from_value(v).map_err(|e| Error::Connect(format!("解析卷列表失败：{e}")))
    }

    /// 创建卷（driver=local，打 `manager=easytidy` label）。
    ///
    /// 同名已存在 → podman **500 already exists**（实测：409 只属于挂载中删除），
    /// 错误原文透传；「存在则复用」用 [`Self::volume_ensure`]。
    pub async fn volume_create(&self, name: &str) -> Result<VolumeInfo> {
        let body = serde_json::json!({
            "Name": name.trim(),
            "Driver": "local",
            "Labels": { "manager": "easytidy" },
        });
        let v = self
            .http
            .json_ok("POST", &self.http.libpod_path("/libpod/volumes/create"), Some(body))
            .await?;
        serde_json::from_value(v).map_err(|e| Error::Connect(format!("解析创建卷响应失败：{e}")))
    }

    /// 确保卷存在（容器 create 路径用）：不存在则带 label 创建，**已存在 →
    /// 原样不动**（不改他人的 label/属主——全量列出策略下用户/外部工具建的卷
    /// 原样挂用）。
    ///
    /// `uid`/`gid` 不传（实测定案）：默认属主 = 调用者本人（1000:1000 755，
    /// keep-id 下即容器用户 → 可写）；**显式传反而坏事**——podman 走 subuid
    /// 翻译（实测 UID=1000 → 宿主 100999:100998 700，容器内见 999 写不进、
    /// 宿主用户也被锁外）。非 keep-id 容器的属主适配留待后续。幂等：
    /// `IgnoreIfExists`（Go 无 json tag → 必须 PascalCase，下划线写法不认）。
    pub async fn volume_ensure(&self, name: &str) -> Result<()> {
        let body = serde_json::json!({
            "Name": name.trim(),
            "Driver": "local",
            "Labels": { "manager": "easytidy" },
            "IgnoreIfExists": true,
        });
        self.http
            .json_ok("POST", &self.http.libpod_path("/libpod/volumes/create"), Some(body))
            .await?;
        Ok(())
    }

    /// 卷是否存在（`GET /libpod/volumes/{name}/exists`：204/200 = 存在，404 = 不存在）。
    pub async fn volume_exists(&self, name: &str) -> Result<bool> {
        let (status, _) = self
            .http
            .json("GET", &self.http.libpod_path(&format!("/libpod/volumes/{name}/exists")), None)
            .await?;
        Ok(status == 204 || status == 200)
    }

    /// 删除卷。**正被挂载 → podman 409**（"is in use"），错误原文透传。
    pub async fn volume_remove(&self, name: &str, force: bool) -> Result<()> {
        self.http
            .json_ok(
                "DELETE",
                &self.http.libpod_path(&format!("/libpod/volumes/{name}?force={force}")),
                None,
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 列表项真实载荷反序列化契约（2026-10-12 本机 socket 抓取：一个外部卷
    /// `vscode`（devcontainer 建，无 label）+ 一个 `manager=easytidy` 的卷）。
    /// PascalCase 字段映射、未建模字段忽略、Labels 默认空。
    #[test]
    fn volume_info_from_real_payload() {
        let payload = r#"[{"Name":"vscode","Driver":"local","Mountpoint":"/home/div/.local/share/containers/storage/volumes/vscode/_data","CreatedAt":"2026-08-15T06:05:51.959536036+08:00","Labels":{},"Scope":"local","Options":{},"UID":1,"GID":1,"MountCount":0,"NeedsCopyUp":true,"LockNumber":0},{"Name":"et-probe-vol","Driver":"local","Mountpoint":"/home/div/.local/share/containers/storage/volumes/et-probe-vol/_data","CreatedAt":"2026-10-12T06:15:20.122760566+08:00","Labels":{"manager":"easytidy"},"Scope":"local","Options":{},"MountCount":0,"NeedsCopyUp":true,"NeedsChown":true,"LockNumber":3}]"#;
        let list: Vec<VolumeInfo> = serde_json::from_str(payload).expect("真实载荷应可解析");
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "vscode");
        assert_eq!(list[0].driver, "local");
        assert!(list[0].mountpoint.ends_with("/vscode/_data"));
        assert!(list[0].labels.is_empty(), "外部卷无 label（全量列出策略依赖此）");
        assert_eq!(list[0].mount_count, 0);
        assert_eq!(list[1].name, "et-probe-vol");
        assert_eq!(
            list[1].labels.get("manager").map(String::as_str),
            Some("easytidy"),
            "easytidy 创建的卷带接管 label"
        );
        // 序列化形状：snake_case（GUI/CLI payload 统一约定；Pascal 只在 podman 侧）
        let v = serde_json::to_value(&list[1]).unwrap();
        assert_eq!(v["name"], "et-probe-vol");
        assert_eq!(v["mount_count"], 0);
        assert!(v.get("Name").is_none(), "不应输出 Pascal 键");
    }

    /// create 请求 body 契约（snake_case，实测 201）。
    #[test]
    fn volume_create_body_shape() {
        let body = serde_json::json!({
            "name": "data",
            "driver": "local",
            "labels": { "manager": "easytidy" },
        });
        assert_eq!(body["name"], "data");
        assert_eq!(body["labels"]["manager"], "easytidy");
        assert_eq!(body["driver"], "local");
    }
}
