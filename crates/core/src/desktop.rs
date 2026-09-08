//! .desktop 文件生成（宿主导出应用图标）。
//!
//! 功能：
//! - 生成容器专用的 .desktop 文件（Exec = `<cli> open --container <name>`，
//!   经 CLI 垫片确保容器运行/等待 server 后再拉起 GUI——桌面快捷方式统一入口）
//! - 安装到 $XDG_DATA_HOME/applications/
//! - 支持自定义图标（使用发行版 logo 或默认）

use std::path::{Path, PathBuf};
use std::fs;
use crate::error::{Error, Result};

/// .desktop 文件模板。
const DESKTOP_ENTRY_TEMPLATE: &str = r#"[Desktop Entry]
Version=1.0
Type=Application
Name={title}
Exec={exec}
Icon={icon}
Terminal=false
Categories=System;ContainerManagement;
X-easytidy-container={name}
"#;

/// 清理 .desktop INI 值（`Key=value` 单行结构专用）：换行 / 控制字符会让
/// 一行断裂成多行 → 破坏 INI 解析。换行替换为空格、控制字符丢弃、连续空白
/// 压缩、首尾 trim。**仅用于 INI 值字段**（Name/Comment/Keywords/Icon 等）；
/// `Exec` 是 shell 命令行（含空格合法），不走此函数。
fn sanitize_ini_value(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .filter_map(|c| match c {
            '\n' | '\r' => Some(' '),
            c if (c as u32) < 0x20 => None, // 丢弃其他控制字符（\0-\x1f）
            c => Some(c),
        })
        .collect();
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 生成 .desktop 文件内容。
///
/// 参数：
/// - name: 容器名
/// - title: 显示标题（默认 "easytidy <name>"）
/// - icon: 图标路径（默认使用发行版 logo 或 easytidy 图标）
/// - cli_path: 宿主 easytidy CLI 绝对路径（Exec/TryExec）
///
/// 返回 .desktop 文件内容字符串。
pub fn generate_desktop_entry(
    name: &str,
    title: Option<&str>,
    icon: Option<&str>,
    cli_path: &str,
) -> String {
    let default_title = format!("easytidy {}", name);
    let title = sanitize_ini_value(title.unwrap_or(&default_title));
    let icon = sanitize_ini_value(icon.unwrap_or("easytidy-container"));
    let exec = format!("{} open --container {}", cli_path, name);

    DESKTOP_ENTRY_TEMPLATE
        .replace("{title}", &title)
        .replace("{exec}", &exec)
        .replace("{icon}", &icon)
        .replace("{name}", name)
}

/// 安装 .desktop 文件到用户应用目录。
///
/// 参数：
/// - name: 容器名
/// - title: 显示标题
/// - icon: 图标路径
/// - cli_path: 宿主 easytidy CLI 绝对路径
///
/// 返回安装后的 .desktop 文件路径。
pub fn install_desktop_entry(
    name: &str,
    title: Option<&str>,
    icon: Option<&str>,
    cli_path: &str,
) -> Result<PathBuf> {
    let content = generate_desktop_entry(name, title, icon, cli_path);

    // 确定目标路径
    let data_home = dirs::data_local_dir()
        .ok_or_else(|| Error::Config("无法确定 XDG_DATA_HOME".to_string()))?;

    let applications_dir = data_home.join("applications");
    fs::create_dir_all(&applications_dir)
        .map_err(|e| Error::Config(format!("创建 applications 目录失败：{e}")))?;

    let desktop_path = applications_dir.join(format!("easytidy-{}.desktop", name));

    // 写入文件
    fs::write(&desktop_path, content)
        .map_err(|e| Error::Config(format!("写入 .desktop 文件失败：{e}")))?;

    tracing::info!("安装 .desktop 文件：{:?}", desktop_path);

    Ok(desktop_path)
}

/// 卸载 .desktop 文件。
pub fn uninstall_desktop_entry(name: &str) -> Result<()> {
    let data_home = dirs::data_local_dir()
        .ok_or_else(|| Error::Config("无法确定 XDG_DATA_HOME".to_string()))?;

    let desktop_path = data_home
        .join("applications")
        .join(format!("easytidy-{}.desktop", name));

    if desktop_path.exists() {
        fs::remove_file(&desktop_path)
            .map_err(|e| Error::Config(format!("删除 .desktop 文件失败：{e}")))?;

        tracing::info!("卸载 .desktop 文件：{:?}", desktop_path);
    }

    Ok(())
}

// ============================================================================
// passthrough 导出（宿主侧）：把容器内应用导出为宿主 .desktop，
// Exec = <cli> --container <name> run -- <exec>，经 server socket 拉起应用。
// 风格对齐 distrobox 生成的 .desktop（TryExec / GenericName / Keywords /
// Actions=Remove 等——TryExec 缺失时部分桌面环境不显示入口，实测）。
// ============================================================================

/// 宿主 applications 目录（~/.local/share/applications/）
pub fn passthrough_dir() -> Result<PathBuf> {
    let data_home = dirs::data_local_dir()
        .ok_or_else(|| Error::Config("无法确定 XDG_DATA_HOME".to_string()))?;
    Ok(data_home.join("applications"))
}

/// 宿主图标缓存目录（~/.easytidy/icons/，集中管理；不存在则创建）
pub fn passthrough_icon_dir() -> Result<PathBuf> {
    crate::appdata::icons_dir()
}

/// 宿主桌面目录（`xdg-user-dir DESKTOP` 动态获取，兼容中文系统"桌面"；
/// 失败回退 ~/Desktop / ~/桌面 中存在者）。桌面路径可能不存在
/// （GNOME 默认无桌面图标扩展时用户仍可能有目录）。
pub fn desktop_dir() -> Option<PathBuf> {
    // xdg-user-dir 动态获取（Ubuntu/Debian 自带 xdg-utils）
    if let Ok(out) = std::process::Command::new("xdg-user-dir")
        .arg("DESKTOP")
        .output()
    {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !s.is_empty() && Path::new(&s).is_dir() {
                return Some(PathBuf::from(s));
            }
        }
    }
    let home = dirs::home_dir()?;
    [home.join("Desktop"), home.join("桌面")]
        .into_iter()
        .find(|cand| cand.is_dir())
}

