//! 通用命令：NVIDIA 规避 / devtools 切换 / 应用模式。

use std::fs;
use std::path::Path;

use crate::state::{AppMode, AppModeResponse};

pub fn apply_nvidia_workaround() {
    // 如果环境变量已设置，跳过检测
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_some() {
        return;
    }

    if has_nvidia_gpu() {
        unsafe {
            std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
        }
        println!("Detected NVIDIA GPU, applied WEBKIT_DISABLE_DMABUF_RENDERER=1 workaround");
    }
}

/// 检测是否存在 NVIDIA GPU
///
/// 检测路径（参考 clash-verge-rev）：
/// - /proc/driver/nvidia/version
/// - /sys/module/nvidia*
/// - /sys/class/drm/card*/device/vendor == 0x10de
fn has_nvidia_gpu() -> bool {
    // 检查 NVIDIA 驱动文件
    if Path::new("/proc/driver/nvidia/version").exists()
        || Path::new("/sys/module/nvidia").exists()
        || Path::new("/sys/module/nvidia_drm").exists()
    {
        return true;
    }

    // 检查 DRM 设备供应商
    let Ok(entries) = fs::read_dir("/sys/class/drm") else {
        return false;
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("card") || name.contains('-') {
            continue;
        }

        let vendor_path = entry.path().join("device/vendor");
        let Ok(vendor) = fs::read_to_string(vendor_path) else {
            continue;
        };
        if vendor.trim().eq_ignore_ascii_case("0x10de") {
            return true;
        }
    }

    false
}

// ============================================================================
// 通用命令
// ============================================================================

/// 切换 webview devtools（调试用；前端按钮触发）
#[tauri::command]
pub fn toggle_devtools(webview: tauri::WebviewWindow) {
    if webview.is_devtools_open() {
        webview.close_devtools();
    } else {
        webview.open_devtools();
    }
}

/// 应用模式响应（结构体，避开 serde 单元变体→裸字符串的歧义）。
/// 前端按 result.mode 判断："centralized" | "per_container"。
#[tauri::command]
pub fn get_app_mode(mode: tauri::State<'_, AppMode>) -> Result<AppModeResponse, String> {
    Ok(match mode.inner() {
        AppMode::Centralized => AppModeResponse {
            mode: "centralized".to_string(),
            name: None,
        },
        AppMode::Container { name } => AppModeResponse {
            mode: "per_container".to_string(),
            name: Some(name.clone()),
        },
    })
}
