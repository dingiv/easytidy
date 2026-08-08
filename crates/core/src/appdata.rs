//! 应用数据目录（`~/.easytidy`）：图标缓存 + 配置文件统一放这里。
//!
//! 宿主侧集中目录，方便用户查看/备份/清理；桌面入口 `Icon=` 也指向这里
//! （图标放私有目录不依赖 hicolor 主题，GNOME 也能解析绝对路径）。
//! 旧版本（v0.1）配置在 `$XDG_CONFIG_HOME/easytidy/`，首次使用自动复制迁移
//! （只复制不删除，回滚友好）。

use std::path::PathBuf;

use crate::error::{Error, Result};

/// 应用数据根目录（`~/.easytidy`；HOME 不可用时回退 `$XDG_DATA_HOME/easytidy`）
pub fn app_data_dir() -> Result<PathBuf> {
    if let Some(home) = dirs::home_dir() {
        return Ok(home.join(".easytidy"));
    }
    dirs::data_local_dir()
        .map(|d| d.join("easytidy"))
        .ok_or_else(|| Error::Config("无法确定宿主主目录".to_string()))
}

/// 图标缓存目录（`~/.easytidy/icons`，不存在则创建）
pub fn icons_dir() -> Result<PathBuf> {
    let dir = app_data_dir()?.join("icons");
    std::fs::create_dir_all(&dir)
        .map_err(|e| Error::Config(format!("创建图标目录失败：{e}")))?;
    Ok(dir)
}

/// 主配置文件路径（`~/.easytidy/config.toml`）
pub fn config_file_path() -> Result<PathBuf> {
    Ok(app_data_dir()?.join("config.toml"))
}

/// passthrough 配置文件路径（`~/.easytidy/passthrough.toml`）
pub fn passthrough_config_path() -> Result<PathBuf> {
    Ok(app_data_dir()?.join("passthrough.toml"))
}

/// 一次性迁移：`$XDG_CONFIG_HOME/easytidy/` 下旧配置/flavors → 新目录
/// （新路径不存在且旧路径存在时复制；幂等，可反复调用）。
pub fn migrate_legacy_configs() {
    let Ok(new_dir) = app_data_dir() else {
        return;
    };
    let Some(legacy_dir) = dirs::config_dir().map(|d| d.join("easytidy")) else {
        return;
    };
    if !legacy_dir.exists() {
        return;
    }

    // 配置文件
    for name in ["config.toml", "passthrough.toml"] {
        let legacy = legacy_dir.join(name);
        let new = new_dir.join(name);
        if legacy.exists() && !new.exists() {
            let _ = std::fs::create_dir_all(&new_dir);
            if let Ok(bytes) = std::fs::read(&legacy) {
                if std::fs::write(&new, &bytes).is_ok() {
                    tracing::info!("迁移旧配置：{} → {}", legacy.display(), new.display());
                }
            }
        }
    }

    // flavors 模板目录（整体复制）
    let legacy_flavors = legacy_dir.join("flavors");
    let new_flavors = new_dir.join("flavors");
    if legacy_flavors.exists() && !new_flavors.exists() {
        if let Ok(entries) = std::fs::read_dir(&legacy_flavors) {
            let _ = std::fs::create_dir_all(&new_flavors);
            for entry in entries.flatten() {
                let src = entry.path();
                let dst = new_flavors.join(entry.file_name());
                if let Ok(bytes) = std::fs::read(&src) {
                    let _ = std::fs::write(&dst, &bytes);
                }
            }
        }
    }
}
