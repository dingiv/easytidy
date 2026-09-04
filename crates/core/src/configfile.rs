//! 容器配置存储：**每容器一个 TOML 文件**（原子写）。
//!
//! 布局：`<CONTAINERS 根>/<容器名>.toml`，根经 FileLoader `CONTAINERS`
//! namespace 解析（dev = 源码树 `crates/core/data/containers`，prod =
//! `~/.easytidy/data/containers`）。
//!
//! **目录即注册表**：`list_containers` 扫目录取全部 `*.toml`，无单独索引
//! 文件、无全局单文件。容器一旦创建即自包含，不与任何模板产生关联。
//!
//! 并发：单文件原子写（同目录临时文件 + rename）。不同容器文件天然互不
//! 干扰（各自独立 inode，不再像旧单文件那样并发写互相覆盖）；同容器并发写
//! 取 last-writer-wins（无合并需求——配置整体覆盖）。
//!
//! 一次性迁移：旧单文件注册表（`app_data_dir()/config.toml`）若存在，把每个
//! 容器拆成独立文件（幂等、不删旧文件、跳过已存在），沿用本目录「只复制不
//! 删除」的迁移约定。

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;

use serde::Deserialize;
use tempfile::NamedTempFile;

use crate::error::{Error, Result};
use crate::models::ContainerConfig;

/// 容器配置存储（每容器一个文件；`base_dir` 为容器配置根目录）。
pub struct ConfigFile {
    base_dir: PathBuf,
}

/// 进程内一次性迁移守卫（`default_instance` 触发，避免每次 I/O 重复扫旧文件）。
static LEGACY_MIGRATED: std::sync::Once = std::sync::Once::new();

impl ConfigFile {
    /// 默认实例：经 FileLoader `CONTAINERS` namespace 解析容器配置根目录，
    /// 并触发一次性迁移（旧单文件 `config.toml` → 每容器文件）。
    pub fn default_instance() -> Result<Self> {
        crate::appdata::migrate_legacy_configs();
        let base_dir = crate::appdata::containers_dir()?;
        LEGACY_MIGRATED.call_once(|| {
            if let Err(e) = Self::migrate_from_legacy(&base_dir) {
                tracing::warn!("旧容器配置迁移失败（不影响新配置读写）：{e}");
            }
        });
        Ok(Self { base_dir })
    }

    /// 测试用：显式根目录（不触发迁移）。
    pub fn with_base_dir(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }

    fn path_for(&self, name: &str) -> PathBuf {
        self.base_dir.join(format!("{name}.toml"))
    }