/// XDG 标准用户资源目录（宿主侧探测结果 + 容器侧标准名）。
///
/// GUI 快捷映射「宿主常用目录 → 容器 `${HOME}/<folder>`」的数据源：
/// - `host_path`：宿主真实绝对路径（GUI 展示 / Tooltip）
/// - `host_path_expr`：可移植的 `${HOME}/<rel>` 表达（GUI 写入挂载行——避免把
///   `/home/<user>` 硬编进配置,换用户 / 换 home 仍可用；运行时经
///   [`crate::pathvars`] 两侧分别展开）
/// - 容器侧路径由 GUI 拼 `${HOME}/<folder>`（运行时展开为容器用户 home）
#[derive(Debug, Clone, serde::Serialize)]
pub struct XdgUserDir {
    /// 稳定标识（文件夹名小写，如 `downloads`）
    pub key: String,
    /// 容器侧标准文件夹名（XDG 标准，如 `Downloads`；GUI 拼 `${HOME}/<folder>`）
    pub folder: String,
    /// 宿主侧探测到的真实绝对路径（展示用）
    pub host_path: String,
    /// 宿主侧可移植表达（家目录下 → `${HOME}/<rel>`；非家目录自定义绝对路径 → 原样）。
    /// GUI 写入挂载行的 `host_path` 用它,而非 `host_path` 绝对路径。
    pub host_path_expr: String,
    /// 宿主路径是否为目录（false → GUI 置灰提示）
    pub exists: bool,
}

/// 探测宿主常用用户资源目录（下载/文档/桌面/图片/音乐/视频）。
///
/// 经 `xdg-user-dir <KEY>` 动态获取真实路径（兼容中文系统本地化目录名 /
/// 用户自定义 XDG 位置），探测退化或失败回退 `$HOME/<folder>`（XDG 默认位置）。
/// `exists` 标记宿主路径是否存在——不存在时 GUI 仍可添加（用户可后建目录），
/// 但按钮置灰提示。
pub fn host_user_resource_dirs() -> Vec<XdgUserDir> {
    // (xdg-user-dir 键, 容器侧标准文件夹名)
    const DIRS: [(&str, &str); 6] = [
        ("DOWNLOAD", "Downloads"),
        ("DOCUMENTS", "Documents"),
        ("DESKTOP", "Desktop"),
        ("MUSIC", "Music"),
        ("PICTURES", "Pictures"),
        ("VIDEO", "Videos"),
    ];
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    DIRS
        .into_iter()
        .map(|(xdg_key, folder)| {
            let host_path = resolve_host_resource_dir(xdg_user_dir(xdg_key), &home, folder);
            let host_path_str = host_path.to_string_lossy().into_owned();
            XdgUserDir {
                key: folder.to_lowercase(),
                folder: folder.to_string(),
                exists: host_path.is_dir(),
                host_path: host_path_str.clone(),
                host_path_expr: home_expr(&host_path, &home),
            }
        })
        .collect()
}

/// 将宿主绝对路径改写为可移植的 `${HOME}/<rel>` 表达。
///
/// 路径在 home 下 → `${HOME}/<rel>`（跨用户可移植,不硬编 `/home/<user>`）；
/// 非 home 下的自定义绝对路径（如 `/data/Dl`）→ 原样保留（无法用 `${HOME}` 表达）。
/// 前缀匹配须落在**路径组件边界**（余量为空或以 `/` 开头）,避免 `/home/divx`
/// 被 `/home/div` 误吞。
fn home_expr(path: &Path, home: &Path) -> String {
    let home_norm = home.to_string_lossy().trim_end_matches('/').to_string();
    let path_norm = path.to_string_lossy().into_owned();
    let is_under = path_norm.starts_with(&home_norm)
        && {
            let rest = &path_norm[home_norm.len()..];
            rest.is_empty() || rest.starts_with('/')
        };
    if is_under {
        let rel = &path_norm[home_norm.len()..];
        // rel 形如 "" / "/Videos" / "/视频"
        format!("${{HOME}}{rel}")
    } else {
        path_norm
    }
}

/// 解析单个宿主资源目录的真实路径。
///
/// 优先用 `xdg-user-dir` 探测结果（支持中文本地化目录名 / 用户自定义 XDG
/// 位置）；但 `xdg-user-dir` 在目录**未配置或配置畸形**（如 `XDG_TEMPLATES_DIR=
/// "$HOME/"` 这种值为裸 `$HOME/` 的行）时会**退化为家目录本身**——此时若照搬
/// 会把整个 home 挂进容器（危险且错误）。故：探测结果为空或 = 家目录 → 回退
/// 标准位置 `$HOME/<folder>`。
fn resolve_host_resource_dir(xdg: Option<PathBuf>, home: &Path, folder: &str) -> PathBuf {
    match xdg {
        Some(p) if !is_bare_home(&p, home) => p,
        _ => home.join(folder),
    }
}

/// 路径是否为「家目录本身」（忽略尾随 `/`）——`xdg-user-dir` 退化的标志。
fn is_bare_home(p: &Path, home: &Path) -> bool {
    let norm = |b: &Path| b.to_string_lossy().trim_end_matches('/').to_string();
    !norm(p).is_empty() && norm(p) == norm(home)
}

