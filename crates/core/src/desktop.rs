//! .desktop 快捷方式生成与管理（宿主侧）。
//!
//! 功能：
//! - 容器入口 .desktop（`easytidy-gui-<name>.desktop`，Exec = `<cli> open
//!   --container <name>`，经 CLI 垫片确保容器运行/等待 server 后再拉起 GUI）
//!   ——应用菜单 + 桌面副本
//! - passthrough 应用 .desktop 导出（容器内 GUI 应用 → 宿主启动器）
//! - 桌面快捷方式扫描/移除/图标重编（纯宿主侧）

use std::path::{Path, PathBuf};
use std::fs;
use crate::error::{Error, Result};

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
    /// 桌面显示名（.desktop `Name=`）；空/None = 用 app_name
    pub display_name: Option<String>,
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
    // 桌面显示名：用户指定优先（空串视同未指定），否则回退 app_name
    let display = spec
        .display_name
        .as_deref()
        .map(sanitize_ini_value)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| app_name.clone());
    let comment = sanitize_ini_value(spec.comment.as_deref().unwrap_or("easytidy passthrough"));
    let categories = sanitize_ini_value(spec.categories.as_deref().unwrap_or("Application;Utility;"));
    let icon = spec.icon.as_ref().map(|i| sanitize_ini_value(i));
    let wm = spec.startup_wm_class.as_ref().map(|w| sanitize_ini_value(w));
    let desktop_file = sanitize_ini_value(&spec.desktop_file);

    let mut content = format!(
        "[Desktop Entry]\n\
         Name={display}\n\
         GenericName=easytidy {} - {display}\n\
         Comment={comment}\n\
         Categories={categories}\n",
        spec.container,
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
         Name=Remove {display} from system\n\
         Exec={} unexport --container {} --app-id {}\n",
        spec.container,
        spec.cli_path,
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

/// 容器入口图标文件名（`container_icon_dir(container)/easytidy-gui.png`）。
const CONTAINER_ENTRY_ICON_FILE: &str = "easytidy-gui.png";

/// 容器入口图标文件路径（`container_icon_dir(container)/easytidy-gui.png`）。
fn container_entry_icon_path(container: &str) -> Option<PathBuf> {
    Some(
        crate::appdata::container_icon_dir(container)
            .ok()?
            .join(CONTAINER_ENTRY_ICON_FILE),
    )
}

/// 把加工后的入口图标写入 `container_icon_dir(container)/easytidy-gui.png`，返回路径。
fn write_container_entry_icon(container: &str, bytes: &[u8]) -> Option<String> {
    let path = container_entry_icon_path(container)?;
    std::fs::write(&path, bytes).ok()?;
    tracing::info!("容器入口图标写入：{container} → {}", path.display());
    Some(path.to_string_lossy().into_owned())
}

/// 容器入口兒底图标（未设自定义图标时）：按 `seed`（导出显示名；改名重导出
/// 后图标随之变）确定性生成的 identicon 像素图 + 品牌包装，写入
/// `container_icon_dir(container)/identicon.png`
/// （独立文件名——不与用户自定义/加工图标 `easytidy-gui.png` 同文件互踩）。
/// **每次覆盖写**。返回图标绝对路径；生成/写失败返回 None。
pub fn ensure_container_entry_icon(container: &str, seed: &str) -> Option<String> {
    let path = container_entry_icon_path(container)?.with_file_name("identicon.png");
    // 每次都覆盖写：同容器路径固定，重写保证文件内容始终反映当前种子/算法/
    // 品牌包装（生成成本毫秒级）；也避免旧版本残留文件被固化
    let raw = crate::icon::generate_icon(seed).ok()?;
    let wrapped = crate::icon::compose_app_icon(&raw, EASYTIDY_ICON_PNG).ok()?;
    std::fs::write(&path, &wrapped).ok()?;
    Some(path.to_string_lossy().into_owned())
}

/// 应用兒底图标（passthrough 应用无图标时）：按应用 id 确定性生成的
/// identicon 像素图（同应用 id = 同图标），写入
/// `~/.easytidy/icons/identicon-<safe-id>.png`。返回图标绝对路径；
/// 生成/写失败返回 None（不影响快捷方式导出）。
pub fn ensure_app_icon(app_id: &str) -> Option<String> {
    let dir = crate::appdata::icons_dir().ok()?;
    let safe: String = app_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    let path = dir.join(format!("identicon-{safe}.png"));
    // 同应用 id 路径固定 → 每次覆盖写（理由同容器入口兒底）
    let raw = crate::icon::generate_icon(app_id).ok()?;
    let wrapped = crate::icon::compose_app_icon(&raw, EASYTIDY_ICON_PNG).ok()?;
    std::fs::write(&path, &wrapped).ok()?;
    Some(path.to_string_lossy().into_owned())
}

/// 容器入口图标加工：用户配置的图标源（宿主路径）经内置品牌工具
/// [`crate::icon::compose_app_icon`]（渐变圆角边框 + 圆角内容 + 品牌水印）
/// 加工后写入 `container_icon_dir(container)/easytidy-gui.png`。
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
    write_container_entry_icon(container, &composed)
}

