//! XDG 桌面条目（.desktop）解析：把容器内 `.desktop` 快捷方式解析成 [`AppInfo`]，
//! 并把 `Icon=` 值（常为图标主题名而非路径）解析为容器内实际图标文件路径。
//!
//! 从 `services::apps` 抽出独立成模块——.desktop 解析是自包含的纯逻辑
//! （读文件 + 解析 key=value + 图标主题探测），与 apps 服务的 launch/托管进程
//! 无关，单独维护、便于测试与复用。

use std::fs;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use easytidy_protocol::AppInfo;

/// 解析一个 `.desktop` 文件为 [`AppInfo`]。
///
/// 逐行取 `Key=Value`（跳过空行/注释/`[section]` 头），只取
/// Name/Icon/Exec/Comment/Categories/StartupNotify/StartupWMClass。
/// Name 与 Exec 必填（缺失 → 报错）。NoDisplay 不过滤（passthrough 场景
/// 要看到所有 .desktop，如 python3.12.desktop 的 NoDisplay=true）。
pub(crate) fn parse_desktop_file(path: &Path) -> Result<AppInfo> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read .desktop file: {}", path.display()))?;

    let mut name = None;
    let mut icon_path = None;
    let mut exec = None;
    let mut comment = None;
    let mut categories = None;
    let mut startup_notify = false;
    let mut startup_wm_class = None;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
            continue;
        }

        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            let value = value.trim();

            match key {
                "Name" => name = Some(value.to_string()),
                // Icon 常为主题名（如 "google-chrome"）而非路径——解析成实际
                // 图标文件，宿主 passthrough 才能搬运；解析失败保留原值
                "Icon" => icon_path = resolve_icon_path(value).or(Some(value.to_string())),
                "Exec" => exec = Some(value.to_string()),
                "Comment" => comment = Some(value.to_string()),
                "Categories" => categories = Some(value.to_string()),
                "StartupNotify" => startup_notify = value == "true",
                "StartupWMClass" => startup_wm_class = Some(value.to_string()),
                _ => {}
            }
        }
    }

    let name = name.ok_or_else(|| anyhow!("Missing Name"))?;
    let exec = exec.ok_or_else(|| anyhow!("Missing Exec"))?;

    Ok(AppInfo {
        desktop_file: path.display().to_string(),
        name,
        icon_path,
        exec,
        comment,
        categories,
        startup_notify,
        startup_wm_class,
    })
}

/// 把 `.desktop` 的 Icon 值解析为容器内实际图标文件路径。
///
/// Icon= 常为主题名（如 "google-chrome"）而非路径，按图标主题标准位置
/// 依次探测（hicolor 多尺寸 + Adwaita + pixmaps，svg/png 均试）；已是
/// 绝对路径或相对路径则原样返回。解析失败返回 None（保留原值显示）。
pub(crate) fn resolve_icon_path(icon: &str) -> Option<String> {
    if icon.starts_with('/') || icon.contains('/') {
        return Some(icon.to_string());
    }
    let (base, exts): (&str, &[&str]) = if icon.ends_with(".svg") || icon.ends_with(".png") {
        (icon.trim_end_matches(".svg").trim_end_matches(".png"), &["svg", "png"])
    } else {
        (icon, &["svg", "png"])
    };
    let sizes = ["256x256", "128x128", "64x64", "48x48", "32x32", "24x24", "16x16"];
    for size in sizes {
        for ext in exts {
            for theme_root in ["/usr/share/icons/hicolor", "/usr/share/icons/Adwaita"] {
                let p = format!("{theme_root}/{size}/apps/{base}.{ext}");
                if Path::new(&p).exists() {
                    return Some(p);
                }
            }
        }
    }
    for ext in exts {
        let p = format!("/usr/share/pixmaps/{base}.{ext}");
        if Path::new(&p).exists() {
            return Some(p);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::NamedTempFile;

    /// 基本 .desktop 解析：Name/Exec/Comment/Icon 正确提取
    #[test]
    fn test_parse_desktop_file() {
        let temp_file = NamedTempFile::new().unwrap();
        let desktop_path = temp_file.path().with_extension("desktop");

        let content = r#"[Desktop Entry]
Name=Test App
Exec=test-app --option
Comment=A test application
Icon=test-icon
"#;

        fs::write(&desktop_path, content).unwrap();

        let app = parse_desktop_file(&desktop_path).unwrap();
        assert_eq!(app.name, "Test App");
        assert_eq!(app.exec, "test-app --option");
        assert_eq!(app.comment, Some("A test application".to_string()));
        assert_eq!(app.icon_path, Some("test-icon".to_string()));
    }

    /// Icon= 绝对路径/含路径 → 原样返回
    #[test]
    fn test_resolve_icon_path_passthrough() {
        assert_eq!(
            resolve_icon_path("/usr/share/icons/a.png"),
            Some("/usr/share/icons/a.png".to_string())
        );
        assert_eq!(
            resolve_icon_path("relative/path/x.svg"),
            Some("relative/path/x.svg".to_string())
        );
    }

    /// Icon= 纯主题名且磁盘无对应文件 → None（保留原值显示）
    #[test]
    fn test_resolve_icon_path_missing_theme() {
        // 一个几乎不可能存在的主题名 → 探测全部失败
        assert_eq!(resolve_icon_path("easytidy-no-such-icon-zzz"), None);
    }
}
