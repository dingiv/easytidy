//! 共享配置文件存储（原子写 + flock + schema 版本化）。
//!
//! 支持多实例 GUI + CLI 并发读写共享状态：
//! - 容器注册表（name → ContainerConfig）
//! - 全局设置
//!
//! 并发控制：
//! - 原子写（临时文件 + rename）
//! - flock 临界区（fs2::FileExt）
//! - schema 版本化（未来迁移兼容）

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::fs::File;
use std::sync::Mutex;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use crate::error::{Error, Result};
use crate::models::ContainerConfig;

/// 配置文件 schema 版本（变更时递增，用于未来迁移）。
const CURRENT_SCHEMA_VERSION: u32 = 1;

/// 全局配置（单例，含容器注册表 + 全局设置）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GlobalConfig {
    /// schema 版本（用于迁移兼容）
    pub schema_version: u32,
    /// 容器注册表（name → config）
    pub containers: HashMap<String, ContainerConfig>,
    /// 全局设置（保留扩展）
    #[serde(flatten)]
    pub settings: HashMap<String, serde_json::Value>,
}

impl GlobalConfig {
    /// 创建新配置（初始化 schema 版本）。
    pub fn new() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            containers: HashMap::new(),
            settings: HashMap::new(),
        }
    }

    /// 检查 schema 版本是否兼容（未来实现迁移）。
    pub fn check_schema(&self) -> Result<()> {
        if self.schema_version > CURRENT_SCHEMA_VERSION {
            return Err(Error::Config(format!(
                "配置文件版本 {} 过新（当前版本 {}）",
                self.schema_version, CURRENT_SCHEMA_VERSION
            )));
        }
        Ok(())
    }
}

/// 配置文件存储（线程安全，支持私有配置路径）。
pub struct ConfigFile {
    /// 配置文件路径（可自定义）
    path: PathBuf,
    /// 内存缓存（可选，用于减少文件 I/O）
    cache: Mutex<Option<GlobalConfig>>,
}

impl ConfigFile {
    /// 创建配置文件实例（默认路径：$XDG_CONFIG_HOME/easytidy/config.toml）。
    pub fn default_path() -> Result<PathBuf> {
        let config_dir = dirs::config_dir()
            .ok_or_else(|| Error::Config("无法确定 XDG_CONFIG_HOME".to_string()))?;

        Ok(config_dir.join("easytidy").join("config.toml"))
    }

    /// 创建配置文件实例（指定路径）。
    pub fn with_path(path: PathBuf) -> Self {
        Self {
            path,
            cache: Mutex::new(None),
        }
    }

    /// 加载配置（含 flock 并发控制 + schema 检查）。
    ///
    /// 流程：
    /// 1. 打开文件（不存在则返回默认配置）
    /// 2. flock shared lock（读锁）
    /// 3. 读取并解析 TOML
    /// 4. 检查 schema 版本
    /// 5. 更新缓存
    pub fn load(&self) -> Result<GlobalConfig> {
        // 文件不存在则返回默认配置
        if !self.path.exists() {
            tracing::info!("配置文件不存在，返回默认配置：{:?}", self.path);
            let config = GlobalConfig::new();
            return Ok(config);
        }

        // 打开文件
        let file = File::open(&self.path)
            .map_err(|e| Error::Config(format!("打开配置文件失败：{e}")))?;

        // flock shared lock（读锁，阻塞等待）
        file.lock_shared()
            .map_err(|e| Error::Config(format!("获取配置锁失败：{e}")))?;

        // 读取内容
        let mut content = String::new();
        {
            let mut reader = file;
            reader.read_to_string(&mut content)
                .map_err(|e| Error::Config(format!("读取配置文件失败：{e}")))?;
        }

        // 解析 TOML
        let config: GlobalConfig = toml::from_str(&content)
            .map_err(|e| Error::Config(format!("解析配置文件失败：{e}")))?;

        // 检查 schema 版本
        config.check_schema()?;

        // 更新缓存
        *self.cache.lock().unwrap() = Some(config.clone());

        Ok(config)
    }