/// 从字节加工容器入口图标（不经宿主临时文件）：容器内图标源经 server 拉取到
/// 内存后直接加工，写入 `container_icon_dir(container)/easytidy-gui.png`（图标源留在
/// 容器内，宿主仅在导出时生成桌面图标副本）。
pub fn process_container_icon_bytes(container: &str, data: &[u8]) -> Option<String> {
    let composed = crate::icon::compose_app_icon(data, EASYTIDY_ICON_PNG).ok()?;
    write_container_entry_icon(container, &composed)
}

/// GUI 入口 .desktop 内容（纯函数，可单测）。
///
/// Exec = `<cli> open --container <name>`——经 CLI 垫片确保容器运行、
/// 等待 server socket 就绪，再按配置 silent_boot 决定是否拉起 Worker GUI
/// （直接冷启动 380MB GUI 是点击慢的根源，垫片先行秒级保活）。
/// `icon`：加工后的容器图标路径（[`process_container_icon`]）；
/// 未设置回退按容器名生成的 identicon。
pub fn generate_gui_entry_content(
    container: &str,
    cli_path: &str,
    icon: Option<&str>,
    display_name: Option<&str>,
) -> String {
    // 桌面显示名：用户指定优先（空视同未指定），否则 `easytidy <容器名>`
    let display = display_name
        .map(sanitize_ini_value)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("easytidy {container}"));
    let mut content = format!(
        "[Desktop Entry]\n\
         Name={display}\n\
         GenericName=Container GUI for {container}\n\
         Comment=Open {container} management interface\n\
         Categories=System;Utility;ContainerManagement;\n\
         Exec={cli_path} open --container {container}\n",
    );
    let icon = icon
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        // 兒底种子 = 最终桌面显示名（display），与 .desktop Name= 同源：
        // 改名重导出 → 图标与名字一起变
        .or_else(|| ensure_container_entry_icon(container, &display));
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
/// - `icon`：加工后的容器图标路径（未设置回退按容器名生成的 identicon）
///
/// **覆盖写语义**：同容器路径固定，`std::fs::write` 直接覆盖已有 .desktop
/// （含桌面副本），Icon=/Exec= 等字段始终反映本次导出状态；权限/信任标记
/// 同步重打。返回应用菜单路径。
pub fn write_gui_entry(
    container: &str,
    cli_path: &str,
    desktop_icon: bool,
    icon: Option<&str>,
    display_name: Option<&str>,
) -> Result<PathBuf> {
    let dir = passthrough_dir()?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| Error::Config(format!("创建 applications 目录失败：{e}")))?;

    let content = generate_gui_entry_content(container, cli_path, icon, display_name);

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
/// 写入 .desktop 文件（+x 权限）。**覆盖写**：同路径已有文件直接覆盖，
/// 字段始终反映本次导出；权限同步重打。
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
        // 按 app_id（X-easytidy-app-id）匹配
        let matched = parse_pt_value(&content, container, "X-easytidy-app-id=")
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

// ============================================================================
// 桌面快捷方式管理（**纯宿主侧**：扫描 / 移除 / 图标重编，不涉及容器）
// ============================================================================

/// 宿主侧 easytidy 桌面快捷方式（.desktop）扫描结果。
///
/// 同一 identity 的菜单项（applications 目录）与桌面副本合并为一行；
/// identity：entry = 容器名，app = `容器名/app_id`。
#[derive(Debug, Clone, serde::Serialize)]
pub struct DesktopIconEntry {
    /// 稳定 identity（entry = 容器名；app = `容器名/app_id`）
    pub identity: String,
    /// "entry"（容器入口）| "app"（passthrough 应用）
    pub kind: String,
    pub container: String,
    /// .desktop `Name=` 字段
    pub title: String,
    /// .desktop `Icon=` 字段（宿主路径或主题名）
    pub icon: String,
    /// app 的 `X-easytidy-app-id`（旧格式回退 `X-easytidy-app`）
    pub app_id: Option<String>,
    /// app 的容器内 .desktop 路径（`X-easytidy-app`，仅展示）
    pub app_file: Option<String>,
    /// 菜单项（~/.local/share/applications）文件路径
    pub menu_path: Option<String>,
    /// 桌面副本文件路径
    pub desktop_path: Option<String>,
}

