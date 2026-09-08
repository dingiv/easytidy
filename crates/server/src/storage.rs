//! 持久层：容器内数据目录 `{home}/.easytidy` 的统一管理与读写原语。
//!
//! server 在容器内持久保留的全部数据与配置都收拢在 `{home}/.easytidy` 一个根下
//! （容器层文件，重启保留；不在各处散写）：
//!
//! ```text
//! {home}/.easytidy/
//! ├── config.json          # server 配置（config.get/set 服务的数据源）
//! ├── passthrough.toml     # 应用列表 + 收藏（passthrough 服务的数据源）
//! └── icons/               # 自定义应用图标（宿主导入复制进容器的图片）
//!     └── <原文件名>.<ext>
//! ```
//!
//! 历史迁移（启动时一次性搬移，旧文件存在且新路径缺失时搬移后删除旧文件）：
//! - config.json：曾存 `/run/easytidy/config.json`（tmpfs，重启即丢）与 `/home/easytidy/config.json`
//! - passthrough.toml：曾存 `{home}/.config/easytidy/passthrough.toml`（XDG 风格，4c0b31c 起）
//! - icons：曾存 `{home}/.local/share/icons/easytidy/`（XDG icons 位置）
//!
//! 注：本模块只做**与格式无关**的文件/目录搬移（rename）；passthrough 配置里
//! 绝对图标路径的**格式感知改写**由 `services::passthrough` 负责（它拥有该格式）。

use std::sync::OnceLock;
use std::path::PathBuf;

use anyhow::{Context, Result};

/// 旧数据根目录（旧 root 模型固定用户名；仅作迁移源）。
const LEGACY_DATA_DIR: &str = "/home/easytidy";

/// 旧配置路径（tmpfs；仅作迁移源）。
const LEGACY_CONFIG_PATH: &str = "/run/easytidy/config.json";

/// 旧 passthrough 配置相对 home 的路径（XDG 风格；仅作迁移源）。
const LEGACY_PASSTHROUGH_REL: &str = ".config/easytidy/passthrough.toml";

/// 旧自定义应用图标目录相对 home 的路径（XDG icons 位置；仅作迁移源）。
const LEGACY_ICONS_REL: &str = ".local/share/icons/easytidy";

/// 数据根目录全局态（init 后写入；`{home}/.easytidy`，随身份——
/// server 以配置 uid 运行时 /home 常为 root:755 不可写，不能再用
/// 固定用户家目录）。
static DATA_DIR: OnceLock<String> = OnceLock::new();

/// 当前数据根目录（init 未执行时回退旧路径，仅防御极早期调用）。
fn data_dir() -> &'static str {
    static FALLBACK: &str = LEGACY_DATA_DIR;
    DATA_DIR.get().map(|s| s.as_str()).unwrap_or(FALLBACK)
}

/// 配置文件路径（{home}/.easytidy/config.json）。
pub(crate) fn config_path() -> PathBuf {
    PathBuf::from(data_dir()).join("config.json")
}

/// passthrough 配置路径（{home}/.easytidy/passthrough.toml）。
pub(crate) fn passthrough_path() -> PathBuf {
    PathBuf::from(data_dir()).join("passthrough.toml")
}

/// 应用登记表路径（{home}/.easytidy/apps.toml）：扫描得到的 .desktop
/// 应用（含稳定 id），每次扫描全量重写（server 生成数据，与用户态
/// passthrough.toml 分离）。
pub(crate) fn apps_registry_path() -> PathBuf {
    PathBuf::from(data_dir()).join("apps.toml")
}

/// 自定义应用图标目录（{home}/.easytidy/icons）。
pub(crate) fn icons_dir() -> PathBuf {
    PathBuf::from(data_dir()).join("icons")
}

