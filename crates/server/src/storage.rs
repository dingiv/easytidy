//! 持久层：容器内数据目录 `{home}/.easytidy` 的管理与读写原语。
//!
//! server 在容器内持久保留数据与配置（容器层文件，重启保留）：
//!
//! ```text
//! {home}/.easytidy/
//! ├── config.json   # server 配置（config.get/set 服务的数据源）
//! └── ...           #（后续数据文件按需扩展）
//! ```
//!
//! 历史：
//! - 配置曾存 `/run/easytidy/config.json`（tmpfs，容器重启即丢）
//! - 数据目录曾硬编码 `/home/easytidy`（旧 root 模型固定用户名）
//!
//! 两者均已迁移（启动时一次性搬移，旧文件存在且新路径缺失时搬移后删除旧文件）。

use std::sync::OnceLock;
use std::path::PathBuf;

use anyhow::{Context, Result};

/// 旧数据根目录（旧 root 模型固定用户名；仅作迁移源）。
const LEGACY_DATA_DIR: &str = "/home/easytidy";

/// 旧配置路径（tmpfs；仅作迁移源）。
const LEGACY_CONFIG_PATH: &str = "/run/easytidy/config.json";

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