/// .desktop 字段取值（`Key=value` 单行）
fn desktop_field(content: &str, key: &str) -> Option<String> {
    content
        .lines()
        .find_map(|l| l.strip_prefix(key))
        .map(|s| s.trim().to_string())
}

/// 扫描宿主侧全部 easytidy 生成的 .desktop（applications 菜单目录 + 桌面
/// 目录），按 identity 合并菜单/桌面副本。
///
/// 分类以 X- 标记为准（文件名回退）：
/// - app：`X-easytidy-pt=1`（或 `easytidy-pt-*` 文件名）
/// - entry：其余（`X-easytidy-gui=1`）
pub fn scan_desktop_icons() -> Result<Vec<DesktopIconEntry>> {
    let mut entries: std::collections::BTreeMap<String, DesktopIconEntry> =
        std::collections::BTreeMap::new();

    let mut dirs: Vec<(PathBuf, bool)> = Vec::new(); // (目录, 是否桌面副本)
    if let Ok(d) = passthrough_dir() {
        dirs.push((d, false));
    }
    if let Some(d) = desktop_dir() {
        dirs.push((d, true));
    }

    for (dir, is_desktop) in dirs {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for f in rd.flatten() {
            let file_name = f.file_name();
            let name = match file_name.to_str() { Some(s) => s, None => continue };
            if !name.starts_with("easytidy-") || !name.ends_with(".desktop") {
                continue;
            }
            let Ok(content) = fs::read_to_string(f.path()) else { continue };

            let is_pt = content
                .lines()
                .any(|l| l.trim() == "X-easytidy-pt=1")
                || name.starts_with("easytidy-pt-");

            // 容器名：X- 标记优先；文件名回退（仅新格式 easytidy-gui-<c> / easytidy-pt-<c>-<id>）
            let stem = name
                .trim_start_matches("easytidy-")
                .trim_end_matches(".desktop");
            let container = desktop_field(&content, "X-easytidy-container=").or_else(|| {
                if let Some(c) = stem.strip_prefix("gui-") {
                    Some(c.to_string())
                } else if let Some(rest) = stem.strip_prefix("pt-") {
                    rest.split('-').next().map(String::from)
                } else {
                    None
                }
            }).unwrap_or_default();
            if container.is_empty() {
                continue; // 无标记且非已知命名 → 非 easytidy 生成的入口
            }

            let app_id = desktop_field(&content, "X-easytidy-app-id=");

            let (kind, identity, entry_app_id) = if is_pt {
                let aid = app_id.clone().unwrap_or_default();
                ("app", format!("{container}/{aid}"), Some(aid))
            } else {
                ("entry", container.clone(), None)
            };

            let entry = entries.entry(identity.clone()).or_insert_with(|| DesktopIconEntry {
                identity,
                kind: kind.to_string(),
                container: container.clone(),
                title: desktop_field(&content, "Name=")
                    .unwrap_or_else(|| name.to_string()),
                icon: desktop_field(&content, "Icon=").unwrap_or_default(),
                app_id: entry_app_id,
                app_file: desktop_field(&content, "X-easytidy-app="),
                menu_path: None,
                desktop_path: None,
            });
            if is_desktop {
                entry.desktop_path = Some(f.path().to_string_lossy().into_owned());
            } else {
                entry.menu_path = Some(f.path().to_string_lossy().into_owned());
            }
        }
    }

    tracing::info!("桌面快捷方式扫描：{} 项", entries.len());
    Ok(entries.into_values().collect())
}

/// 移除容器入口快捷方式（菜单/桌面副本）。
/// 返回实际删除的路径。
pub fn remove_entry_desktops(container: &str) -> Vec<PathBuf> {
    let mut removed = Vec::new();
    let mut dirs = Vec::new();
    if let Ok(d) = passthrough_dir() {
        dirs.push(d);
    }
    if let Some(d) = desktop_dir() {
        dirs.push(d);
    }
    for dir in dirs {
        let p = dir.join(format!("easytidy-gui-{container}.desktop"));
        if p.exists() && fs::remove_file(&p).is_ok() {
            tracing::info!("移除容器入口快捷方式：{}", p.display());
            removed.push(p);
        }
    }
    removed
}

