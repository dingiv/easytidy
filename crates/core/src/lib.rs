//! easytidy 宿主侧核心引擎。
//!
//! 被三个入口共用：Master GUI、Worker GUI、无头 CLI。
//! 职责：podman socket API 客户端（bollard + libpod 端点）、
//! 容器生命周期/快照、共享配置文件（原子写 + flock + 版本化）、
//! .desktop 生成、宿主 systemd unit 生成。
//!
//! 铁律：不调用任何 podman CLI（见 docs/08-requirements.md L2）。

use crate::error::{Error, Result};
use std::path::PathBuf;

/// 宿主导出 .desktop 的 Exec 前缀（passthrough 机制）
pub const EXEC_PREFIX: &str = "easytidy --container";

pub mod appdata;
pub mod conf_template;
pub mod configfile;
pub mod desktop;
pub mod env;
pub mod error;
pub mod events;
pub mod flavor;
pub mod guilock;
pub mod icon;
pub mod libpod;
pub mod models;
pub mod passthrough;
pub mod pathvars;
pub mod podman;
pub mod storage_health;
pub mod systemd;
pub mod userenv;

/// host socket 目录 = `$XDG_RUNTIME_DIR/easytidy/<name>`。
///
/// **按容器名寻址，不依赖 config**——一个容器一个 socket 目录，容器名唯一即路径
/// 唯一。不掺 config hash（曾按 `<name>-<config-hash>` 命名，但 `env` 是 session
/// 耦合的解析快照（DISPLAY/XDG_RUNTIME_DIR 等），hash 随会话漂移，而容器 source 在
/// 创建时固化为死值 → 两者分叉，重启后 `podman start` 挂不上的死源 → 500，
/// 2026-09-01 chrome 实例实测）。容器 source 与 [`host_socket_path`] /
/// `ensure_socket_dir` 都只依赖 name，永远一致。
pub(crate) fn socket_dir_for(name: &str) -> Result<PathBuf> {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").map_err(|_| Error::NoXdgRuntime)?;
    Ok(PathBuf::from(runtime_dir).join("easytidy").join(name))
}

/// 列出某容器名下所有已存在的 socket 目录（`$XDG_RUNTIME_DIR/easytidy/` 下
/// `== <name>`（新格式，纯 name）或以 `<name>-` 开头（旧 `<name>-<hash>` 格式，
/// 兼容存量容器），按 mtime 降序（最新代在前）。
///
/// 目录不存在/读不到 → 空列表（不报错：开机 XDG_RUNTIME_DIR 重建后目录本就可能缺席）。
fn resolve_socket_dirs(name: &str) -> Vec<PathBuf> {
    let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") else {
        return Vec::new();
    };
    let base = PathBuf::from(runtime_dir).join("easytidy");
    let Ok(read) = std::fs::read_dir(&base) else {
        return Vec::new();
    };
    let mut dirs: Vec<(std::time::SystemTime, PathBuf)> = read
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|e| {
            let file_name = e.file_name();
            let file_name = file_name.to_string_lossy();
            let matches = file_name == name || file_name.starts_with(&format!("{name}-"));
            if !matches {
                return None;
            }
            let modified = e
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            Some((modified, e.path()))
        })
        .collect();
    dirs.sort_by_key(|d| std::cmp::Reverse(d.0)); // 新 → 旧
    dirs.into_iter().map(|(_, p)| p).collect()
}

/// 解析宿主侧 socket 路径（bind-mount 源 / GUI、CLI、worker、autostart 重连）。
///
/// 路径规则：`$XDG_RUNTIME_DIR/easytidy/<name>-<config-hash>/server.sock`
/// （旧格式 `<name>/` 亦被识别）。
///
/// 解析策略（以「实际存在」为准，不依赖 config 注册时序，对漂移鲁棒）：
/// 1. 现有目录优先（glob `<name>-*` 或旧 `<name>`，取最新代——首启/正常在册命中）；
/// 2. 无现有目录（如开机 XDG_RUNTIME_DIR 重建后）→ 读 configfile 重算 hash 重建目录。
pub fn host_socket_path(name: &str) -> Result<PathBuf> {
    // 契约：XDG_RUNTIME_DIR 必须先存在（resolve 步骤依赖它，且旧语义要求
    // 缺失时给出 NoXdgRuntime —— 不可落到 config 分支才报 Connect）
    let _runtime_dir = std::env::var("XDG_RUNTIME_DIR").map_err(|_| Error::NoXdgRuntime)?;

    // 1. 现有目录优先（首启时 autostart 在 config 注册前触发，只能走这里；
    //    resolve 兼容旧 `<name>-<hash>` 格式的存量容器）
    if let Some(dir) = resolve_socket_dirs(name).into_iter().next() {
        return Ok(dir.join("server.sock"));
    }

    // 2. 无目录（开机 XDG_RUNTIME_DIR 重建后）→ 按容器名直接定位
    // （socket 目录 = easytidy/<name>，纯 name 寻址，不依赖 config 注册时序）
    Ok(socket_dir_for(name)?.join("server.sock"))
}