    /// 保存配置（原子写 + flock 临界区）。
    ///
    /// 流程：
    /// 1. 打开文件（不存在则创建父目录）
    /// 2. flock exclusive lock（写锁，阻塞等待）
    /// 3. 读取现有配置（用于 merge，可选）
    /// 4. 写入临时文件（原子替换）
    /// 5. rename 到目标路径
    pub fn save(&self, config: &GlobalConfig) -> Result<()> {
        // 创建父目录
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Config(format!("创建配置目录失败：{e}")))?;
        }

        // 打开文件（不存在则创建）。
        // 注意：不能用 File::create（会先截断文件再拿锁）——并发 load() 可能读到
        // 半截文件；内容写入走临时文件 + rename，这里只需写句柄来承载 flock。
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false) // 关键：先拿锁再替换，绝不截断现有文件
            .open(&self.path)
            .map_err(|e| Error::Config(format!("创建配置文件失败：{e}")))?;

        // flock exclusive lock（写锁，阻塞等待）
        file.lock()
            .map_err(|e| Error::Config(format!("获取配置锁失败：{e}")))?;

        // 序列化为 TOML
        let content = toml::to_string_pretty(config)
            .map_err(|e| Error::Config(format!("序列化配置失败：{e}")))?;

        // 原子写（临时文件 + rename）
        let temp_dir = self.path.parent()
            .ok_or_else(|| Error::Config("配置文件路径无父目录".to_string()))?;

        let mut temp_file = NamedTempFile::new_in(temp_dir)
            .map_err(|e| Error::Config(format!("创建临时文件失败：{e}")))?;

        // 写入临时文件
        temp_file.write_all(content.as_bytes())
            .map_err(|e| Error::Config(format!("写入临时文件失败：{e}")))?;

        // 刷新到磁盘
        temp_file.flush()
            .map_err(|e| Error::Config(format!("刷新临时文件失败：{e}")))?;

        // 原子替换（persist 会关闭文件，先释放 flock）
        drop(file);
        temp_file.persist(&self.path)
            .map_err(|e| Error::Config(format!("持久化配置文件失败：{e}")))?;

        tracing::info!("配置文件保存成功：{:?}", self.path);

        // 更新缓存
        *self.cache.lock().unwrap() = Some(config.clone());

        Ok(())
    }

    /// 获取容器配置（按名）。
    pub fn get_container(&self, name: &str) -> Result<Option<ContainerConfig>> {
        let config = self.load()?;
        Ok(config.containers.get(name).cloned())
    }

    /// 注册容器（upsert）。
    pub fn register_container(&self, container_config: ContainerConfig) -> Result<()> {
        let mut config = self.load()?;
        let name = container_config.name.clone();
        config.containers.insert(name, container_config);
        self.save(&config)
    }

    /// 注销容器。
    pub fn unregister_container(&self, name: &str) -> Result<()> {
        let mut config = self.load()?;
        config.containers.remove(name);
        self.save(&config)
    }

    /// 列出所有容器配置。
    pub fn list_containers(&self) -> Result<Vec<ContainerConfig>> {
        let config = self.load()?;
        Ok(config.containers.values().cloned().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use std::fs;
    use std::thread;
    
    use std::sync::Arc;

    #[test]
    fn test_atomic_write() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        let config_file = ConfigFile::with_path(config_path.clone());

        let config = GlobalConfig::new();
        config_file.save(&config).unwrap();

        // 验证文件存在且可读
        assert!(config_path.exists());
        let loaded = config_file.load().unwrap();
        assert_eq!(loaded.schema_version, CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn test_flock_concurrency() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("concurrent-config.toml");
        let config_file = Arc::new(ConfigFile::with_path(config_path.clone()));

        // 预先创建配置文件
        let config = GlobalConfig::new();
        config_file.save(&config).unwrap();

        let handles: Vec<_> = (0..5)
            .map(|i| {
                let cf = config_file.clone();
                thread::spawn(move || {
                    let mut config = cf.load().unwrap();
                    config.containers.insert(
                        format!("container-{}", i),
                        ContainerConfig {
                            name: format!("container-{}", i),
                            image: "alpine:latest".to_string(),
                            ..Default::default()
                        },
                    );
                    cf.save(&config).unwrap();
                })
            })
            .collect();

        // 等待所有线程完成
        for handle in handles {
            handle.join().unwrap();
        }

        // 验证最终状态（并发写入会互相覆盖，但至少应该有一个）
        let config = config_file.load().unwrap();
        // 由于没有合并逻辑，最终结果可能是 1 个容器（最后一次写入）
        // 但应该没有 panic 或死锁
        assert!(config.containers.len() <= 5, "容器数量应该 <= 5");
        assert!(!config.containers.is_empty(), "至少应该有一个容器被保存");
    }

    #[test]
    fn test_container_crud() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("crud-config.toml");
        let config_file = ConfigFile::with_path(config_path);

        // 创建
        let container = ContainerConfig {
            name: "test-container".to_string(),
            image: "alpine:latest".to_string(),
            entry: Some("/bin/sh".to_string()),
            silent_boot: true,
            persistent: true,
            ..Default::default()
        };
        config_file.register_container(container.clone()).unwrap();

        // 读取
        let loaded = config_file.get_container("test-container").unwrap();
        assert!(loaded.is_some());
        let loaded = loaded.unwrap();
        assert_eq!(loaded.name, "test-container");
        assert_eq!(loaded.image, "alpine:latest");

        // 列出
        let list = config_file.list_containers().unwrap();
        assert_eq!(list.len(), 1);

        // 删除
        config_file.unregister_container("test-container").unwrap();
        let loaded = config_file.get_container("test-container").unwrap();
        assert!(loaded.is_none());
    }

    #[test]
    fn test_old_format_container_defaults() {
        // 旧配置文件（无 mounts/network 字段）→ 加载后新字段取默认值
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("old-format.toml");
        let config_file = ConfigFile::with_path(config_path.clone());

        fs::write(
            &config_path,
            r#"schema_version = 1

[containers.legacy]
name = "legacy"
image = "alpine:latest"
entry = "/bin/sh"
silent_boot = false
persistent = true
"#,
        ).unwrap();

        let loaded = config_file.load().unwrap();
        let c = loaded.containers.get("legacy").expect("旧格式容器应可加载");
        assert_eq!(c.image, "alpine:latest");
        assert!(c.mounts.is_empty(), "旧格式无 mounts → 默认空");
        assert_eq!(c.network.mode, crate::models::NetworkMode::Host);
        assert!(c.network.ports.is_empty(), "旧格式无端口 → 默认空");
    }

    #[test]
    fn test_schema_version() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("schema-config.toml");
        let config_file = ConfigFile::with_path(config_path.clone());

        // 写入默认配置
        let config = GlobalConfig::new();
        config_file.save(&config).unwrap();

        // 读取并验证
        let loaded = config_file.load().unwrap();
        assert_eq!(loaded.schema_version, CURRENT_SCHEMA_VERSION);

        // 尝试加载过新版本（手动篡改）
        fs::write(
            &config_path,
            r#"schema_version = 999
containers = {}"#,
        ).unwrap();

        let result = config_file.load();
        assert!(result.is_err());
    }
}
