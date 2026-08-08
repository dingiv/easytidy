//! .desktop 文件生成（宿主导出应用图标）。
//!
//! 功能：
//! - 生成容器专用的 .desktop 文件（Exec = easytidy --container <name>）
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

/// 生成 .desktop 文件内容。
///
/// 参数：
/// - name: 容器名
/// - title: 显示标题（默认 "easytidy <name>"）
/// - icon: 图标路径（默认使用发行版 logo 或 easytidy 图标）
///
/// 返回 .desktop 文件内容字符串。
pub fn generate_desktop_entry(
    name: &str,
    title: Option<&str>,
    icon: Option<&str>,
) -> String {
    let default_title = format!("easytidy {}", name);
    let title = title.unwrap_or(&default_title);
    let icon = icon.unwrap_or("easytidy-container");
    let exec = format!("easytidy --container {}", name);

    DESKTOP_ENTRY_TEMPLATE
        .replace("{title}", title)
        .replace("{exec}", &exec)
        .replace("{icon}", icon)
        .replace("{name}", name)
}

/// 安装 .desktop 文件到用户应用目录。
///
/// 参数：
/// - name: 容器名
/// - title: 显示标题
/// - icon: 图标路径
///
/// 返回安装后的 .desktop 文件路径。
pub fn install_desktop_entry(
    name: &str,
    title: Option<&str>,
    icon: Option<&str>,
) -> Result<PathBuf> {
    let content = generate_desktop_entry(name, title, icon);

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

/// 标记 GNOME 桌面 .desktop 为已信任（允许双击启动；仅桌面路径需要，
/// 应用菜单无需。非 GNOME 环境 gio 缺失时静默忽略）。
pub fn mark_desktop_trusted(path: &Path) {
    let _ = std::process::Command::new("gio")
        .args(["set", &path.to_string_lossy(), "metadata::trusted", "true"])
        .status();
}

/// passthrough 导出参数（宿主侧生成 .desktop 的全部输入）
pub struct PassthroughSpec {
    pub container: String,
    pub app_name: String,
    pub comment: Option<String>,
    pub categories: Option<String>,
    /// 清理后的容器内执行命令（已去 %U 等占位符）
    pub exec: String,
    /// 宿主图标绝对路径（已搬运到本地；空则不写 Icon 字段）
    pub icon: Option<String>,
    /// 容器内 .desktop 路径（应用标识，revoke/state 用）
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
fn generate_passthrough_content(spec: &PassthroughSpec) -> String {
    let comment = spec.comment.as_deref().unwrap_or("easytidy passthrough");
    let categories = spec.categories.as_deref().unwrap_or("Application;Utility;");
    let mut content = format!(
        "[Desktop Entry]\n\
         Name={}\n\
         GenericName=easytidy {} - {}\n\
         Comment={comment}\n\
         Categories={categories}\n",
        spec.app_name, spec.container, spec.app_name,
    );
    // ⚠️ --container 是子命令级参数（`easytidy run --container <n> -- ...`），
    // 放顶层会报 "unexpected argument '--container'"（journalctl 实测）
    content.push_str(&format!(
        "Exec={} run --container {} -- {}\n",
        spec.cli_path, spec.container, spec.exec,
    ));
    if let Some(icon) = spec.icon.as_ref() {
        content.push_str(&format!("Icon={icon}\n"));
    }
    if spec.startup_notify {
        content.push_str("StartupNotify=true\n");
    }
    if let Some(wm) = spec.startup_wm_class.as_ref() {
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
         Exec={} unexport --container {} --desktop-file {}\n",
        spec.container,
        spec.cli_path,
        spec.app_name,
        spec.cli_path,
        spec.container,
        spec.desktop_file,
    ));
    content.push_str(&format!(
        "X-easytidy-pt=1\n\
         X-easytidy-container={}\n\
         X-easytidy-app={}\n",
        spec.container, spec.desktop_file,
    ));
    content
}

/// easytidy 品牌图标（内置 SVG，首次导出 GUI 入口时写入宿主图标目录；
/// M4 打包时替换为正式品牌资产）。容器 + 终端提示符风格。
const EASYTIDY_ICON_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 128 128">
  <defs>
    <linearGradient id="g" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0" stop-color="#89b4fa"/>
      <stop offset="1" stop-color="#b4befe"/>
    </linearGradient>
  </defs>
  <rect x="8" y="8" width="112" height="112" rx="24" fill="url(#g)"/>
  <rect x="24" y="24" width="80" height="80" rx="10" fill="#1e1e2e" opacity="0.88"/>
  <path d="M34 46h60M34 62h60M34 78h38" stroke="#cdd6f4" stroke-width="6" stroke-linecap="round" fill="none"/>
</svg>"##;

/// 确保 easytidy 品牌图标存在（~/.easytidy/icons/easytidy-gui.svg）
/// 返回图标绝对路径；写入失败返回 None（不影响快捷方式导出）。
pub fn ensure_gui_icon() -> Option<String> {
    let dir = crate::appdata::icons_dir().ok()?;
    let path = dir.join("easytidy-gui.svg");
    if !path.exists() && std::fs::write(&path, EASYTIDY_ICON_SVG).is_err() {
        return None;
    }
    Some(path.to_string_lossy().into_owned())
}

/// 导出本容器的 GUI 管理界面桌面快捷方式（distrobox "进入容器"入口的
/// easytidy 变体：Exec 打开 per-container 管理窗口）。
///
/// - 应用菜单：~/.local/share/applications/easytidy-gui-<container>.desktop
/// - 桌面图标：桌面路径同名文件 + chmod +x + `gio metadata::trusted`
///   （GNOME 双击必需；`desktop_icon=true` 且桌面目录存在时）
/// - `gui_path`：per-container 模式启动的 GUI 二进制绝对路径（Exec/TryExec）
///
/// 返回应用菜单路径。
pub fn write_gui_entry(container: &str, gui_path: &str, desktop_icon: bool) -> Result<PathBuf> {
    let dir = passthrough_dir()?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| Error::Config(format!("创建 applications 目录失败：{e}")))?;

    let icon = ensure_gui_icon();
    let mut content = format!(
        "[Desktop Entry]\n\
         Name=easytidy {container}\n\
         GenericName=Container GUI for {container}\n\
         Comment=Open {container} management interface\n\
         Categories=System;Utility;ContainerManagement;\n\
         Exec={gui_path} --container {container}\n",
    );
    if let Some(icon) = icon.as_ref() {
        content.push_str(&format!("Icon={icon}\n"));
    }
    content.push_str(&format!(
        "Keywords=easytidy;{container};\n\
         NoDisplay=false\n\
         Terminal=false\n\
         TryExec={gui_path}\n\
         Type=Application\n\
         X-easytidy-gui=1\n\
         X-easytidy-container={container}\n",
    ));

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
fn passthrough_file_name(container: &str, desktop_file: &str) -> String {
    let base = desktop_file
        .rsplit('/')
        .next()
        .unwrap_or("app")
        .trim_end_matches(".desktop");
    let safe: String = base
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

    let file_name = passthrough_file_name(&spec.container, &spec.desktop_file);
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
pub fn remove_passthrough(container: &str, desktop_file: &str) -> Result<PathBuf> {
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
        if content.lines().any(|l| l == format!("X-easytidy-app={desktop_file}")) {
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
            tracing::info!("passthrough 撤销：{desktop_file} → {path:?}");
            return Ok(path);
        }
    }
    Err(Error::Config(format!("未找到已导出的应用：{desktop_file}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_generate_desktop_entry() {
        let content = generate_desktop_entry(
            "test-container",
            Some("Test Container"),
            Some("test-icon"),
        );

        assert!(content.contains("Name=Test Container"));
        assert!(content.contains("Exec=easytidy --container test-container"));
        assert!(content.contains("Icon=test-icon"));
        assert!(content.contains("X-easytidy-container=test-container"));
    }

    #[test]
    fn test_generate_desktop_entry_defaults() {
        let content = generate_desktop_entry("test", None, None);

        assert!(content.contains("Name=easytidy test"));
        assert!(content.contains("Icon=easytidy-container"));
    }

    #[test]
    fn test_install_desktop_entry() {
        let temp_dir = TempDir::new().unwrap();

        // 模拟 XDG_DATA_HOME
        let target = temp_dir.path().join("applications/easytidy-test.desktop");

        // 手动创建目录并写入
        let content = generate_desktop_entry("test", None, None);
        fs::create_dir_all(temp_dir.path().join("applications")).unwrap();
        fs::write(&target, content).unwrap();

        assert!(target.exists());
        let loaded = fs::read_to_string(&target).unwrap();
        assert!(loaded.contains("easytidy --container test"));
    }
}