/// 移除容器入口图标（新布局 `container_icon_dir(container)/` + 旧布局
/// `icons_dir/easytidy-gui-<container>*` 遗留）。容器删除时调用，避免图标数据残留。
pub fn remove_entry_icons(container: &str) -> Vec<PathBuf> {
    let mut removed = Vec::new();
    // 新布局：容器入口图标目录（container_icon_dir(container)/）
    if let Ok(base) = crate::appdata::containers_dir() {
        let dir = base.join(container);
        if dir.is_dir() && fs::remove_dir_all(&dir).is_ok() {
            tracing::info!("移除容器入口图标目录：{}", dir.display());
            removed.push(dir);
        }
    }
    // 旧布局遗留：icons_dir/easytidy-gui-<container>.png + -src-* 临时源
    if let Ok(dir) = crate::appdata::icons_dir() {
        if let Ok(entries) = fs::read_dir(&dir) {
            let icon_file = format!("easytidy-gui-{container}.png");
            let src_prefix = format!("easytidy-gui-{container}-");
            for e in entries.flatten() {
                let path = e.path();
                let name = e.file_name().to_string_lossy().to_string();
                if (name == icon_file || name.starts_with(&src_prefix))
                    && path.is_file()
                    && fs::remove_file(&path).is_ok()
                {
                    tracing::info!("移除容器入口图标文件（旧布局）：{}", path.display());
                    removed.push(path);
                }
            }
        }
    }
    removed
}

/// .desktop 内容替换 `Icon=` 行（无则插到首行后）
fn replace_icon_line(content: &str, icon: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut done = false;
    for line in content.lines() {
        if !done && line.starts_with("Icon=") {
            out.push(format!("Icon={icon}"));
            done = true;
        } else {
            out.push(line.to_string());
        }
    }
    if !done {
        let mut new = out;
        new.insert(1, format!("Icon={icon}"));
        return new.join("\n") + "\n";
    }
    out.join("\n") + "\n"
}

/// 图标重编（**纯宿主侧**，不涉及容器）：所选宿主图片经内置品牌工具
/// [`crate::icon::compose_app_icon`] 加工（渐变边框 + 圆角内容 + 水印，
/// 256×256 PNG）写入标准图标文件，并更新该 identity 全部 .desktop 的 `Icon=`。
///
/// - `kind = "entry"`：容器入口（菜单/桌面副本），图标文件
///   `container_icon_dir(container)/easytidy-gui.png`（与创建/导出加工同名同位置）
/// - `kind = "app"`：passthrough 应用（按 app_id），图标文件
///   `icons_dir/easytidy-pt-<container>-<sanitized id>.png`（与导出加工同名覆盖）
///
/// 返回加工后的图标文件路径。
pub fn reedit_desktop_icon(
    kind: &str,
    container: &str,
    app_id: Option<&str>,
    source: &str,
) -> Result<String> {
    let data = fs::read(source)
        .map_err(|e| Error::Config(format!("读取图片失败（{source}）：{e}")))?;
    let composed = crate::icon::compose_app_icon(&data, EASYTIDY_ICON_PNG)
        .map_err(|e| Error::Config(format!("图标加工失败：{e}")))?;

    let icon_file = match kind {
        "entry" => crate::appdata::container_icon_dir(container)
            .map_err(|e| Error::Config(format!("创建容器图标目录失败：{e}")))?
            .join(CONTAINER_ENTRY_ICON_FILE),
        _ => {
            let dir = crate::appdata::icons_dir()?;
            fs::create_dir_all(&dir)
                .map_err(|e| Error::Config(format!("创建 icons 目录失败：{e}")))?;
            let id = app_id.ok_or_else(|| Error::Config("应用 id 缺失".to_string()))?;
            let safe: String = id
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
                .collect();
            dir.join(format!("easytidy-pt-{container}-{safe}.png"))
        }
    };
    fs::write(&icon_file, &composed)
        .map_err(|e| Error::Config(format!("写入图标失败（{}）：{e}", icon_file.display())))?;
    let icon = icon_file.to_string_lossy().into_owned();

    match kind {
        "entry" => update_entry_icons(container, &icon)?,
        _ => update_pt_icons(container, app_id, &icon)?,
    }
    tracing::info!("桌面快捷方式图标重编：{kind} {container} {:?} → {icon}", app_id);
    Ok(icon)
}

