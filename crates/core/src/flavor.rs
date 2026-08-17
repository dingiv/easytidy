//! 配置 flavor：可复用的容器配置模板。
//!
//! flavor 描述"如何构建一个预配置容器"：镜像 + 安装命令（setup）+ GUI 透传
//! + entry 应用 + 挂载/网络。
//!
//! `easytidy flavor apply <name>` 按模板创建容器、经 server 执行 setup、
//! 注册配置；之后即可用 `easytidy run` 启动容器内的 GUI 应用（entry 应用）。
//!
//! 清单存放：`$XDG_CONFIG_HOME/easytidy/flavors/<name>.toml`

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::models::{ContainerConfig, ContainerParams, MountConfig};

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
    /// 核心参数（与 ContainerConfig 共享基座；flatten 平铺，TOML 形状不变）
    #[serde(flatten)]
    pub params: ContainerParams,
    /// GUI 应用：展开时自动注入宿主显示环境（DISPLAY/WAYLAND_DISPLAY/
    /// XAUTHORITY）+ 挂载 /tmp 与 $XDG_RUNTIME_DIR（X11/Wayland socket
    /// 透传）+ 字体/图标只读透传；并强制 user_home
    #[serde(default)]
    pub gui: bool,
    /// 容器内按序执行的安装命令（经 server PTY 以 `bash -c` 执行）
    #[serde(default)]
    pub setup: Vec<String>,
}

impl Flavor {
    /// flavors 目录（$XDG_CONFIG_HOME/easytidy/flavors）。
    pub fn flavors_dir() -> Result<PathBuf> {
        let base = crate::configfile::ConfigFile::default_path()?;
        let dir = match base.parent() {
            Some(p) => p.join("flavors"),
            None => PathBuf::from("flavors"),
        };
        Ok(dir)
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

    /// 展开为容器配置（模板 → 实例快照）。
    ///
    /// 基座（`params`）整体继承（含 `entry_args`——曾在此处丢失）；GUI 透传
    /// （gui = true）追加推导产物，配方参考 docs/11-gui-container.md（宿主实测验证）：
    /// - env：DISPLAY / WAYLAND_DISPLAY / XAUTHORITY / XDG_RUNTIME_DIR（取宿主值）
    /// - 挂载：`/tmp/.X11-unix`（X11 socket）、`$XDG_RUNTIME_DIR`（Wayland/dbus/XAUTHORITY）
    /// - 字体/图标透传（只读）：`/usr/share/fonts`、`$HOME/.local/share/fonts`、
    ///   `/usr/share/icons`、`$HOME/.local/share/icons`（容器内 GUI 应用中文渲染
    ///   与图标主题需要宿主字体；distrobox 同类挂载）
    /// - 用户一致性映射（distrobox 式）：gui=true 强制 `user_home=true`，
    ///   由 create_with_config 映射 `$HOME` + 注入 EASYTIDY_USER_*
    /// - GPU 透传（--gpus=all + NVIDIA_* env）与 apparmor=unconfined 属 P1（需宿主
    ///   nvidia-container-toolkit），此处仅做纯显示透传，GUI 应用以软件渲染可用。
    ///
    /// 血缘：展开结果盖 `flavor = Some(self.name)`（模板同步/漂移检测依据）。
    pub fn build_config(&self, name: &str) -> Result<ContainerConfig> {
        let mut env = Vec::new();
        let mut params = self.params.clone();

        if self.gui {
            for key in ["DISPLAY", "WAYLAND_DISPLAY", "XAUTHORITY", "XDG_RUNTIME_DIR"] {
                if let Ok(v) = std::env::var(key) {
                    if !v.is_empty() {
                        env.push(format!("{key}={v}"));
                    }
                }
            }
            // X11 socket
            params.mounts.push(MountConfig {
                host_path: "/tmp/.X11-unix".to_string(),
                container_path: "/tmp/.X11-unix".to_string(),
                read_only: false,
            });
            // Wayland / dbus / XAUTHORITY（$XDG_RUNTIME_DIR）
            if let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR") {
                params.mounts.push(MountConfig {
                    host_path: runtime.clone(),
                    container_path: runtime,
                    read_only: false,
                });
            }
            // 字体/图标透传（只读）。⚠️ 不能直接覆盖容器自身 /usr/share/fonts 或
            // /usr/share/icons——图标/字体包的 dpkg postinst 会写入这两个目录
            // （update-icon-caches / fc-cache），只读挂载导致安装失败（实测）。
            // 因此挂到非冲突路径 /usr/share/easytidy-host/，由 fontconfig local.conf
            // （server 启动时写）+ XDG_DATA_DIRS 接入。
            // 用户级目录（~/.local/share/*）无此问题（postinst 不写），可原地挂。
            let mut env_extra = Vec::new();
            for (host, container) in [
                ("/usr/share/fonts", "/usr/share/easytidy-host/fonts"),
                ("/usr/share/icons", "/usr/share/easytidy-host/icons"),
            ] {
                if Path::new(host).exists() {
                    params.mounts.push(MountConfig {
                        host_path: host.to_string(),
                        container_path: container.to_string(),
                        read_only: true,
                    });
                }
            }
            // ⚠️ 必须**追加**系统默认目录，不能纯覆盖：gdk-pixbuf 2.42 经
            // $XDG_DATA_DIRS/gdk-pixbuf-2.0/2.10.0/loaders.cache 查找 loader
            // 注册表，覆盖后系统 cache 不可达 → 容器内 PNG 图标解码失败 →
            // GTK 文件选择器断言崩溃（2026-08-07 Chrome 保存图片实测）。
            env_extra.push(
                "XDG_DATA_DIRS=/usr/share/easytidy-host:/usr/local/share:/usr/share".to_string(),
            );
            if let Ok(home) = std::env::var("HOME") {
                for sub in [".local/share/fonts", ".local/share/icons"] {
                    let p = format!("{home}/{sub}");
                    if Path::new(&p).exists() {
                        params.mounts.push(MountConfig {
                            host_path: p.clone(),
                            container_path: format!("/usr/share/easytidy-host/{sub}"),
                            read_only: true,
                        });
                    }
                }
            }
            env.extend(env_extra);
            // gui=true 恒开用户一致性映射（GUI 应用需以宿主用户身份读写宿主挂载目录）
            params.user_home = true;
        }

        Ok(ContainerConfig {
            name: name.to_string(),
            params,
            env,
            silent_boot: false,
            persistent: true,
            // 血缘盖章：后续 ConfigManager「从模板同步」与漂移检测依据
            flavor: Some(self.name.clone()),
        })
    }
}

