//! 桌面应用命令：枚举容器内 .desktop 应用。

use serde::{Deserialize, Serialize};

use easytidy_protocol::ops::{AppsList, AppsListResp};

use crate::commands::socket::send_json_request;
use crate::state::GuiSession;

// ============================================================================
// 桌面应用命令
// ============================================================================

/// 应用信息（前端）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppInfoFrontend {
    /// 稳定应用 id（server 登记表：`pt-<hash>`；导出/启动/收藏引用）
    pub id: String,
    pub name: String,
    pub icon_path: Option<String>,
    pub exec: String,
    pub comment: Option<String>,
    pub desktop_file: String,
    pub categories: Option<String>,
    pub startup_notify: bool,
    pub startup_wm_class: Option<String>,
}

/// 列出桌面应用
#[tauri::command]
pub async fn apps_list(
    session: tauri::State<'_, Option<GuiSession>>,
) -> Result<Vec<AppInfoFrontend>, String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;

    let resp = send_json_request(
        sess,
        "apps.list".to_string(),
        serde_json::to_value(AppsList).map_err(|e| e.to_string())?,
    )
    .await
    .map_err(|e| e.to_string())?;

    let list_resp: AppsListResp = serde_json::from_value(resp.payload)
        .map_err(|e| format!("解析 apps.list 响应失败：{}", e))?;

    let apps: Vec<AppInfoFrontend> = list_resp
        .apps
        .into_iter()
        .map(|a| AppInfoFrontend {
            id: a.id,
            name: a.name,
            icon_path: a.icon_path,
            exec: a.exec,
            comment: a.comment,
            desktop_file: a.desktop_file,
            categories: a.categories,
            startup_notify: a.startup_notify,
            startup_wm_class: a.startup_wm_class,
        })
        .collect();

    Ok(apps)
}
