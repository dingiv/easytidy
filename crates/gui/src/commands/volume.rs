//! 数据卷命令（named volume）。
//!
//! 第二轮（配置界面挂卷）先出 `volume_list`（卷名下拉数据源，与卷管理面板
//! 共用）；第三轮补 create / rm。

use crate::state::PodmanState;

/// 列出宿主全部数据卷（**全量**——含外部工具建的，接管策略定案；
/// `labels.manager == "easytidy"` 为 easytidy 创建，GUI 打标用）。
#[tauri::command]
pub async fn volume_list(
    podman: tauri::State<'_, PodmanState>,
) -> Result<Vec<easytidy_core::podman::volume::VolumeInfo>, String> {
    let p = podman.get().await.map_err(|e| e.to_string())?;
    let result = p.volume_list().await.map_err(|e| e.to_string())?;
    podman.return_podman(p).await;
    Ok(result)
}

/// 创建数据卷（`manager=easytidy` label）。同名已存在 → podman 500 already
/// exists（实测），错误原文透传。
#[tauri::command]
pub async fn volume_create(
    podman: tauri::State<'_, PodmanState>,
    name: String,
) -> Result<easytidy_core::podman::volume::VolumeInfo, String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("卷名不能为空".to_string());
    }
    let p = podman.get().await.map_err(|e| e.to_string())?;
    let result = p.volume_create(&name).await.map_err(|e| e.to_string())?;
    podman.return_podman(p).await;
    Ok(result)
}

/// 删除数据卷。**挂载中 → podman 409（is in use）**，错误透传给前端展示；
/// `force` 对应 podman `rm -f`（可选，camelCase 传参）。
#[tauri::command]
pub async fn volume_rm(
    podman: tauri::State<'_, PodmanState>,
    name: String,
    force: Option<bool>,
) -> Result<(), String> {
    let p = podman.get().await.map_err(|e| e.to_string())?;
    let result = p
        .volume_remove(name.trim(), force.unwrap_or(false))
        .await
        .map_err(|e| e.to_string());
    podman.return_podman(p).await;
    result
}
