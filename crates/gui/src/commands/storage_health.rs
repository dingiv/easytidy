//! 存储健康（docs/18 道路二）GUI 命令：诊断 / 一键修复 / 横幅忽略标记。
//!
//! 核心逻辑在 `core::storage_health`（与 CLI `easytidy doctor` 共用）；
//! 这里只做 Tauri 适配 + 用户级 dismissed 标记持久化。

use std::path::PathBuf;

use crate::state::PodmanState;

/// dismissed 标记文件（`~/.easytidy/data/storage-health-dismissed.json`）
fn dismissed_path() -> Result<PathBuf, String> {
    Ok(easytidy_core::appdata::data_dir()
        .map_err(|e| e.to_string())?
        .join("storage-health-dismissed.json"))
}

/// 诊断（只读：/info + 宿主探测）
#[tauri::command]
pub async fn storage_health_diagnose(
    podman: tauri::State<'_, PodmanState>,
) -> Result<easytidy_core::storage_health::DiagnoseReport, String> {
    let p = podman.get().await.map_err(|e| e.to_string())?;
    let report = easytidy_core::storage_health::diagnose(&p)
        .await
        .map_err(|e| e.to_string())?;
    podman.return_podman(p).await;
    Ok(report)
}

/// 一键修复（备份 → 合并写 → 验证；仅 Recommended 结论可执行）
#[tauri::command]
pub async fn storage_health_fix(
    podman: tauri::State<'_, PodmanState>,
) -> Result<easytidy_core::storage_health::ApplyReport, String> {
    let p = podman.get().await.map_err(|e| e.to_string())?;
    let report = easytidy_core::storage_health::apply_fix(&p)
        .await
        .map_err(|e| e.to_string())?;
    podman.return_podman(p).await;
    Ok(report)
}

/// 横幅忽略标记是否已置（存在即 dismissed）
#[tauri::command]
pub fn storage_health_dismissed() -> Result<bool, String> {
    Ok(dismissed_path()?.exists())
}

/// 置忽略标记（横幅不再出现；「重新检查」入口保留）
#[tauri::command]
pub fn storage_health_dismiss() -> Result<(), String> {
    let path = dismissed_path()?;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    std::fs::write(&path, format!("{{\"dismissed_at\": {ts}}}\n"))
        .map_err(|e| format!("写入忽略标记失败：{e}"))
}

/// 清除忽略标记（「重新检查存储健康」入口）
#[tauri::command]
pub fn storage_health_reset_dismiss() -> Result<(), String> {
    let path = dismissed_path()?;
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| format!("清除忽略标记失败：{e}"))?;
    }
    Ok(())
}