/// 启动初始化：建数据目录 + 迁移旧配置。幂等。
///
/// `home` = 容器默认用户 home（身份自发现结果）。
pub(crate) fn init(home: &str) {
    let dir = format!("{home}/.easytidy");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::error!("创建数据目录 {dir} 失败（权限不足？配置将不可持久化）：{e}");
        return;
    }
    let _ = DATA_DIR.set(dir.clone());
    // 一次性迁移：/run 旧配置存在且新路径缺失 → 搬移
    let new_path = config_path();
    let legacy = PathBuf::from(LEGACY_CONFIG_PATH);
    if legacy.exists() && !new_path.exists() {
        match std::fs::rename(&legacy, &new_path) {
            Ok(()) => tracing::info!("配置已迁移：{LEGACY_CONFIG_PATH} → {}", new_path.display()),
            Err(e) => tracing::warn!("配置迁移失败（忽略，以新路径为准）：{e}"),
        }
    }
    // 一次性迁移：旧 /home/easytidy/config.json 存在且新路径缺失 → 搬移
    let legacy_home = PathBuf::from(LEGACY_DATA_DIR).join("config.json");
    if legacy_home.exists() && !new_path.exists() {
        match std::fs::rename(&legacy_home, &new_path) {
            Ok(()) => tracing::info!(
                "配置已迁移：{} → {}",
                legacy_home.display(),
                new_path.display()
            ),
            Err(e) => tracing::warn!("配置迁移失败（忽略，以新路径为准）：{e}"),
        }
    }
    // 一次性迁移：passthrough.toml 从旧 XDG 位置 .config/easytidy → 新 .easytidy
    let pt_new = passthrough_path();
    let pt_legacy = PathBuf::from(home).join(LEGACY_PASSTHROUGH_REL);
    if pt_legacy.exists() && !pt_new.exists() {
        if let Some(parent) = pt_new.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::rename(&pt_legacy, &pt_new) {
            Ok(()) => tracing::info!(
                "passthrough 配置已迁移：{} → {}",
                pt_legacy.display(),
                pt_new.display()
            ),
            Err(e) => tracing::warn!("passthrough 配置迁移失败（忽略）：{e}"),
        }
    }
    // 旧 XDG 子目录搬空后清理（仅当空；不碰 .config 父目录；幂等）
    if let Some(pt_legacy_dir) = pt_legacy.parent() {
        let _ = std::fs::remove_dir(pt_legacy_dir);
    }
    // 一次性迁移：自定义应用图标从旧 .local/share/icons/easytidy → 新 .easytidy/icons
    // （仅搬移文件；配置里绝对图标路径的格式感知改写由 passthrough 服务负责）
    let icons_new = icons_dir();
    let icons_legacy = PathBuf::from(home).join(LEGACY_ICONS_REL);
    if icons_legacy.exists() {
        let _ = std::fs::create_dir_all(&icons_new);
        let moved = match std::fs::read_dir(&icons_legacy) {
            Ok(entries) => entries
                .flatten()
                .filter(|e| e.path().is_file())
                .map(|e| {
                    let from = e.path();
                    let to = icons_new.join(e.file_name());
                    match std::fs::rename(&from, &to) {
                        Ok(()) => Some(from.display().to_string()),
                        Err(_) => None,
                    }
                })
                .count(),
            Err(e) => {
                tracing::warn!("读取旧图标目录失败（忽略）：{e}");
                0
            }
        };
        if moved > 0 {
            tracing::info!(
                "自定义应用图标已迁移：{} → {}（{} 个文件）",
                icons_legacy.display(),
                icons_new.display(),
                moved
            );
        }
    }
    // 旧图标子目录搬空后清理（仅当空；不碰 .local/share/icons 父目录；幂等）
    let _ = std::fs::remove_dir(&icons_legacy);
    tracing::info!("数据目录就绪：{dir}");
}

/// 读取配置全文（不存在返回 None）。
pub(crate) fn read_config() -> Result<Option<String>> {
    let path = config_path();
    if !path.exists() {
        return Ok(None);
    }
    std::fs::read_to_string(&path)
        .map(Some)
        .with_context(|| format!("读取配置失败：{}", path.display()))
}

/// 写入配置（临时文件 + rename 原子写，避免半截文件）。
pub(crate) fn write_config(content: &str) -> Result<()> {
    let path = config_path();
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, content)
        .with_context(|| format!("写入配置临时文件失败：{}", tmp.display()))?;
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("替换配置文件失败：{}", path.display()))?;
    Ok(())
}