/// `xdg-user-dir <KEY>`：动态获取宿主用户目录真实路径（兼容本地化命名）。
/// 命令缺失 / 失败 / 空输出 → `None`（调用方回退 `$HOME/<folder>`）。
fn xdg_user_dir(key: &str) -> Option<PathBuf> {
    let out = std::process::Command::new("xdg-user-dir")
        .arg(key)
        .output()
        .ok()?;
    if out.status.success() {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !s.is_empty() {
            return Some(PathBuf::from(s));
        }
    }
    None
}

#[cfg(test)]
mod resource_dir_tests {
    use super::*;

    #[test]
    fn bare_home_degenerates_to_standard_folder() {
        // xdg-user-dir 退化为 home 本身 → 回退 $HOME/<folder>（不挂整 home）
        let home = PathBuf::from("/home/div");
        assert_eq!(
            resolve_host_resource_dir(Some(home.clone()), &home, "Videos"),
            PathBuf::from("/home/div/Videos")
        );
    }

    #[test]
    fn bare_home_with_trailing_slash() {
        let home = PathBuf::from("/home/div");
        assert_eq!(
            resolve_host_resource_dir(Some(PathBuf::from("/home/div/")), &home, "Videos"),
            PathBuf::from("/home/div/Videos")
        );
    }

    #[test]
    fn empty_result_falls_back() {
        let home = PathBuf::from("/home/div");
        assert_eq!(
            resolve_host_resource_dir(None, &home, "Downloads"),
            PathBuf::from("/home/div/Downloads")
        );
    }

    #[test]
    fn real_subdir_is_respected() {
        let home = PathBuf::from("/home/div");
        assert_eq!(
            resolve_host_resource_dir(Some(PathBuf::from("/home/div/Downloads")), &home, "Downloads"),
            PathBuf::from("/home/div/Downloads")
        );
    }

    #[test]
    fn localized_or_custom_dir_is_respected() {
        // 中文本地化 / 用户自定义位置（非裸 home）→ 照搬
        let home = PathBuf::from("/home/div");
        assert_eq!(
            resolve_host_resource_dir(Some(PathBuf::from("/home/div/文档")), &home, "Documents"),
            PathBuf::from("/home/div/文档")
        );
        assert_eq!(
            resolve_host_resource_dir(Some(PathBuf::from("/data/Dl")), &home, "Downloads"),
            PathBuf::from("/data/Dl")
        );
    }

    #[test]
    fn is_bare_home_detection() {
        let home = PathBuf::from("/home/div");
        assert!(is_bare_home(&home, &home));
        assert!(is_bare_home(&PathBuf::from("/home/div/"), &home));
        assert!(!is_bare_home(&PathBuf::from("/home/div/Videos"), &home));
        assert!(!is_bare_home(&PathBuf::from("/"), &home));
        assert!(!is_bare_home(&PathBuf::from(""), &home));
    }

    #[test]
    fn home_expr_under_home() {
        let home = PathBuf::from("/home/div");
        assert_eq!(home_expr(&PathBuf::from("/home/div/Videos"), &home), "${HOME}/Videos");
        assert_eq!(home_expr(&PathBuf::from("/home/div/视频"), &home), "${HOME}/视频");
    }

    #[test]
    fn home_expr_outside_home_kept_absolute() {
        // 非 home 下的自定义绝对路径 → 原样（无法用 ${HOME} 表达）
        let home = PathBuf::from("/home/div");
        assert_eq!(home_expr(&PathBuf::from("/data/Dl"), &home), "/data/Dl");
        assert_eq!(home_expr(&PathBuf::from("/"), &home), "/");
    }

    #[test]
    fn home_expr_prefix_not_misread() {
        // /home/divx 不应被 /home/div 前缀误吞（ends_with 边界）
        let home = PathBuf::from("/home/div");
        assert_eq!(home_expr(&PathBuf::from("/home/divx/Videos"), &home), "/home/divx/Videos");
    }
}

/// 标记 GNOME 桌面 .desktop 为已信任（允许双击启动；仅桌面路径需要，
/// 应用菜单无需。非 GNOME 环境 gio 缺失时静默忽略）。
pub fn mark_desktop_trusted(path: &Path) {
    let _ = std::process::Command::new("gio")
        .args(["set", &path.to_string_lossy(), "metadata::trusted", "true"])
        .status();
}

/// passthrough 导出参数（宿主侧生成薄指针 .desktop 的全部输入）
pub struct PassthroughSpec {
    pub container: String,
    pub app_name: String,
    pub comment: Option<String>,
    pub categories: Option<String>,
    /// 稳定应用 id（server 登记表：`pt-<hash>` / 自定义 `custom:<name>`）——
    /// Exec 只引用 id，启动决策（谁/如何）由容器内 server 负责
    pub app_id: String,
    /// 宿主图标绝对路径（已搬运到本地并验证存在；空则不写 Icon 字段）
    pub icon: Option<String>,
    /// 容器内 .desktop 路径（显示/列表匹配用，`X-easytidy-app`）
    pub desktop_file: String,
    /// 宿主 easytidy CLI 绝对路径（Exec/TryExec）
    pub cli_path: String,
    /// StartupNotify（容器内 .desktop）
    pub startup_notify: bool,
    /// StartupWMClass（容器内 .desktop；Wayland 窗口匹配用）
    pub startup_wm_class: Option<String>,
}

