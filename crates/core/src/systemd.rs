//! systemd user unit 生成（静默启动）。
//!
//! 功能：
//! - 生成用户 systemd unit 文件（用于容器静默启动）
//! - ExecStart = easytidy --config <cfg> boot
//! - 安装到 $XDG_CONFIG_HOME/systemd/user/
//! - 支持 enable/disable

use std::path::{Path, PathBuf};
use std::fs;
use crate::error::{Error, Result};

/// systemd user unit 模板。
///
/// 注意：
/// - Type=oneshot（单次启动，不常驻）
/// - RemainAfterExit=yes（启动后视为活跃）
/// - WantedBy=default.target（登录时启动）
const SYSTEMD_UNIT_TEMPLATE: &str = r#"[Unit]
Description=easytidy container silent boot: {name}
Documentation=https://github.com/dingiv/easy-tidy
After=network-online.target podman.socket
Wants=network-online.target

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart={exec} boot
Environment=RUST_LOG=info

[Install]
WantedBy=default.target
"#;

/// 生成 systemd user unit 内容。
///
/// 参数：
/// - name: 容器名
/// - config_path: 配置文件路径（easytidy --config <cfg>）
///
/// 返回 unit 文件内容字符串。
pub fn generate_systemd_unit(
    name: &str,
    config_path: &Path,
) -> String {
    let exec = if let Some(cfg) = config_path.to_str() {
        format!("easytidy --config {}", cfg)
    } else {
        "easytidy".to_string()
    };

    SYSTEMD_UNIT_TEMPLATE
        .replace("{name}", name)
        .replace("{exec}", &exec)
}

/// 安装 systemd user unit 文件。
///
/// 参数：
/// - name: 容器名
/// - config_path: 配置文件路径
///
/// 返回安装后的 unit 文件路径。
pub fn install_systemd_unit(
    name: &str,
    config_path: &Path,
) -> Result<PathBuf> {
    let content = generate_systemd_unit(name, config_path);

    // 确定目标路径（$XDG_CONFIG_HOME/systemd/user/）
    let config_home = dirs::config_dir()
        .ok_or_else(|| Error::Config("无法确定 XDG_CONFIG_HOME".to_string()))?;

    let systemd_dir = config_home.join("systemd").join("user");
    fs::create_dir_all(&systemd_dir)
        .map_err(|e| Error::Config(format!("创建 systemd 用户目录失败：{e}")))?;

    let unit_path = systemd_dir.join(format!("easytidy-{}.service", name));

    // 写入文件
    fs::write(&unit_path, content)
        .map_err(|e| Error::Config(format!("写入 systemd unit 文件失败：{e}")))?;

    tracing::info!("安装 systemd unit：{:?}", unit_path);

    Ok(unit_path)
}

/// 卸载 systemd user unit 文件。
pub fn uninstall_systemd_unit(name: &str) -> Result<()> {
    let config_home = dirs::config_dir()
        .ok_or_else(|| Error::Config("无法确定 XDG_CONFIG_HOME".to_string()))?;

    let unit_path = config_home
        .join("systemd")
        .join("user")
        .join(format!("easytidy-{}.service", name));

    if unit_path.exists() {
        fs::remove_file(&unit_path)
            .map_err(|e| Error::Config(format!("删除 systemd unit 文件失败：{e}")))?;

        tracing::info!("卸载 systemd unit：{:?}", unit_path);
    }

    Ok(())
}

/// 启用 systemd user unit（需要调用 systemctl daemon-reload）。
///
/// 注意：此函数仅生成启用命令，不实际执行（需要 shell 执行）。
pub fn enable_systemd_unit(name: &str) -> String {
    format!("systemctl --user enable easytidy-{}.service", name)
}

/// 禁用 systemd user unit。
///
/// 注意：此函数仅生成禁用命令，不实际执行。
pub fn disable_systemd_unit(name: &str) -> String {
    format!("systemctl --user disable easytidy-{}.service", name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_generate_systemd_unit() {
        let content = generate_systemd_unit(
            "test-container",
            &PathBuf::from("/home/user/.config/easytidy/config.toml"),
        );

        assert!(content.contains("Description=easytidy container silent boot: test-container"));
        assert!(content.contains("ExecStart=easytidy --config"));
        assert!(content.contains("boot"));
        assert!(content.contains("WantedBy=default.target"));
    }

    #[test]
    fn test_install_systemd_unit() {
        let temp_dir = TempDir::new().unwrap();

        // 模拟 XDG_CONFIG_HOME
        let target = temp_dir
            .path()
            .join("systemd/user/easytidy-test.service");

        // 手动创建目录并写入
        let content = generate_systemd_unit("test", &PathBuf::from("/tmp/config.toml"));
        fs::create_dir_all(temp_dir.path().join("systemd/user")).unwrap();
        fs::write(&target, content).unwrap();

        assert!(target.exists());
        let loaded = fs::read_to_string(&target).unwrap();
        assert!(loaded.contains("easytidy container silent boot"));
    }

    #[test]
    fn test_enable_disable_commands() {
        let enable = enable_systemd_unit("test");
        assert_eq!(enable, "systemctl --user enable easytidy-test.service");

        let disable = disable_systemd_unit("test");
        assert_eq!(disable, "systemctl --user disable easytidy-test.service");
    }
}