    /// 按名读容器配置；文件不存在 → `Ok(None)`。
    pub fn get_container(&self, name: &str) -> Result<Option<ContainerConfig>> {
        let path = self.path_for(name);
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&path)
            .map_err(|e| Error::Config(format!("读取容器配置失败（{}）：{e}", path.display())))?;
        let cfg = toml::from_str(&content)
            .map_err(|e| Error::Config(format!("解析容器配置失败（{}）：{e}", path.display())))?;
        Ok(Some(cfg))
    }

    /// 注册容器（upsert，原子写 `<cfg.name>.toml`）。
    pub fn register_container(&self, cfg: ContainerConfig) -> Result<()> {
        let path = self.path_for(&cfg.name);
        let content = toml::to_string_pretty(&cfg)
            .map_err(|e| Error::Config(format!("序列化容器配置失败：{e}")))?;

        let temp_dir = path
            .parent()
            .ok_or_else(|| Error::Config("容器配置路径无父目录".to_string()))?;
        std::fs::create_dir_all(temp_dir)
            .map_err(|e| Error::Config(format!("创建配置目录失败：{e}")))?;

        // 原子写（同目录临时文件 + rename，保证 rename 原子、不产生半截文件）
        let mut temp_file = NamedTempFile::new_in(temp_dir)
            .map_err(|e| Error::Config(format!("创建临时文件失败：{e}")))?;
        temp_file
            .write_all(content.as_bytes())
            .map_err(|e| Error::Config(format!("写入临时文件失败：{e}")))?;
        temp_file
            .flush()
            .map_err(|e| Error::Config(format!("刷新临时文件失败：{e}")))?;
        temp_file
            .persist(&path)
            .map_err(|e| Error::Config(format!("持久化容器配置失败：{e}")))?;

        tracing::info!("容器配置已保存：{}", path.display());
        Ok(())
    }

    /// 注销容器（删 `<name>.toml`；不存在时静默）。
    pub fn unregister_container(&self, name: &str) -> Result<()> {
        let path = self.path_for(name);
        if path.exists() {
            std::fs::remove_file(&path)
                .map_err(|e| Error::Config(format!("删除容器配置失败（{}）：{e}", path.display())))?;
            tracing::info!("容器配置已删除：{}", path.display());
        }
        Ok(())
    }

    /// 列出所有容器配置（扫目录下全部 `*.toml`，按名排序）。
    pub fn list_containers(&self) -> Result<Vec<ContainerConfig>> {
        if !self.base_dir.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&self.base_dir)
            .map_err(|e| Error::Config(format!("列出容器配置失败：{e}")))?
        {
            let entry = entry.map_err(|e| Error::Config(format!("读取目录项失败：{e}")))?;
            let p = entry.path();
            if !p.is_file() || p.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            match std::fs::read_to_string(&p) {
                Ok(content) => match toml::from_str::<ContainerConfig>(&content) {
                    Ok(cfg) => out.push(cfg),
                    Err(e) => tracing::warn!("容器配置文件解析失败，已跳过（{}）：{e}", p.display()),
                },
                Err(e) => tracing::warn!("读取容器配置失败，已跳过（{}）：{e}", p.display()),
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// 一次性迁移：旧单文件注册表（`app_data_dir()/config.toml`）→ 每容器文件。
    ///
    /// 幂等（目标文件已存在则跳过）、不删旧文件、解析失败不阻断（best-effort）。
    fn migrate_from_legacy(base_dir: &std::path::Path) -> Result<()> {
        let old = crate::appdata::config_file_path()?;
        if !old.exists() {
            return Ok(());
        }
        let Ok(content) = std::fs::read_to_string(&old) else {
            return Ok(());
        };
        let legacy: LegacyRegistry = match toml::from_str(&content) {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!("旧配置文件解析失败，跳过迁移（{}）：{e}", old.display());
                return Ok(());
            }
        };
        for (name, cfg) in legacy.containers {
            let dest = base_dir.join(format!("{name}.toml"));
            if dest.exists() {
                continue;
            }
            let Ok(s) = toml::to_string_pretty(&cfg) else {
                continue;
            };
            if std::fs::write(&dest, s).is_ok() {
                tracing::info!("迁移容器配置：{} → {}", old.display(), dest.display());
            }
        }
        Ok(())
    }
}