/// 生成 distrobox 风格 .desktop 内容（TryExec/GenericName/Keywords/
/// Actions=Remove——TryExec 缺失时部分桌面环境不显示入口，实测）。
///
/// **薄指针**：Exec 只含 `easytidy launch <id> --container <n>`——不嵌命令
/// 行，容器内应用变更（升级/改参数）时快捷方式自动跟随（id 不变）。
fn generate_passthrough_content(spec: &PassthroughSpec) -> String {
    // INI 值清理：app_name / comment / categories / icon / wm / desktop_file 来自
    // 容器内 .desktop 或用户，可能含换行 / 控制字符 → 破坏单行 Key=value 结构。
    // container（podman 字符集限制 [a-zA-Z0-9][a-zA-Z0-9_.-]*）与 cli_path /
    // app_id（哈希 / `custom:<name>`，无空格）安全，不处理。
    let app_name = sanitize_ini_value(&spec.app_name);
    let comment = sanitize_ini_value(spec.comment.as_deref().unwrap_or("easytidy passthrough"));
    let categories = sanitize_ini_value(spec.categories.as_deref().unwrap_or("Application;Utility;"));
    let icon = spec.icon.as_ref().map(|i| sanitize_ini_value(i));
    let wm = spec.startup_wm_class.as_ref().map(|w| sanitize_ini_value(w));
    let desktop_file = sanitize_ini_value(&spec.desktop_file);

    let mut content = format!(
        "[Desktop Entry]\n\
         Name={}\n\
         GenericName=easytidy {} - {}\n\
         Comment={comment}\n\
         Categories={categories}\n",
        app_name, spec.container, app_name,
    );
    // launch = 按引用启动：server 查登记表解析 exec 并自行 spawn（点火即走，
    // 点图标秒回；应用生命周期归 server，可经 apps.ps/logs 跟踪）。
    content.push_str(&format!(
        "Exec={} launch --id {} --container {}\n",
        spec.cli_path, spec.app_id, spec.container,
    ));
    if let Some(icon) = icon {
        content.push_str(&format!("Icon={icon}\n"));
    }
    if spec.startup_notify {
        content.push_str("StartupNotify=true\n");
    }
    if let Some(wm) = wm {
        content.push_str(&format!("StartupWMClass={wm}\n"));
    }
    content.push_str(&format!(
        "Keywords=easytidy;{};\n\
         NoDisplay=false\n\
         Terminal=false\n\
         TryExec={}\n\
         Type=Application\n\
         Actions=Remove;\n\
         \n\
         [Desktop Action Remove]\n\
         Name=Remove {} from system\n\
         Exec={} unexport --container {} --app-id {}\n",
        spec.container,
        spec.cli_path,
        app_name,
        spec.cli_path,
        spec.container,
        spec.app_id,
    ));
    content.push_str(&format!(
        "X-easytidy-pt=1\n\
         X-easytidy-container={}\n\
         X-easytidy-app-id={}\n\
         X-easytidy-app={}\n",
        spec.container, spec.app_id, desktop_file,
    ));
    content
}

/// easytidy 品牌图标（256×256 PNG，内嵌；与 GUI 应用图标/水印同源——
/// crates/gui/icons/easytidy256x256.png 的拷贝，core 无法跨 crate 引用）。
const EASYTIDY_ICON_PNG: &[u8] = include_bytes!("../assets/easytidy.png");

/// 确保 easytidy 品牌图标存在（~/.easytidy/icons/easytidy-gui.png）
/// 返回图标绝对路径；写入失败返回 None（不影响快捷方式导出）。
///
/// 历史遗留：旧版本写 SVG 线条图标（easytidy-gui.svg，v0.1 设计）——
/// 已导出的 .desktop 若仍指向它则图标失效/显示旧设计，顺手清理。
pub fn ensure_gui_icon() -> Option<String> {
    let dir = crate::appdata::icons_dir().ok()?;
    let path = dir.join("easytidy-gui.png");
    if !path.exists() && std::fs::write(&path, EASYTIDY_ICON_PNG).is_err() {
        return None;
    }
    // 清理旧版 SVG（同目录同名 .svg；新导出一律指向 PNG）
    let legacy_svg = dir.join("easytidy-gui.svg");
    if legacy_svg.exists() {
        let _ = std::fs::remove_file(&legacy_svg);
    }
    Some(path.to_string_lossy().into_owned())
}

/// 容器入口图标加工：用户配置的图标源（宿主路径）经内置品牌工具
/// [`crate::icon::compose_app_icon`]（渐变圆角边框 + 圆角内容 + 品牌水印）
/// 加工后写入 `icons_dir()/easytidy-gui-<container>.png`。
///
/// 返回加工后图标路径；未设置 / 源图读取失败 / 加工失败均返回 `None`
///（调用方回退内置品牌图标）。
pub fn process_container_icon(container: &str, icon_src: Option<&str>) -> Option<String> {
    let src = icon_src?.trim();
    if src.is_empty() {
        return None;
    }
    let data = std::fs::read(src).ok()?;
    let composed = crate::icon::compose_app_icon(&data, EASYTIDY_ICON_PNG).ok()?;
    let dir = crate::appdata::icons_dir().ok()?;
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("easytidy-gui-{container}.png"));
    std::fs::write(&path, &composed).ok()?;
    tracing::info!("容器入口图标加工：{container} ← {src} → {}", path.display());
    Some(path.to_string_lossy().into_owned())
}

/// GUI 入口 .desktop 内容（纯函数，可单测）。
///
/// Exec = `<cli> open --container <name>`——经 CLI 垫片确保容器运行、
/// 等待 server socket 就绪，再按配置 silent_boot 决定是否拉起 Worker GUI
/// （直接冷启动 380MB GUI 是点击慢的根源，垫片先行秒级保活）。
/// `icon`：加工后的容器图标路径（[`process_container_icon`]）；
/// 未设置回退内置品牌图标。
pub fn generate_gui_entry_content(container: &str, cli_path: &str, icon: Option<&str>) -> String {
    let mut content = format!(
        "[Desktop Entry]\n\
         Name=easytidy {container}\n\
         GenericName=Container GUI for {container}\n\
         Comment=Open {container} management interface\n\
         Categories=System;Utility;ContainerManagement;\n\
         Exec={cli_path} open --container {container}\n",
    );
    let icon = icon
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(ensure_gui_icon);
    if let Some(icon) = icon {
        content.push_str(&format!("Icon={icon}\n"));
    }
    content.push_str(&format!(
        "Keywords=easytidy;{container};\n\
         NoDisplay=false\n\
         Terminal=false\n\
         TryExec={cli_path}\n\
         Type=Application\n\
         X-easytidy-gui=1\n\
         X-easytidy-container={container}\n",
    ));
    content
}