/// 更新容器入口 .desktop（菜单/桌面副本）的 Icon= 行
fn update_entry_icons(container: &str, icon: &str) -> Result<()> {
    let mut dirs = Vec::new();
    if let Ok(d) = passthrough_dir() {
        dirs.push(d);
    }
    if let Some(d) = desktop_dir() {
        dirs.push(d);
    }
    let mut touched = 0;
    for dir in dirs {
        let p = dir.join(format!("easytidy-gui-{container}.desktop"));
        if let Ok(content) = fs::read_to_string(&p) {
            fs::write(&p, replace_icon_line(&content, icon))
                .map_err(|e| Error::Config(format!("更新 .desktop 失败（{}）：{e}", p.display())))?;
            touched += 1;
        }
    }
    if touched == 0 {
        return Err(Error::Config(format!("未找到容器入口 .desktop：{container}")));
    }
    Ok(())
}

/// 更新 passthrough .desktop（菜单/桌面副本，按 X- 标记匹配）的 Icon= 行
fn update_pt_icons(container: &str, app_id: Option<&str>, icon: &str) -> Result<()> {
    let mut dirs = Vec::new();
    if let Ok(d) = passthrough_dir() {
        dirs.push(d);
    }
    if let Some(d) = desktop_dir() {
        dirs.push(d);
    }
    let mut touched = 0;
    for dir in dirs {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for f in rd.flatten() {
            let file_name = f.file_name();
            let Some(name) = file_name.to_str() else { continue };
            if !name.starts_with("easytidy-") || !name.ends_with(".desktop") {
                continue;
            }
            let Ok(content) = fs::read_to_string(f.path()) else { continue };
            if !content.lines().any(|l| l.trim() == "X-easytidy-pt=1") {
                continue;
            }
            if parse_pt_value(&content, container, "X-easytidy-container=").is_none() {
                continue;
            }
            let matched = match app_id {
                Some(id) => parse_pt_value(&content, container, "X-easytidy-app-id=")
                    .or_else(|| parse_pt_value(&content, container, "X-easytidy-app="))
                    .as_deref()
                    == Some(id),
                None => false,
            };
            if matched {
                fs::write(f.path(), replace_icon_line(&content, icon))
                    .map_err(|e| Error::Config(format!("更新 .desktop 失败（{}）：{e}", f.path().display())))?;
                touched += 1;
            }
        }
    }
    if touched == 0 {
        return Err(Error::Config(format!(
            "未找到已导出的应用快捷方式：{container} {:?}",
            app_id
        )));
    }
    Ok(())
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
    fn test_generate_passthrough_content_sanitizes_app_name() {
        let spec = PassthroughSpec {
            container: "chrome".to_string(),
            app_name: "My\nApp".to_string(),
            display_name: None,
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
    fn test_generate_gui_entry_content_open_format() {
        let content = generate_gui_entry_content("chrome", "/usr/bin/easytidy", None, None);

        assert!(content.contains("Exec=/usr/bin/easytidy open --container chrome"));
        assert!(content.contains("TryExec=/usr/bin/easytidy"));
        assert!(content.lines().any(|l| l.trim() == "X-easytidy-gui=1"));
        assert!(content.contains("X-easytidy-container=chrome"));
        // Exec 不再直连 GUI 二进制（X-easytidy-gui 标记含该子串，须按行判定）
        let exec_line = content.lines().find(|l| l.starts_with("Exec=")).unwrap();
        assert!(!exec_line.contains("easytidy-gui"));
    }

    #[test]
    fn test_display_name_override_and_fallback() {
        // 容器入口：指定显示名 → Name= 用指定值；未指定 → easytidy <容器名>
        let content =
            generate_gui_entry_content("chrome", "/usr/bin/easytidy", None, Some("我的 Chrome 容器"));
        assert!(content.lines().any(|l| l == "Name=我的 Chrome 容器"));
        let content = generate_gui_entry_content("chrome", "/usr/bin/easytidy", None, None);
        assert!(content.lines().any(|l| l == "Name=easytidy chrome"));
        // 空白显示名视同未指定
        let content = generate_gui_entry_content("chrome", "/usr/bin/easytidy", None, Some("   "));
        assert!(content.lines().any(|l| l == "Name=easytidy chrome"));

        // passthrough：指定显示名 → Name=/GenericName/Remove 动作名均用指定值
        let spec = PassthroughSpec {
            container: "chrome".to_string(),
            app_name: "Google Chrome".to_string(),
            display_name: Some("我的浏览器".to_string()),
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
        assert!(content.lines().any(|l| l == "Name=我的浏览器"));
        assert!(!content.lines().any(|l| l == "Name=Google Chrome"));
        assert!(content.lines().any(|l| l == "Name=Remove 我的浏览器 from system"));
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
        assert!(out.ends_with("e2e-icon-test/easytidy-gui.png"));
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
            display_name: None,
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

}
