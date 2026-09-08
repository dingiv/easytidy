//! 配置 flavor：可复用的容器配置模板。
//!
//! flavor 描述"如何构建一个预配置容器"：镜像 + 安装命令（setup）+ GUI 透传
//! + entry 应用 + 挂载/网络。
//!
//! `easytidy flavor apply <name>` 按模板创建容器、经 server 执行 setup、
//! 注册配置；之后即可用 `easytidy run` 启动容器内的 GUI 应用（entry 应用）。
//!
//! 清单存放：`$XDG_CONFIG_HOME/easytidy/flavors/<name>.toml`

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::models::{ContainerConfig, ContainerParams};

/// 配置 flavor（TOML 清单）。
///
/// 模板 = 共享基座 [`ContainerParams`]（镜像/entry/挂载/网络/用户映射）+
/// 模板专属的**意图**字段：`gui`（展开期推导指令——宿主侧探测 DISPLAY/
/// 字体目录后生成 env/mounts，非容器参数）与 `setup`（创建后经 server PTY
/// 执行的安装命令，生命周期动作）。存意图，不存解析快照——展开
/// （[`Flavor::build_config`]）才产出实例快照 [`ContainerConfig`]。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Flavor {
    /// flavor 名（与文件名一致）
    pub name: String,
    /// 核心参数（与 ContainerConfig 共享基座；flatten 平铺，TOML 形状不变）。
    /// `gui` / `gpu` 透传意图即在本基座内（与实例共享），展开时据其注入。
    #[serde(flatten)]
    pub params: ContainerParams,
    /// 容器内按序执行的安装命令（经 server PTY 以 `bash -c` 执行）
    #[serde(default)]
    pub setup: Vec<String>,
}

impl Flavor {
    /// flavors 目录（应用数据根下的 `flavors` 子目录，dev/prod 自动切）。
    pub fn flavors_dir() -> Result<PathBuf> {
        Ok(crate::appdata::app_data_dir()?.join("flavors"))
    }

    /// 列出可用 flavor（*.toml 文件名）。
    pub fn list() -> Result<Vec<String>> {
        let dir = Self::flavors_dir()?;
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.extension().map(|e| e == "toml").unwrap_or(false) {
                    if let Some(stem) = p.file_stem() {
                        out.push(stem.to_string_lossy().to_string());
                    }
                }
            }
        }
        out.sort();
        Ok(out)
    }

    /// 加载指定 flavor。
    pub fn load(name: &str) -> Result<Flavor> {
        let dir = Self::flavors_dir()?;
        let path = dir.join(format!("{name}.toml"));
        let text = std::fs::read_to_string(&path)
            .map_err(|_| Error::Config(format!("flavor 不存在：{name}（{}）", path.display())))?;
        let flavor: Flavor = toml::from_str(&text)
            .map_err(|e| Error::Config(format!("解析 flavor {name} 失败：{e}")))?;
        Ok(flavor)
    }

    /// 列出全部 flavor（含完整配置；GUI flavor 管理用）。
    pub fn list_detailed() -> Result<Vec<Flavor>> {
        Self::list()?
            .into_iter()
            .map(|name| Self::load(&name))
            .collect()
    }

    /// 保存 flavor（新建或覆盖；临时文件 + rename 原子写，与 configfile 同款）。
    pub fn save(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(Error::Config("flavor 名不能为空".to_string()));
        }
        let dir = Self::flavors_dir()?;
        std::fs::create_dir_all(&dir)
            .map_err(|e| Error::Config(format!("创建 flavors 目录失败：{e}")))?;
        let path = dir.join(format!("{}.toml", self.name));
        let content = toml::to_string_pretty(self)
            .map_err(|e| Error::Config(format!("序列化 flavor 失败：{e}")))?;
        let tmp = dir.join(format!("{}.toml.tmp", self.name));
        std::fs::write(&tmp, &content)
            .map_err(|e| Error::Config(format!("写入 flavor 失败：{e}")))?;
        std::fs::rename(&tmp, &path)
            .map_err(|e| Error::Config(format!("替换 flavor 文件失败：{e}")))?;
        tracing::info!("flavor 已保存：{path:?}");
        Ok(())
    }

    /// 删除 flavor（不存在时静默）。
    pub fn delete(name: &str) -> Result<()> {
        let path = Self::flavors_dir()?.join(format!("{name}.toml"));
        if path.exists() {
            std::fs::remove_file(&path)
                .map_err(|e| Error::Config(format!("删除 flavor 失败：{e}")))?;
            tracing::info!("flavor 已删除：{path:?}");
        }
        Ok(())
    }

    /// 复制模板为一键快捷动作：加载 → 改名 → 保存为新文件（原子写）。
    ///
    /// `to` 已存在时报错防覆盖；调用方（GUI 复制按钮）负责生成不冲突的
    /// 目标名（如 `name-copy`、冲突则 `name-copy2/3…`）。
    pub fn duplicate(from: &str, to: &str) -> Result<()> {
        if to.trim().is_empty() {
            return Err(Error::Config("目标模板名不能为空".to_string()));
        }
        if to == from {
            return Err(Error::Config("目标名与源模板相同，无法复制".to_string()));
        }
        let mut copy = Self::load(from)?;
        let dir = Self::flavors_dir()?;
        let path = dir.join(format!("{to}.toml"));
        if path.exists() {
            return Err(Error::Config(format!("模板 {to} 已存在，不能覆盖")));
        }
        copy.name = to.to_string();
        copy.save()
    }

    /// 展开为容器配置（模板 → 实例快照）。
    ///
    /// 基座（`params`）整体继承（含 `entry_args`——曾在此处丢失）；GUI 透传
    /// （gui = true）按 [`gui_passthrough`](crate::gui_passthrough) 规则追加推导
    /// 产物（显示 env + X11/XDG_RUNTIME_DIR/字体图标挂载 + 恒开 keep-id）。具体
    /// 映射表外置到资源文件 `ASSETS_DIR::gui-passthrough.yaml`（dev 源码树 /
    /// prod 数据目录），见 [`crate::gui_passthrough`] 模块文档与 docs/11。
    ///
    /// 模板仅作**创建期预填**：展开为实例快照，容器创建后与模板彻底解耦
    /// （无血缘字段、无同步、无漂移检测）。
    pub fn build_config(&self, name: &str) -> Result<ContainerConfig> {
        let mut config = ContainerConfig {
            name: name.to_string(),
            params: self.params.clone(),
            env: Vec::new(),
            silent_boot: false,
            persistent: true,
            icon: None,
        };
        crate::env::host::inject_passthrough(&mut config);
        Ok(config)
    }
}