/// 导出本容器的 GUI 管理界面桌面快捷方式（distrobox "进入容器"入口的
/// easytidy 变体：Exec 经 CLI 垫片拉起 Worker 管理窗口）。
///
/// - 应用菜单：~/.local/share/applications/easytidy-gui-<container>.desktop
/// - 桌面图标：桌面路径同名文件 + chmod +x + `gio metadata::trusted`
///   （GNOME 双击必需；`desktop_icon=true` 且桌面目录存在时）
/// - `cli_path`：宿主 easytidy CLI 绝对路径（Exec/TryExec；垫片入口）
/// - `icon`：加工后的容器图标路径（未设置回退内置品牌图标）
///
/// 返回应用菜单路径。
pub fn write_gui_entry(
    container: &str,
    cli_path: &str,
    desktop_icon: bool,
    icon: Option<&str>,
) -> Result<PathBuf> {
    let dir = passthrough_dir()?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| Error::Config(format!("创建 applications 目录失败：{e}")))?;

    let content = generate_gui_entry_content(container, cli_path, icon);

    let file_name = format!("easytidy-gui-{container}.desktop");
    let menu_path = dir.join(&file_name);
    write_desktop_file(&menu_path, &content)?;
    tracing::info!("GUI 入口导出（菜单）：{menu_path:?}");

    if desktop_icon {
        if let Some(dd) = desktop_dir() {
            let desktop_path = dd.join(&file_name);
            if write_desktop_file(&desktop_path, &content).is_ok() {
                mark_desktop_trusted(&desktop_path);
                tracing::info!("GUI 入口导出（桌面）：{desktop_path:?}");
            } else {
                tracing::warn!("桌面图标写入失败（跳过）：{desktop_path:?}");
            }
        }
    }

    Ok(menu_path)
}

/// 升级迁移：把旧格式 Exec 的 easytidy .desktop 重写为 CLI 垫片格式。
///
/// 背景：旧容器入口 `Exec=<gui> --container <n>`（冷启动重型 GUI）、
/// 旧 passthrough `Exec=<cli> run --container <n> -- <cmd>` 均不享受垫片
/// 收益（保活 + 等 socket 就绪 + 容器缺失友好报错）。旧格式升级后仍可
/// 工作（run 与 GUI 直连都没删），迁移是补齐而非防坏——在 GUI 启动时
/// 调用（存量文件只在显式导出时生成，不会自己更新）。
///
/// 幂等：Exec 已含 ` open --container ` 的跳过。按标记/文件名识别三类：
/// - `X-easytidy-gui=1`（容器入口）→ `Exec=<cli> open --container <n>`
/// - `X-easytidy-pt=1`（passthrough，容器名取 `X-easytidy-container=`，
///   应用命令 = 旧 Exec 中 ` -- ` 之后的全部）→ 仅 run→open 替换
/// - 遗留 `easytidy-<n>.desktop`（无标记）→ `Exec=<cli> open --container <n>`
///
/// 扫 applications 目录 + 桌面副本两处（委托 [`migrate_dir`]）。
pub fn migrate_shortcuts(cli_path: &str) -> usize {
    let mut total = 0;
    if let Ok(d) = passthrough_dir() {
        total += migrate_dir(&d, cli_path);
    }
    if let Some(d) = desktop_dir() {
        total += migrate_dir(&d, cli_path);
    }
    total
}

/// 迁移单个目录中的 easytidy .desktop（逐文件尽力而为，失败 warn 不中断）。
/// 返回重写的文件数。
pub fn migrate_dir(dir: &Path, cli_path: &str) -> usize {
    let mut rewritten = 0;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with("easytidy-") || !name.ends_with(".desktop") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        // None = 无需迁移（已是新格式 / 无容器标记 / 无 Exec 行）
        let Some(new_content) = migrate_one_content(&content, cli_path) else {
            continue;
        };
        if let Err(e) = write_desktop_file(&path, &new_content) {
            tracing::warn!("迁移 .desktop 失败（{name}）：{e}");
        } else {
            tracing::info!("迁移 .desktop 到新垫片格式：{name}");
            rewritten += 1;
        }
    }
    rewritten
}

/// 单文件迁移（纯函数）：返回新内容；`None` = 无需迁移。
///
/// 只重写**首个**（主）Exec 行与 TryExec 行（`open --container` 为幂等信号），
/// 其余行原样保留——passthrough 的 `[Desktop Action Remove]` 里也有一行
/// `Exec=... unexport ...`（CLI 命令，语义不变），不能被误替换。
fn migrate_one_content(content: &str, cli_path: &str) -> Option<String> {
    // 幂等：Exec 已是垫片格式
    if content.lines().any(|l| l.starts_with("Exec=") && l.contains(" open --container ")) {
        return None;
    }
    let container = content
        .lines()
        .find_map(|l| l.strip_prefix("X-easytidy-container="))
        .unwrap_or_default();
    // 容器名缺失（不应发生）时不迁移
    if container.is_empty() {
        return None;
    }
    let exec_line = content.lines().find(|l| l.starts_with("Exec="))?;
    let new_exec_line = if content.contains("X-easytidy-pt=1") {
        // passthrough：保留应用命令尾巴（旧 Exec 中首个 ` -- ` 之后的全部）
        let rest = exec_line.trim_end().split_once(" -- ").map(|(_, r)| r).unwrap_or_default();
        if rest.is_empty() {
            format!("Exec={cli_path} open --container {container}")
        } else {
            format!("Exec={cli_path} open --container {container} -- {rest}")
        }
    } else {
        // 容器入口（X-easytidy-gui=1）与遗留（easytidy-<n>.desktop）：
        // 无应用命令，整行替换
        format!("Exec={cli_path} open --container {container}")
    };

    let mut out = Vec::with_capacity(content.lines().count());
    let mut replaced_exec = false;
    for line in content.lines() {
        if !replaced_exec && line.starts_with("Exec=") {
            out.push(new_exec_line.clone());
            replaced_exec = true;
        } else if line.starts_with("TryExec=") {
            // TryExec 指向可执行文件本身：旧容器入口指 GUI 二进制（已失效语义），
            // 统一指向 CLI
            out.push(format!("TryExec={cli_path}"));
        } else {
            out.push(line.to_string());
        }
    }
    Some(out.join("\n") + "\n")
}

