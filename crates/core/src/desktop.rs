//! .desktop 文件生成（宿主导出应用图标）。
//!
//! 功能：
//! - 生成容器专用的 .desktop 文件（Exec = easytidy --container <name>）
//! - 安装到 $XDG_DATA_HOME/applications/
//! - 支持自定义图标（使用发行版 logo 或默认）

use std::path::PathBuf;
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
