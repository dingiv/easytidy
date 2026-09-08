//! 桌面快捷方式管理（**纯宿主侧**）：扫描 / 移除 / 图标重编 / 宿主文件预览。
//!
//! 全部操作只触碰宿主文件（~/.local/share/applications、桌面目录、
//! ~/.easytidy/icons），不涉及容器内任何处理。生成/加工逻辑在
//! `core::desktop`（与导出/创建链路共用同一批函数与命名）。

use easytidy_core::desktop::DesktopIconEntry;
use tracing::info;

/// 扫描宿主侧全部 easytidy 桌面快捷方式（菜单 + 桌面副本，按 identity 合并）
#[tauri::command]
pub fn desktop_icons_scan() -> Result<Vec<DesktopIconEntry>, String> {
    easytidy_core::desktop::scan_desktop_icons().map_err(|e| e.to_string())
}

/// 移除一个 identity 的桌面快捷方式（全部文件：菜单 + 桌面副本）。
/// 返回实际删除的路径列表。
#[tauri::command]
pub fn desktop_icons_remove(
    kind: String,
    container: String,
    app_id: Option<String>,
) -> Result<Vec<String>, String> {
    let removed: Vec<std::path::PathBuf> = match kind.as_str() {
        "entry" => easytidy_core::desktop::remove_entry_desktops(&container),
        _ => {
            let app_id = app_id.ok_or_else(|| "应用 id 缺失".to_string())?;
            let path = easytidy_core::desktop::remove_passthrough(&container, &app_id)
                .map_err(|e| e.to_string())?;
            vec![path]
        }
    };
    Ok(removed
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect())
}

/// 选择宿主图片作为图标源（原生对话框 → 拷入 icons 目录保留原图 →
/// 返回宿主路径；重编时由 `reedit_desktop_icon` 统一品牌加工）。
#[tauri::command]
pub async fn desktop_icons_pick() -> Result<String, String> {
    use std::io::Read;

    // 原生对话框阻塞主线程：spawn_blocking 避免卡 async runtime
    let picked = tokio::task::spawn_blocking(|| {
        rfd::FileDialog::new()
            .add_filter("图片", &["png", "jpg", "jpeg", "svg", "ico", "webp", "gif"])
            .pick_file()
            .map(|p| p.to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| format!("文件选择对话框失败：{e}"))?
    .ok_or_else(|| "已取消".to_string())?;

    let mut bytes = Vec::new();
    std::fs::File::open(&picked)
        .and_then(|mut f| f.read_to_end(&mut bytes))
        .map_err(|e| format!("读取图片失败：{e}"))?;

    let icons_dir = easytidy_core::appdata::icons_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&icons_dir).map_err(|e| e.to_string())?;
    let ext = std::path::Path::new(&picked)
        .extension()
        .and_then(|e| e.to_str())
        .filter(|e| !e.is_empty())
        .unwrap_or("png");
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dest = icons_dir.join(format!("easytidy-src-{ts}.{ext}"));
    std::fs::write(&dest, &bytes).map_err(|e| format!("写入图标源失败：{e}"))?;
    info!("桌面快捷方式图标源已选择：{picked} → {}", dest.display());
    Ok(dest.to_string_lossy().into_owned())
}

/// 图标重编（纯宿主侧）：所选宿主图片经品牌工具加工 → 写入标准图标文件 →
/// 更新该 identity 全部 .desktop 的 Icon=。返回加工后的图标文件路径。
#[tauri::command]
pub fn desktop_icons_set_icon(
    kind: String,
    container: String,
    app_id: Option<String>,
    source: String,
) -> Result<String, String> {
    easytidy_core::desktop::reedit_desktop_icon(&kind, &container, app_id.as_deref(), &source)
        .map_err(|e| e.to_string())
}

/// 宿主文件 → base64（图标预览用；调用方保证路径来自扫描结果）
#[tauri::command]
pub fn host_file_b64(path: String) -> Result<String, String> {
    use base64::Engine as _;
    let data = std::fs::read(&path).map_err(|e| format!("读取文件失败（{path}）：{e}"))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(&data))
}