// ============================================================================
// 内置预设 flavor（快速拉起 GUI 容器；缺失时补齐，不覆盖用户修改）
// ============================================================================

/// 内置预设模板（name → TOML 内容）。
const PRESET_FLAVORS: &[(&str, &str)] = &[
    (
        "chrome",
        r#"# Chrome 快速拉起：GUI 底座 + 官方源安装 + entry
# 使用：easytidy flavor apply chrome（或主 GUI Flavor 面板「创建容器」）
name = "chrome"
image = "docker.io/library/ubuntu:24.04"
gui = true
setup = [
    "apt-get update -qq && apt-get install -y -qq curl gpg",
    "curl -fsSL https://dl.google.com/linux/linux_signing_key.pub | gpg --dearmor -o /usr/share/keyrings/google-chrome.gpg",
    "echo 'deb [arch=amd64 signed-by=/usr/share/keyrings/google-chrome.gpg] https://dl.google.com/linux/chrome/deb/ stable main' > /etc/apt/sources.list.d/google-chrome.list",
    "apt-get update -qq && apt-get install -y -qq google-chrome-stable",
]
entry = "google-chrome-stable"
"#,
    ),
    (
        "firefox",
        r#"# Firefox 快速拉起：debian 底座（firefox-esr 为 deb 原生包，无 snap 问题）
name = "firefox"
image = "docker.io/library/debian:bookworm"
gui = true
setup = ["apt-get update -qq && apt-get install -y -qq firefox-esr"]
entry = "firefox-esr"
"#,
    ),
    (
        "code",
        r#"# VS Code 快速拉起：GUI 底座 + 微软官方源安装
name = "code"
image = "docker.io/library/ubuntu:24.04"
gui = true
setup = [
    "apt-get update -qq && apt-get install -y -qq curl gpg",
    "curl -fsSL https://packages.microsoft.com/keys/microsoft.asc | gpg --dearmor -o /usr/share/keyrings/microsoft.gpg",
    "echo 'deb [arch=amd64 signed-by=/usr/share/keyrings/microsoft.gpg] https://packages.microsoft.com/repos/code stable main' > /etc/apt/sources.list.d/vscode.list",
    "apt-get update -qq && apt-get install -y -qq code",
]
entry = "code"
"#,
    ),
];

/// 补齐内置预设 flavor（文件不存在时写入；已存在 = 用户修改过，不覆盖）。
pub fn ensure_presets() {
    let Ok(dir) = Flavor::flavors_dir() else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    for (name, content) in PRESET_FLAVORS {
        let path = dir.join(format!("{name}.toml"));
        if !path.exists()
            && std::fs::write(&path, content).is_ok() {
                tracing::info!("预设 flavor 已写入：{path:?}");
            }
    }
}