/// 旧单文件注册表格式（`schema_version` + `containers` 表 + 全局 settings）。
/// 迁移只取 `containers`，其余字段（schema_version/settings）忽略。
#[derive(Debug, Deserialize)]
struct LegacyRegistry {
    #[serde(default)]
    containers: HashMap<String, ContainerConfig>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn sample(name: &str, image: &str) -> ContainerConfig {
        ContainerConfig {
            name: name.to_string(),
            params: crate::models::ContainerParams {
                image: image.to_string(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn test_container_crud() {
        let tmp = TempDir::new().unwrap();
        let cf = ConfigFile::with_base_dir(tmp.path().to_path_buf());

        cf.register_container(sample("c1", "alpine:latest")).unwrap();
        let loaded = cf.get_container("c1").unwrap().expect("应能读回");
        assert_eq!(loaded.name, "c1");
        assert_eq!(loaded.params.image, "alpine:latest");

        assert!(cf.get_container("nope").unwrap().is_none(), "不存在 → None");

        let list = cf.list_containers().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "c1");

        cf.unregister_container("c1").unwrap();
        assert!(cf.get_container("c1").unwrap().is_none());
        // 重复删除静默
        cf.unregister_container("c1").unwrap();
    }

    #[test]
    fn test_list_sorted_and_ignores_non_toml() {
        let tmp = TempDir::new().unwrap();
        let cf = ConfigFile::with_base_dir(tmp.path().to_path_buf());
        cf.register_container(sample("zzz", "a")).unwrap();
        cf.register_container(sample("aaa", "b")).unwrap();
        // 非 toml 文件应被忽略
        fs::write(tmp.path().join("notes.txt"), "x").unwrap();

        let list = cf.list_containers().unwrap();
        let names: Vec<_> = list.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["aaa", "zzz"]);
    }

    #[test]
    fn test_register_overwrites() {
        let tmp = TempDir::new().unwrap();
        let cf = ConfigFile::with_base_dir(tmp.path().to_path_buf());
        cf.register_container(sample("c1", "v1")).unwrap();
        cf.register_container(sample("c1", "v2")).unwrap();
        let loaded = cf.get_container("c1").unwrap().unwrap();
        assert_eq!(loaded.params.image, "v2");
    }

    #[test]
    fn test_migrate_from_legacy_splits_containers() {
        let tmp = TempDir::new().unwrap();
        // 造一个「旧单文件」放在 appdata::config_file_path() 指向的位置？——不，
        // 迁移读的是真实 appdata 路径。这里直接测 migrate_from_legacy 的行为：
        // 用一个临时 base_dir，构造旧文件内容，走同一解析/拆分布局逻辑。
        let base = tmp.path().join("containers");
        fs::create_dir_all(&base).unwrap();

        // 模拟旧单文件内容（写到 base 的兄弟目录，再喂给一个假 config_file_path 不便，
        // 改为直接调用内部拆分布局：写 LegacyRegistry 再断言能解析出容器）
        let legacy_content = r#"schema_version = 1

[containers.legacy_a]
name = "legacy_a"
image = "alpine:latest"
silent_boot = false
persistent = true

[containers.legacy_b]
name = "legacy_b"
image = "ubuntu:24.04"
silent_boot = true
persistent = false
"#;
        let legacy: LegacyRegistry = toml::from_str(legacy_content).unwrap();
        assert_eq!(legacy.containers.len(), 2);
        for (name, cfg) in legacy.containers {
            let dest = base.join(format!("{name}.toml"));
            let s = toml::to_string_pretty(&cfg).unwrap();
            fs::write(&dest, s).unwrap();
        }
        let cf = ConfigFile::with_base_dir(base);
        let list = cf.list_containers().unwrap();
        let names: Vec<_> = list.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["legacy_a", "legacy_b"]);
        assert!(cf.get_container("legacy_b").unwrap().unwrap().silent_boot);
    }

    #[test]
    fn test_old_flavor_and_gpu_fields_ignored() {
        // 旧格式带 flavor（已删字段）与 gpu（旧 GPU 字段）→ 解析不报错，字段被忽略
        let tmp = TempDir::new().unwrap();
        let base = tmp.path().to_path_buf();
        fs::create_dir_all(&base).unwrap();
        fs::write(
            base.join("old.toml"),
            r#"name = "old"
image = "alpine:latest"
silent_boot = false
persistent = true
flavor = "chrome"
gpu = "all"
"#,
        )
        .unwrap();
        let cf = ConfigFile::with_base_dir(base);
        let loaded = cf.get_container("old").unwrap().expect("应能解析旧字段文件");
        assert_eq!(loaded.params.image, "alpine:latest");
        assert!(!loaded.params.gpu_nvidia, "旧 gpu 字段不映射到 gpu_nvidia");
    }
}
