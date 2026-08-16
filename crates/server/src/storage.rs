//! 持久层：容器内数据目录 `/home/easytidy` 的管理与读写原语。
//!
//! server 在容器内持久保留数据与配置（容器层文件，重启保留）：
//!
//! ```text
//! /home/easytidy/
//! ├── config.json   # server 配置（config.get/set 服务的数据源）
//! └── ...           #（后续数据文件按需扩展）
//! ```
//!
//! 历史：配置曾存 `/run/easytidy/config.json`（tmpfs，容器重启即丢）——
//! 启动时一次性迁移（旧文件存在且新路径缺失时搬移后删除旧文件）。

use std::path::PathBuf;

use anyhow::{Context, Result};

/// 容器内 easytidy 数据根目录。
pub(crate) const DATA_DIR: &str = "/home/easytidy";

/// 旧配置路径（tmpfs；仅作迁移源）。
const LEGACY_CONFIG_PATH: &str = "/run/easytidy/config.json";

/// 配置文件路径（/home/easytidy/config.json）。
pub(crate) fn config_path() -> PathBuf {
    PathBuf::from(DATA_DIR).join("config.json")
}

/// 启动初始化：建数据目录 + 迁移旧配置。幂等。
pub(crate) fn init() {
    if let Err(e) = std::fs::create_dir_all(DATA_DIR) {
        tracing::error!("创建数据目录 {DATA_DIR} 失败（权限不足？配置将不可持久化）：{e}");
        return;
    }
    // 一次性迁移：/run 旧配置存在且新路径缺失 → 搬移
    let new_path = config_path();
    let legacy = PathBuf::from(LEGACY_CONFIG_PATH);
    if legacy.exists() && !new_path.exists() {
        match std::fs::rename(&legacy, &new_path) {
            Ok(()) => tracing::info!("配置已迁移：{LEGACY_CONFIG_PATH} → {}", new_path.display()),
            Err(e) => tracing::warn!("配置迁移失败（忽略，以新路径为准）：{e}"),
        }
    }
    tracing::info!("数据目录就绪：{DATA_DIR}");
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
