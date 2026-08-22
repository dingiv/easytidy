//! easytidy 宿主侧核心引擎。
//!
//! 被三个入口共用：Master GUI、Worker GUI、无头 CLI。
//! 职责：podman socket API 客户端（bollard + libpod 端点）、
//! 容器生命周期/快照、共享配置文件（原子写 + flock + 版本化）、
//! .desktop 生成、宿主 systemd unit 生成。
//!
//! 铁律：不调用任何 podman CLI（见 docs/08-requirements.md L2）。

use std::path::PathBuf;
use crate::error::{Error, Result};

/// 宿主导出 .desktop 的 Exec 前缀（passthrough 机制）
pub const EXEC_PREFIX: &str = "easytidy --container";

pub mod appdata;
pub mod configfile;
pub mod guilock;
pub mod desktop;
pub mod icon;
pub mod passthrough;
pub mod error;
pub mod events;
pub mod flavor;
pub mod libpod;
pub mod models;
pub mod podman;
pub mod systemd;
pub mod userenv;

/// 解析 server 二进制路径（宿主侧）。
///
/// 路径规则（按优先级）：
/// 1. `$XDG_DATA_HOME/easytidy/bin/easytidy-server`
/// 2. `~/.local/share/easytidy/bin/easytidy-server`（fallback）
///
/// 返回 helpful error 如果二进制不存在（提示运行 `easytidy build-server`）。
pub fn server_binary_path() -> Result<PathBuf> {
    // 优先使用 XDG_DATA_HOME
    let data_dir = std::env::var("XDG_DATA_HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| dirs::data_dir().map(|p| p.join("easytidy")));

    let bin_path = if let Some(dir) = data_dir {
        dir.join("bin/easytidy-server")
    } else {
        // Fallback: ~/.local/share
        let home = std::env::var("HOME")
            .map_err(|_| Error::Connect("无法确定 HOME 目录".to_string()))?;
        PathBuf::from(home).join(".local/share/easytidy/bin/easytidy-server")
    };

    // 检查文件是否存在且可执行
    if !bin_path.exists() {
        return Err(Error::Connect(format!(
            "server 二进制不存在：{}\n请先运行：easytidy build-server",
            bin_path.display()
        )));
    }

    Ok(bin_path)
}

/// 解析宿主侧 socket 路径（用于 bind-mount 到容器）。
///
/// 路径规则：`$XDG_RUNTIME_DIR/easytidy/<name>/server.sock`
///
/// 参数：
/// - name: 容器名
pub fn host_socket_path(name: &str) -> Result<PathBuf> {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
        .map_err(|_| Error::NoXdgRuntime)?;

    let socket_dir = PathBuf::from(runtime_dir)
        .join("easytidy")
        .join(name);

    // 确保父目录存在（调用方负责创建 server.sock 本身）
    Ok(socket_dir.join("server.sock"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// 串行化所有读写进程级环境变量的测试。`std::env::set_var`/`remove_var`
    /// 是进程全局的——Rust 测试并行多线程跑，`test_host_socket_path_no_xdg`
    /// 移除 XDG_RUNTIME_DIR 与 `test_host_socket_path` 读它并发踩踏（实测
    /// 偶发 NoXdgRuntime panic）；XDG_DATA_HOME 两个测试同理。共用一把锁
    /// 让环境相关的测试互斥执行。
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn test_host_socket_path() {
        let _guard = ENV_LOCK.lock().unwrap();
        let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap();
        let expected = PathBuf::from(runtime_dir)
            .join("easytidy")
            .join("test-container")
            .join("server.sock");

        let result = host_socket_path("test-container").unwrap();
        assert_eq!(result, expected);
    }

    #[test]
    fn test_host_socket_path_no_xdg() {
        let _guard = ENV_LOCK.lock().unwrap();
        // 临时 unset XDG_RUNTIME_DIR
        let original = std::env::var("XDG_RUNTIME_DIR").ok();
        std::env::remove_var("XDG_RUNTIME_DIR");

        let result = host_socket_path("test-container");
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), Error::NoXdgRuntime));

        // 恢复
        if let Some(val) = original {
            std::env::set_var("XDG_RUNTIME_DIR", val);
        }
    }

    #[test]
    fn test_server_binary_path_with_temp_xdg() {
        let _guard = ENV_LOCK.lock().unwrap();
        // 临时设置 XDG_DATA_HOME
        let original = std::env::var("XDG_DATA_HOME").ok();
        let temp_dir = tempfile::tempdir().unwrap();
        let bin_dir = temp_dir.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();

        // 创建假 binary
        let bin_path = bin_dir.join("easytidy-server");
        std::fs::write(&bin_path, "").unwrap();

        std::env::set_var("XDG_DATA_HOME", temp_dir.path());

        let result = server_binary_path().unwrap();
        assert_eq!(result, bin_path);

        // 恢复
        if let Some(val) = original {
            std::env::set_var("XDG_DATA_HOME", val);
        } else {
            std::env::remove_var("XDG_DATA_HOME");
        }
    }

    #[test]
    fn test_server_binary_path_missing() {
        let _guard = ENV_LOCK.lock().unwrap();
        // 临时设置 XDG_DATA_HOME 到空目录
        let original = std::env::var("XDG_DATA_HOME").ok();
        let temp_dir = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_DATA_HOME", temp_dir.path());

        let result = server_binary_path();
        assert!(result.is_err());

        // 恢复
        if let Some(val) = original {
            std::env::set_var("XDG_DATA_HOME", val);
        } else {
            std::env::remove_var("XDG_DATA_HOME");
        }
    }
}