/// 容器内 .desktop basename → 宿主文件名（sanitize，防路径穿越）
/// 容器内应用 id → 宿主文件名（sanitize，防路径穿越；`custom:chrome` →
/// `custom-chrome`，`pt-<hash>` 天然安全）
fn passthrough_file_name(container: &str, app_id: &str) -> String {
    let safe: String = app_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    format!("easytidy-pt-{container}-{safe}.desktop")
}

/// 写入 .desktop 文件（+x 权限）。
fn write_desktop_file(path: &Path, content: &str) -> Result<()> {
    std::fs::write(path, content)
        .map_err(|e| Error::Config(format!("写入 .desktop 失败（{}）：{e}", path.display())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
    }
    Ok(())
}

/// 导出 passthrough（应用菜单 + 桌面图标）。
///
/// - 应用菜单：~/.local/share/applications/easytidy-pt-*.desktop（无需信任标记）
/// - 桌面图标：桌面路径同名文件 + chmod +x + `gio metadata::trusted`（GNOME
///   双击启动必需；`desktop_icon=true` 且桌面目录存在时）
///
/// 返回应用菜单路径。
pub fn write_passthrough(spec: &PassthroughSpec, desktop_icon: bool) -> Result<PathBuf> {
    let dir = passthrough_dir()?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| Error::Config(format!("创建 applications 目录失败：{e}")))?;

    let file_name = passthrough_file_name(&spec.container, &spec.app_id);
    let content = generate_passthrough_content(spec);
    let menu_path = dir.join(&file_name);
    write_desktop_file(&menu_path, &content)?;
    tracing::info!("passthrough 导出（菜单）：{menu_path:?}");

    // 桌面图标（可选；无桌面目录则跳过）
    if desktop_icon {
        if let Some(dd) = desktop_dir() {
            let desktop_path = dd.join(&file_name);
            if write_desktop_file(&desktop_path, &content).is_ok() {
                mark_desktop_trusted(&desktop_path);
                tracing::info!("passthrough 导出（桌面）：{desktop_path:?}");
            } else {
                tracing::warn!("桌面图标写入失败（跳过）：{desktop_path:?}");
            }
        }
    }

    Ok(menu_path)
}

/// 解析导出 .desktop 的字段值（按容器过滤）
fn parse_pt_value(content: &str, container: &str, key: &str) -> Option<String> {
    let in_container = content
        .lines()
        .find_map(|l| l.strip_prefix("X-easytidy-container="))
        .unwrap_or_default();
    if in_container != container {
        return None;
    }
    content
        .lines()
        .find_map(|l| l.strip_prefix(key))
        .map(|s| s.to_string())
}

/// 已导出的 passthrough 条目（含 .desktop 全文，供 GUI 展示）
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ExportedPassthrough {
    /// 容器内 .desktop 路径（X-easytidy-app 标记）
    pub desktop_file: String,
    /// .desktop 文件全文（read_to_string，非 UTF-8 以 lossy 兜底）
    pub content: String,
    /// 宿主文件路径
    pub path: PathBuf,
}

/// 枚举指定容器已导出的应用（返回容器内 .desktop 路径列表）
pub fn list_passthrough(container: &str) -> Result<Vec<String>> {
    Ok(list_passthrough_detailed(container)?
        .into_iter()
        .map(|e| e.desktop_file)
        .collect())
}

/// 枚举指定容器已导出的应用（含 .desktop 全文，GUI 展示用）
pub fn list_passthrough_detailed(container: &str) -> Result<Vec<ExportedPassthrough>> {
    let dir = passthrough_dir()?;
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(out);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with("easytidy-pt-") || !name.ends_with(".desktop") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        if !content.contains("X-easytidy-pt=1") {
            continue;
        }
        if let Some(app) = parse_pt_value(&content, container, "X-easytidy-app=") {
            if !app.is_empty() {
                out.push(ExportedPassthrough {
                    desktop_file: app,
                    content,
                    path,
                });
            }
        }
    }
    Ok(out)
}