/// 清理某容器名**所有代**的 socket 目录（尽力而为：失败仅告警）。
///
/// env_rm 删容器、rebuild 换代后调用，避免旧代目录成孤儿。
pub fn remove_socket_dirs(name: &str) -> Result<()> {
    for dir in resolve_socket_dirs(name) {
        if let Err(e) = std::fs::remove_dir_all(&dir) {
            tracing::warn!("清理 socket 目录失败（忽略）：{}：{e}", dir.display());
        }
    }
    Ok(())
}

/// 解析 server 二进制路径（宿主侧）。
///
/// 候选按顺序返回第一个存在的：
/// 1. `SERVER_BIN` namespace **dev 根**（dev 构建树：cargo run / cargo test /
///    tauri dev 下 `beforeDevCommand` 已 `cargo build -p easytidy-server`
///    → workspace `target/debug`）
/// 2. `SERVER_BIN` namespace **prod 根**（发布**随包安装位**：server 随安装包一起
///    发布，无 build-server。deb 打包装到 `/usr/bin`（或 per-user
///    `~/.local/share/easytidy`）——打包阶段在 core Cargo.toml 配置该值）
/// 3. `$XDG_DATA_HOME/easytidy/bin/easytidy-server`（历史兼容回退）
///
/// 返回 helpful error 如果二进制不存在（提示随安装包安装）。
pub fn server_binary_path() -> Result<PathBuf> {
    let loader = easytidy_shared::loader!();
    let mut candidates: Vec<PathBuf> = Vec::new();
    candidates.extend(loader.ns_candidates("SERVER_BIN", "easytidy-server"));
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        candidates.push(PathBuf::from(xdg).join("easytidy/bin/easytidy-server"));
    }
    candidates
        .iter()
        .find(|p| p.exists())
        .cloned()
        .ok_or_else(|| {
            let tried = candidates
                .iter()
                .map(|p| format!("  {}", p.display()))
                .collect::<Vec<_>>()
                .join("\n");
            Error::Connect(format!(
                "server 二进制不存在（已尝试：\n{tried}）\n请确保已随安装包安装 easytidy-server"
            ))
        })
}

/// 解析 dock 二进制路径（宿主侧）。
///
/// 与 [`server_binary_path`] 完全同构：`DOCK_BIN` namespace dev/prod
/// 候选 + `$XDG_DATA_HOME/easytidy/bin` 历史兼容回退。dock 是容器内
/// root 工具（`/run/easytidy-bin/easytidy-dock`：prepare 容器准备 +
/// daemon/client root 终端通道，源自 root-channel + ctool 合并），与
/// server 同为 musl 静态二进制（同一构建目标，见 DOCK_BIN namespace 注释）。
pub fn dock_binary_path() -> Result<PathBuf> {
    let loader = easytidy_shared::loader!();
    let mut candidates: Vec<PathBuf> = Vec::new();
    candidates.extend(loader.ns_candidates("DOCK_BIN", "easytidy-dock"));
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        candidates.push(PathBuf::from(xdg).join("easytidy/bin/easytidy-dock"));
    }
    candidates
        .iter()
        .find(|p| p.exists())
        .cloned()
        .ok_or_else(|| {
            let tried = candidates
                .iter()
                .map(|p| format!("  {}", p.display()))
                .collect::<Vec<_>>()
                .join("\n");
            Error::Connect(format!(
                "easytidy-dock 二进制不存在（已尝试：\n{tried}）\n请确保已随安装包安装"
            ))
        })
}

/// `ets` 二进制位置（容器内 easytidy-server 命令行客户端，musl 静态）。
///
/// 与 server/dock 同构：`ETS_BIN` namespace dev/prod 候选 + XDG 历史回退。
/// **允许缺失**：老宿主构建没有 ets 二进制时返回 Err，调用方（
/// [`ContainerBins::resolve`]）降级为 `None`——容器不挂载、prepare 不建软链，
/// 功能优雅缺省，不阻断容器创建。
pub fn ets_binary_path() -> Result<PathBuf> {
    let loader = easytidy_shared::loader!();
    let mut candidates: Vec<PathBuf> = Vec::new();
    candidates.extend(loader.ns_candidates("ETS_BIN", "ets"));
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        candidates.push(PathBuf::from(xdg).join("easytidy/bin/ets"));
    }
    candidates.iter().find(|p| p.exists()).cloned().ok_or_else(|| {
        let tried = candidates
            .iter()
            .map(|p| format!("  {}", p.display()))
            .collect::<Vec<_>>()
            .join("\n");
        Error::Connect(format!(
            "ets 二进制不存在（已尝试：\n{tried}）\n容器内将无 ets 命令（可重新构建后 rebuild 容器）"
        ))
    })
}

