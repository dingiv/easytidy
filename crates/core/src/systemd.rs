//! systemd user unit 生成与安装（容器自启动）。
//!
//! 登录时自启动（WantedBy=default.target）：
//! - 静默启动：`ExecStart = <cli> boot --container <name>`——仅后台启动容器
//! - 非静默启动：`ExecStart = <cli> boot --container <name> --gui`——
//!   启动容器后拉起 per-container GUI 窗口
//! - 关闭：卸载 unit 并 disable
//!
//! unit 文件在 `~/.config/systemd/user/easytidy-<name>.service`
//! （systemd 用户单元规范位置）；enable/disable/daemon-reload 实际执行
//! `systemctl --user`（GUI/CLI 运行于用户会话，DBus 可用）。

use std::fs;
use std::path::PathBuf;

use crate::error::{Error, Result};

/// systemd user unit 模板。
///
/// - Type=oneshot（单次启动，不常驻）+ RemainAfterExit=yes（启动后视为活跃）
/// - After=podman.socket（依赖 rootless podman API）
const SYSTEMD_UNIT_TEMPLATE: &str = r#"[Unit]
Description=easytidy container boot: {name}
Documentation=https://github.com/dingiv/easy-tidy
After=network-online.target podman.socket
Wants=network-online.target

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart={cli} boot --container {name}{gui_flag}
Environment=RUST_LOG=info

[Install]
WantedBy=default.target
"#;

/// 生成容器自启动 unit 内容。
///
/// - `cli_path`：easytidy CLI 绝对路径（systemd 用户单元 PATH 受限，必须绝对）
/// - `gui`：true = 非静默（启动容器后拉起 per-container GUI）
pub fn generate_boot_unit(name: &str, cli_path: &str, gui: bool) -> String {
    let gui_flag = if gui { " --gui" } else { "" };
    SYSTEMD_UNIT_TEMPLATE
        .replace("{name}", name)
        .replace("{cli}", cli_path)
        .replace("{gui_flag}", gui_flag)
}

/// unit 文件路径（~/.config/systemd/user/easytidy-<name>.service）
fn unit_path(name: &str) -> Result<PathBuf> {
    let config_home = dirs::config_dir()
        .ok_or_else(|| Error::Config("无法确定 XDG_CONFIG_HOME".to_string()))?;
    Ok(config_home.join("systemd").join("user").join(format!("easytidy-{name}.service")))
}

/// 安装自启动 unit（写入 unit 文件；需随后 daemon-reload + enable）。
pub fn install_boot_unit(name: &str, cli_path: &str, gui: bool) -> Result<PathBuf> {
    let path = unit_path(name)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| Error::Config(format!("创建 systemd 用户目录失败：{e}")))?;
    }
    fs::write(&path, generate_boot_unit(name, cli_path, gui))
        .map_err(|e| Error::Config(format!("写入 systemd unit 失败：{e}")))?;
    tracing::info!("安装自启动 unit：{:?}", path);
    Ok(path)
}

/// 卸载自启动 unit（删除文件；需随后 daemon-reload）。
pub fn remove_boot_unit(name: &str) -> Result<()> {
    let path = unit_path(name)?;
    if path.exists() {
        fs::remove_file(&path)
            .map_err(|e| Error::Config(format!("删除 systemd unit 失败：{e}")))?;
        tracing::info!("卸载自启动 unit：{:?}", path);
    }
    Ok(())
}

/// 执行 `systemctl --user` 命令（单元生命周期操作）。
fn run_systemctl(args: &[&str]) -> Result<()> {
    let output = std::process::Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .map_err(|e| Error::Connect(format!("执行 systemctl 失败（用户会话可用？）：{e}")))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(Error::Connect(format!(
            "systemctl {} 失败：{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

/// daemon-reload（unit 文件变更后必须）。
pub fn daemon_reload() -> Result<()> {
    run_systemctl(&["daemon-reload"])
}

/// 启用自启动（WantedBy=default.target → 登录时触发）。
pub fn enable_boot_unit(name: &str) -> Result<()> {
    run_systemctl(&["enable", &format!("easytidy-{name}.service")])
}

/// 禁用自启动。
pub fn disable_boot_unit(name: &str) -> Result<()> {
    run_systemctl(&["disable", &format!("easytidy-{name}.service")])
}

/// 查询当前自启动模式："off"（未安装）/ "silent"（静默）/ "gui"（非静默）。
pub fn boot_mode(name: &str) -> Result<String> {
    let path = unit_path(name)?;
    if !path.exists() {
        return Ok("off".to_string());
    }
    let content = fs::read_to_string(&path)
        .map_err(|e| Error::Config(format!("读取 systemd unit 失败：{e}")))?;
    Ok(if content.contains(" --gui") { "gui" } else { "silent" }.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_boot_unit_silent() {
        let content = generate_boot_unit("chrome", "/usr/bin/easytidy", false);
        assert!(content.contains("Description=easytidy container boot: chrome"));
        assert!(content.contains("ExecStart=/usr/bin/easytidy boot --container chrome"));
        assert!(!content.contains(" --gui"));
        assert!(content.contains("WantedBy=default.target"));
    }

    #[test]
    fn test_generate_boot_unit_gui() {
        let content = generate_boot_unit("chrome", "/usr/bin/easytidy", true);
        assert!(content.contains("ExecStart=/usr/bin/easytidy boot --container chrome --gui"));
    }
}