// ============================================================================
// 血缘：模板同步与漂移检测（config ← flavor 重展开）
// ============================================================================

/// 血缘状态（GUI 展示用）：来源模板是否存在 + 实例是否漂移。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LineageStatus {
    /// 来源模板名
    pub flavor: String,
    /// 模板文件是否存在（被删除 = 无法同步，仅展示血缘）
    pub exists: bool,
    /// 实例基座与模板当前声明不一致（GUI 依据此展示「从模板同步」）。
    /// 只比 `params`——env 是展开期宿主环境解析快照（DISPLAY 等），
    /// 天然随会话变化，不参与漂移判定
    pub drifted: bool,
}

/// 查询实例配置的血缘状态。
///
/// `None` = 无血缘（自由创建）。模板文件损坏/不可读按 `exists=false` 处理
/// （不吞掉血缘信息）。
pub fn lineage_status(config: &ContainerConfig) -> Option<LineageStatus> {
    let flavor_name = config.flavor.clone()?;
    let status = match Flavor::load(&flavor_name) {
        Ok(f) => LineageStatus {
            drifted: f
                .build_config(&config.name)
                .map(|expanded| expanded.params != config.params)
                .unwrap_or(true),
            flavor: flavor_name,
            exists: true,
        },
        Err(_) => LineageStatus {
            flavor: flavor_name,
            exists: false,
            drifted: false,
        },
    };
    Some(status)
}

/// 从来源模板重新同步容器配置（flavor = 实例配置批量管理的核心动作）：
/// 重新展开 → 保留实例侧字段 → 重建容器 → 更新注册。
///
/// - 基座（image/entry/entry_args/mounts/network/user_home）与 env 取模板
///   重新展开结果（env 重解析当前宿主显示环境）
/// - `silent_boot` / `persistent` 保留实例当前值（用户本地决策不随模板走）
///
/// 返回同步后的配置。
pub async fn sync_from_flavor(
    podman: &crate::podman::Podman,
    config_file: &crate::configfile::ConfigFile,
    name: &str,
) -> Result<ContainerConfig> {
    let current = config_file
        .get_container(name)?
        .ok_or_else(|| Error::Config(format!("容器配置不存在：{name}")))?;
    let flavor_name = current.flavor.clone().ok_or_else(|| {
        Error::Config(format!("容器 {name} 无血缘（非模板创建），不参与模板同步"))
    })?;
    let flavor = Flavor::load(&flavor_name)?;
    let mut next = flavor.build_config(name)?;
    next.silent_boot = current.silent_boot;
    next.persistent = current.persistent;
    podman.rebuild(name, &next).await?;
    config_file.register_container(next.clone())?;
    Ok(next)
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