/// 容器内二进制（ro bind-mount 进容器 `/run/easytidy-bin/`）：server（常驻）
/// + dock（容器内 root 工具：prepare 容器准备 + daemon/client root 终端
///   通道）+ ets（容器内 server 命令行客户端，可选）。
///   `create_with_config`/`rebuild` 的统一输入。
#[derive(Debug, Clone)]
pub struct ContainerBins {
    /// → `/run/easytidy-bin/easytidy-server`
    pub server: PathBuf,
    /// → `/run/easytidy-bin/easytidy-dock`
    ///
    /// 2026-09-02：由 root-channel 与 ctool 合并而来，跑在**容器内**，由宿主
    /// GUI/CLI 通过 `podman exec --user 0 <container> <bin> {prepare|bootstrap|client ...}`
    /// 拉起。容器内 daemon 持有 0..N 个 root bash + PTY session；多 client 可
    /// 同时 attach 同一 session（fan-out + 128KB 回放）。daemon 与容器共死，
    /// **零主机端残留**。
    pub dock: PathBuf,
    /// → `/run/easytidy-bin/ets`（容器内命令行客户端；None = 宿主未安装，
    /// 不挂载不软链，容器内无 ets 命令）
    pub ets: Option<PathBuf>,
}

impl ContainerBins {
    /// 宿主侧解析二进制的安装位置（纯路径解析，无副作用）。
    ///
    /// server/dock 缺失 → 报错（核心依赖）；ets 缺失 → None（优雅降级）。
    pub fn resolve() -> Result<Self> {
        Ok(Self {
            server: server_binary_path()?,
            dock: dock_binary_path()?,
            ets: ets_binary_path().ok(),
        })
    }
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

    // （旧的 `test_host_socket_path` 测「纯路径推导」，契约已随 resolver 化作废：
    //  host_socket_path 现按「现有目录 → configfile hash」解析，覆盖见
    //  test_socket_dir_resolution_prefers_existing 与下方 no_xdg 用例。）
    #[test]
    fn test_host_socket_path_no_xdg() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        assert!(msg.contains("安装"), "错误应提示随包安装：{msg}");

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
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // XDG 历史兼容回退：server 装在 $XDG_DATA_HOME/easytidy/bin 时，
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

    #[test]
    fn test_socket_dir_for_uses_container_name() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // socket 目录 = easytidy/<name>，纯 name 寻址（不依赖 config，避免漂移）
        let original = std::env::var("XDG_RUNTIME_DIR").ok();
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_RUNTIME_DIR", tmp.path());
        let dir = socket_dir_for("chrome").unwrap();
        assert!(
            dir.ends_with("easytidy/chrome"),
            "应为 $XDG_RUNTIME_DIR/easytidy/chrome：{dir:?}"
        );
        if let Some(val) = original {
            std::env::set_var("XDG_RUNTIME_DIR", val);
        } else {
            std::env::remove_var("XDG_RUNTIME_DIR");
        }
    }

    #[test]
    fn test_socket_dir_resolution_prefers_existing() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let original = std::env::var("XDG_RUNTIME_DIR").ok();
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_RUNTIME_DIR", tmp.path());

        // 新格式 `<name>-<hash>` + 旧格式 `<name>` 各建一个（后者更新 → resolve 最新在前）
        let base = tmp.path().join("easytidy");
        let new_dir = base.join("chrome-a1b2c3d4");
        let old_dir = base.join("chrome");
        std::fs::create_dir_all(&new_dir).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::create_dir_all(&old_dir).unwrap();

        let dirs = resolve_socket_dirs("chrome");
        assert_eq!(dirs.len(), 2, "新格式与旧格式目录都应被识别");
        assert_eq!(dirs[0], old_dir, "最新 mtime 的目录应排在首位");

        // host_socket_path 走现有目录（不依赖 config 注册）
        let p = host_socket_path("chrome").unwrap();
        assert_eq!(p, old_dir.join("server.sock"));

        if let Some(val) = original {
            std::env::set_var("XDG_RUNTIME_DIR", val);
        } else {
            std::env::remove_var("XDG_RUNTIME_DIR");
        }
    }

    #[test]
    fn test_remove_socket_dirs_removes_all_generations() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let original = std::env::var("XDG_RUNTIME_DIR").ok();
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_RUNTIME_DIR", tmp.path());

        let base = tmp.path().join("easytidy");
        let dirs = vec![
            base.join("chrome-a1b2c3d4"),
            base.join("chrome"),
            base.join("chrome-ff001122"),
        ];
        for d in &dirs {
            std::fs::create_dir_all(d).unwrap();
        }
        assert_eq!(resolve_socket_dirs("chrome").len(), 3);

        remove_socket_dirs("chrome").unwrap();
        assert!(
            dirs.iter().all(|d| !d.exists()),
            "所有代的 socket 目录都应被清理"
        );

        if let Some(val) = original {
            std::env::set_var("XDG_RUNTIME_DIR", val);
        } else {
            std::env::remove_var("XDG_RUNTIME_DIR");
        }
    }
}
