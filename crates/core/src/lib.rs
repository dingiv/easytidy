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
pub mod conf_template;
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

/// 解析 server 二进制路径（宿主侧）。
///
/// 候选按顺序返回第一个存在的：
/// 1. `SERVER_BIN` namespace **dev 根**（dev 构建树：cargo run / cargo test /
///    tauri dev 下 `beforeDevCommand` 已 `cargo build -p easytidy-server`
///    → workspace `target/debug`）
/// 2. `SERVER_BIN` namespace **prod 根**（easytidy build-server 默认安装位
///    `~/.local/share/easytidy/bin`）
/// 3. `$XDG_DATA_HOME/easytidy/bin/easytidy-server`（scripts/build-server.sh
///    尊重 XDG_DATA_HOME 的兼容回退）
///
/// 返回 helpful error 如果二进制不存在（提示运行 `easytidy build-server`）。
pub fn server_binary_path() -> Result<PathBuf> {
    let loader = easytidy_shared::loader!();
    let mut candidates: Vec<PathBuf> = Vec::new();
    candidates.extend(loader.ns_candidates("SERVER_BIN", "easytidy-server"));
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        candidates.push(PathBuf::from(xdg).join("easytidy/bin/easytidy-server"));
    }
    candidates.iter().find(|p| p.exists()).cloned().ok_or_else(|| {
        let tried = candidates
            .iter()
            .map(|p| format!("  {}", p.display()))
            .collect::<Vec<_>>()
            .join("\n");
        Error::Connect(format!(
            "server 二进制不存在（已尝试：\n{tried}）\n请先运行：easytidy build-server"
        ))
    })
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
    fn test_server_binary_path_missing() {
        let _guard = ENV_LOCK.lock().unwrap();
        // HOME/XDG_DATA_HOME 都指到空临时目录：ns_prod、XDG 候选必然 miss
        // （ns_dev 指向 workspace target/debug，`cargo test -p easytidy-core`
        // 单独跑时不会预先 build server 二进制）。守卫:dev 二进制若已存在则跳过——
        // 真·缺失语义只在「没 build 过的干净 target」上成立。
        let dev_binary_present = PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../target/debug/easytidy-server"
        ))
        .exists();
        if dev_binary_present {
            eprintln!("skip: dev server 二进制已存在于 target/debug，缺失用例环境不成立");
            return;
        }

        let original_home = std::env::var("HOME").ok();
        let original_xdg = std::env::var("XDG_DATA_HOME").ok();

        let empty_home = tempfile::tempdir().unwrap();
        let empty_data = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", empty_home.path());
        std::env::set_var("XDG_DATA_HOME", empty_data.path());

        let result = server_binary_path();
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("build-server"), "错误应提示 build-server：{msg}");

        if let Some(val) = original_home {
            std::env::set_var("HOME", val);
        } else {
            std::env::remove_var("HOME");
        }
        if let Some(val) = original_xdg {
            std::env::set_var("XDG_DATA_HOME", val);
        } else {
            std::env::remove_var("XDG_DATA_HOME");
        }
    }

    #[test]
    fn test_server_binary_path_respects_xdg_data_home() {
        let _guard = ENV_LOCK.lock().unwrap();
        // XDG 兼容回退：build-server.sh 装在 $XDG_DATA_HOME/easytidy/bin 时，
        // helper 应命中该处（ns_dev/ns_prod 均 miss：dev 未 build + HOME 隔离）。
        let dev_binary_present = PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../target/debug/easytidy-server"
        ))
        .exists();
        if dev_binary_present {
            eprintln!("skip: dev server 二进制已存在于 target/debug，XDG 用例环境不成立");
            return;
        }

        let original_home = std::env::var("HOME").ok();
        let original_xdg = std::env::var("XDG_DATA_HOME").ok();

        // 隔离 ns_prod（~/.local/share）：HOME 指到临时目录
        let isolated_home = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", isolated_home.path());
        // 假二进制装在 $XDG_DATA_HOME/easytidy/bin/
        let data_home = tempfile::tempdir().unwrap();
        let bin_dir = data_home.path().join("easytidy/bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        std::fs::write(bin_dir.join("easytidy-server"), "").unwrap();
        std::env::set_var("XDG_DATA_HOME", data_home.path());

        let result = server_binary_path().unwrap();
        assert_eq!(result, bin_dir.join("easytidy-server"));

        if let Some(val) = original_home {
            std::env::set_var("HOME", val);
        } else {
            std::env::remove_var("HOME");
        }
        if let Some(val) = original_xdg {
            std::env::set_var("XDG_DATA_HOME", val);
        } else {
            std::env::remove_var("XDG_DATA_HOME");
        }
    }
}