/// 撤销某容器的 passthrough 导出（删除对应 .desktop；返回删除的路径）
/// 撤销某容器的 passthrough 导出（按应用 id 定位；兼容旧格式按
/// `X-easytidy-app` 匹配；删除对应 .desktop + 桌面副本；返回删除的路径）
pub fn remove_passthrough(container: &str, app_id: &str) -> Result<PathBuf> {
    let dir = passthrough_dir()?;
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Err(Error::Config("applications 目录不存在".to_string()));
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with("easytidy-pt-") || !name.ends_with(".desktop") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        if !content.contains("X-easytidy-pt=1") {
            continue;
        }
        if parse_pt_value(&content, container, "X-easytidy-container=").is_none() {
            continue;
        }
        // 新格式按 id 匹配；旧格式（无 app-id）按 X-easytidy-app 兼容
        let matched = parse_pt_value(&content, container, "X-easytidy-app-id=")
            .or_else(|| parse_pt_value(&content, container, "X-easytidy-app="))
            .as_deref()
            == Some(app_id);
        if matched {
            std::fs::remove_file(&path)
                .map_err(|e| Error::Config(format!("删除 .desktop 失败：{e}")))?;
            // 同步删除桌面副本（若存在）
            if let Some(dd) = desktop_dir() {
                if let Some(file_name) = path.file_name() {
                    let desktop_copy = dd.join(file_name);
                    if desktop_copy.exists() {
                        let _ = std::fs::remove_file(&desktop_copy);
                    }
                }
            }
            tracing::info!("passthrough 撤销：{app_id} → {path:?}");
            return Ok(path);
        }
    }
    Err(Error::Config(format!("未找到已导出的应用：{app_id}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_sanitize_ini_value() {
        // 换行 → 空格
        assert_eq!(sanitize_ini_value("a\nb"), "a b");
        assert_eq!(sanitize_ini_value("a\r\nb"), "a b");
        assert_eq!(sanitize_ini_value("a\rb"), "a b");
        // 控制字符丢弃
        assert_eq!(sanitize_ini_value("a\x00b"), "ab");
        assert_eq!(sanitize_ini_value("a\x1fb"), "ab");
        // 连续空白压缩 + 首尾 trim
        assert_eq!(sanitize_ini_value("  my  app  "), "my app");
        assert_eq!(sanitize_ini_value("\n\n"), "");
        // 正常值不变
        assert_eq!(sanitize_ini_value("Google Chrome"), "Google Chrome");
        assert_eq!(sanitize_ini_value("Application;Utility;"), "Application;Utility;");
    }

    #[test]
    fn test_generate_desktop_entry_sanitizes_title() {
        let content = generate_desktop_entry(
            "test-container",
            Some("Bad\nTitle"),
            None,
            "/usr/bin/easytidy",
        );
        // 换行被清理，Name 保持单行
        assert!(content.contains("Name=Bad Title"));
        assert!(!content.lines().any(|l| l.starts_with("Title=")));
    }

    #[test]
    fn test_generate_passthrough_content_sanitizes_app_name() {
        let spec = PassthroughSpec {
            container: "chrome".to_string(),
            app_name: "My\nApp".to_string(),
            comment: Some("line1\nline2".to_string()),
            categories: None,
            app_id: "pt-1234567890ab".to_string(),
            icon: None,
            desktop_file: "/usr/share/applications/myapp.desktop".to_string(),
            cli_path: "/usr/bin/easytidy".to_string(),
            startup_notify: false,
            startup_wm_class: None,
        };
        let content = generate_passthrough_content(&spec);
        assert!(content.contains("Name=My App"));
        assert!(content.contains("Comment=line1 line2"));
        // 无断裂行（line2 不成为新行开头）
        assert!(!content.lines().any(|l| l.starts_with("line2")));
    }

    #[test]
    fn test_generate_desktop_entry() {
        let content = generate_desktop_entry(
            "test-container",
            Some("Test Container"),
            Some("test-icon"),
            "/usr/bin/easytidy",
        );

        assert!(content.contains("Name=Test Container"));
        // 垫片格式：CLI 子命令级 --container（放顶层会 clap 报错，存量 bug 已修）
        assert!(content.contains("Exec=/usr/bin/easytidy open --container test-container"));
        assert!(content.contains("Icon=test-icon"));
        assert!(content.contains("X-easytidy-container=test-container"));
    }

    #[test]
    fn test_generate_desktop_entry_defaults() {
        let content = generate_desktop_entry("test", None, None, "/usr/bin/easytidy");

        assert!(content.contains("Name=easytidy test"));
        assert!(content.contains("Icon=easytidy-container"));
    }

    #[test]
    fn test_install_desktop_entry() {
        let temp_dir = TempDir::new().unwrap();

        // 模拟 XDG_DATA_HOME
        let target = temp_dir.path().join("applications/easytidy-test.desktop");

        // 手动创建目录并写入
        let content = generate_desktop_entry("test", None, None, "/usr/bin/easytidy");
        fs::create_dir_all(temp_dir.path().join("applications")).unwrap();
        fs::write(&target, content).unwrap();

        assert!(target.exists());
        let loaded = fs::read_to_string(&target).unwrap();
        assert!(loaded.contains("open --container test"));
    }

    #[test]
    fn test_generate_gui_entry_content_open_format() {
        let content = generate_gui_entry_content("chrome", "/usr/bin/easytidy", None);

        assert!(content.contains("Exec=/usr/bin/easytidy open --container chrome"));
        assert!(content.contains("TryExec=/usr/bin/easytidy"));
        // 迁移识别标记
        assert!(content.contains("X-easytidy-gui=1"));
        assert!(content.contains("X-easytidy-container=chrome"));
        // Exec 不再直连 GUI 二进制（X-easytidy-gui 标记含该子串，须按行判定）
        let exec_line = content.lines().find(|l| l.starts_with("Exec=")).unwrap();
        assert!(!exec_line.contains("easytidy-gui"));
    }

    #[test]
    fn test_process_container_icon_composes_and_writes() {
        use image::RgbaImage;

        // 2×2 红色源图（内存生成，不依赖外部资源）
        let img = RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]));
        let mut bytes = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src.png");
        fs::write(&src, &bytes).unwrap();

        let out = process_container_icon("e2e-icon-test", Some(src.to_str().unwrap()))
            .expect("源图存在且可加工，应返回输出路径");
        let out_path = std::path::PathBuf::from(&out);
        assert!(out_path.exists());
        assert!(out.ends_with("easytidy-gui-e2e-icon-test.png"));
        // 加工输出 = 256×256 有效 PNG（PNG 头：16-19 宽 / 20-23 高，big-endian）
        let data = fs::read(&out_path).unwrap();
        assert_eq!(&data[12..16], b"IHDR", "应为 PNG（IHDR 块）");
        let w = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
        let h = u32::from_be_bytes([data[20], data[21], data[22], data[23]]);
        assert_eq!((w, h), (256, 256));
    }

    #[test]
    fn test_process_container_icon_none_on_missing() {
        assert_eq!(process_container_icon("x", None), None);
        assert_eq!(process_container_icon("x", Some("")), None);
        assert_eq!(process_container_icon("x", Some("/nonexistent/icon.png")), None);
    }

    #[test]
    fn test_generate_passthrough_content_thin_exec() {
        let spec = PassthroughSpec {
            container: "chrome".to_string(),
            app_name: "Google Chrome".to_string(),
            comment: None,
            categories: None,
            app_id: "pt-abcdef123456".to_string(),
            icon: None,
            desktop_file: "/usr/share/applications/google-chrome.desktop".to_string(),
            cli_path: "/usr/bin/easytidy".to_string(),
            startup_notify: false,
            startup_wm_class: None,
        };
        let content = generate_passthrough_content(&spec);

        // 薄指针：Exec 只含 launch --id <id> --container，不嵌命令行
        assert!(content.contains("Exec=/usr/bin/easytidy launch --id pt-abcdef123456 --container chrome"));
        assert!(!content.lines().any(|l| l.starts_with("Exec=") && l.contains("google-chrome")));
        // Remove action 的 unexport 行按 app-id
        assert!(content.contains("Exec=/usr/bin/easytidy unexport --container chrome --app-id pt-abcdef123456"));
        // 标记：id + 旧格式 app 并存（列表匹配用）
        assert!(content.contains("X-easytidy-pt=1"));
        assert!(content.contains("X-easytidy-app-id=pt-abcdef123456"));
        assert!(content.contains("X-easytidy-app=/usr/share/applications/google-chrome.desktop"));
    }

    #[test]
    fn test_migrate_one_content_gui_entry() {
        let old = "[Desktop Entry]\nName=easytidy chrome\nExec=/path/gui --container chrome\nTryExec=/path/gui\nX-easytidy-gui=1\nX-easytidy-container=chrome\n";
        let new = migrate_one_content(old, "/usr/bin/easytidy").unwrap();
        assert!(new.contains("Exec=/usr/bin/easytidy open --container chrome"));
        assert!(new.contains("TryExec=/usr/bin/easytidy"));
        // 幂等：新内容不再迁移
        assert!(migrate_one_content(&new, "/usr/bin/easytidy").is_none());
    }

    #[test]
    fn test_migrate_one_content_passthrough_keeps_remove_action() {
        let old = "[Desktop Entry]\nName=App\nExec=/usr/bin/easytidy run --container c -- google-chrome\nTryExec=/usr/bin/easytidy\nX-easytidy-pt=1\nX-easytidy-container=c\nX-easytidy-app=app.desktop\n[Desktop Action Remove]\nName=Remove\nExec=/usr/bin/easytidy unexport --container c --desktop-file app.desktop\n";
        let new = migrate_one_content(old, "/usr/bin/easytidy").unwrap();
        // 主 Exec 改 open + 命令尾巴保留
        assert!(new.contains("Exec=/usr/bin/easytidy open --container c -- google-chrome"));
        // Remove action 的 unexport 行不被误替换
        assert!(new.contains("Exec=/usr/bin/easytidy unexport --container c --desktop-file app.desktop"));
        // 恰好一行 open Exec
        assert_eq!(new.lines().filter(|l| l.contains("open --container")).count(), 1);
    }

    #[test]
    fn test_migrate_one_content_missing_container_noop() {
        // 无 X-easytidy-container 标记 → 不迁移
        assert!(migrate_one_content(
            "[Desktop Entry]\nExec=whatever\n",
            "/usr/bin/easytidy"
        )
        .is_none());
    }

    #[test]
    fn test_migrate_dir_idempotent() {
        let tmp = TempDir::new().unwrap();
        let apps = tmp.path().join("applications");
        fs::create_dir_all(&apps).unwrap();

        // 旧格式容器入口
        let gui_file = apps.join("easytidy-gui-chrome.desktop");
        fs::write(
            &gui_file,
            "[Desktop Entry]\nExec=/path/gui --container chrome\nTryExec=/path/gui\nX-easytidy-gui=1\nX-easytidy-container=chrome\n",
        )
        .unwrap();
        // 旧格式 passthrough
        let pt_file = apps.join("easytidy-pt-chrome-google-chrome.desktop");
        fs::write(
            &pt_file,
            "[Desktop Entry]\nExec=/usr/bin/easytidy run --container chrome -- google-chrome\nX-easytidy-pt=1\nX-easytidy-container=chrome\n",
        )
        .unwrap();
        // 无关文件应被跳过
        let other = apps.join("nautilus.desktop");
        fs::write(&other, "[Desktop Entry]\nExec=nautilus\n").unwrap();

        let n1 = migrate_dir(&apps, "/usr/bin/easytidy");
        assert_eq!(n1, 2, "首次应重写 2 个 easytidy 文件");
        let gui = fs::read_to_string(&gui_file).unwrap();
        assert!(gui.contains("Exec=/usr/bin/easytidy open --container chrome"));
        let pt = fs::read_to_string(&pt_file).unwrap();
        assert!(pt.contains("open --container chrome -- google-chrome"));
        // 无关文件未被改动
        assert_eq!(fs::read_to_string(&other).unwrap(), "[Desktop Entry]\nExec=nautilus\n");

        // 幂等：二次调用零重写
        let n2 = migrate_dir(&apps, "/usr/bin/easytidy");
        assert_eq!(n2, 0, "已是新格式时不应再重写");
    }
}
