// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/

use std::fs;
use std::path::Path;

/// GUI 应用模式（main.rs 传入）
#[derive(Debug, Clone, PartialEq)]
pub enum AppMode {
    /// 中心化管理模式（管理所有容器）
    Centralized,
    /// 单容器管理模式
    Container { name: String },
}

/// 应用 NVIDIA DMABUF 渲染器规避措施
///
/// 根据 docs/10-m0-spike-report.md 的结论：
/// - NVIDIA 宿主需设置 WEBKIT_DISABLE_DMABUF_RENDERER=1
/// - 参考 clash-verge-rev 的 workarounds.rs 实现
fn apply_nvidia_workaround() {
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

/// 切换 webview devtools（调试用；前端按钮触发）
#[tauri::command]
fn toggle_devtools(webview: tauri::WebviewWindow) {
    if webview.is_devtools_open() {
        webview.close_devtools();
    } else {
        webview.open_devtools();
    }
}

/// 获取应用模式（前端调用）
#[tauri::command]
fn get_app_mode(mode: tauri::State<AppMode>) -> String {
    match mode.inner() {
        AppMode::Centralized => "centralized".to_string(),
        AppMode::Container { name } => format!("container:{}", name),
    }
}

pub fn run(mode: AppMode, _config_file: Option<String>) {
    // 第一步：应用 NVIDIA 规避措施（在 Tauri 初始化之前）
    apply_nvidia_workaround();

    // 第二步：启动 Tauri 应用
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(mode)
        .invoke_handler(tauri::generate_handler![toggle_devtools, get_app_mode])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
