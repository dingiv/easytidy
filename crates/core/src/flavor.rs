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
use crate::models::{ContainerConfig, MountConfig, NetworkConfig, NetworkMode};

/// 配置 flavor（TOML 清单）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Flavor {
    /// flavor 名（与文件名一致）
    pub name: String,
    /// 基础镜像
    pub image: String,
    /// GUI 应用：自动注入宿主显示环境（DISPLAY/WAYLAND_DISPLAY/XAUTHORITY）+
    /// 挂载 /tmp 与 $XDG_RUNTIME_DIR（X11/Wayland socket 透传）
    #[serde(default)]
    pub gui: bool,
    /// 容器内按序执行的安装命令（经 server PTY 以 `bash -c` 执行）
    #[serde(default)]
    pub setup: Vec<String>,
    /// entry 应用（容器内可执行名）
    pub entry: Option<String>,
    /// entry 应用参数
    #[serde(default)]
    pub entry_args: Vec<String>,
    /// 额外路径映射
    #[serde(default)]
    pub mounts: Vec<MountConfig>,
    /// 用户一致性映射（distrobox 式：映射宿主用户目录 + 容器用户与宿主一致）。
    /// 默认开启；`gui = true` 时强制开启。显式 `false` 且非 GUI 时关闭
    /// （容器内以 root 运行，行为与旧版一致）。
    #[serde(default)]
    pub user_home: Option<bool>,
    /// 网络配置（默认 host 模式）
    #[serde(default = "default_network")]
    pub network: NetworkConfig,
}

fn default_network() -> NetworkConfig {
    NetworkConfig {
        mode: NetworkMode::Host,
        ports: Vec::new(),
    }
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

    /// 展开为容器配置。
    ///
    /// GUI 透传（gui = true），配方参考 docs/11-gui-container.md（宿主实测验证）：
    /// - env：DISPLAY / WAYLAND_DISPLAY / XAUTHORITY / XDG_RUNTIME_DIR（取宿主值）
    /// - 挂载：`/tmp/.X11-unix`（X11 socket）、`$XDG_RUNTIME_DIR`（Wayland/dbus/XAUTHORITY）
    /// - 字体/图标透传（只读）：`/usr/share/fonts`、`$HOME/.local/share/fonts`、
    ///   `/usr/share/icons`、`$HOME/.local/share/icons`（容器内 GUI 应用中文渲染
    ///   与图标主题需要宿主字体；distrobox 同类挂载）
    /// - 用户一致性映射（distrobox 式）：gui=true 强制 `user_home=true`，
    ///   由 create_with_config 映射 `$HOME` + 注入 EASYTIDY_USER_*（见 models::ContainerConfig）
    /// - GPU 透传（--gpus=all + NVIDIA_* env）与 apparmor=unconfined 属 P1（需宿主
    ///   nvidia-container-toolkit），此处仅做纯显示透传，GUI 应用以软件渲染可用。
    pub fn build_config(&self, name: &str) -> Result<ContainerConfig> {
        let mut env = Vec::new();
        let mut mounts = self.mounts.clone();

        if self.gui {
            for key in ["DISPLAY", "WAYLAND_DISPLAY", "XAUTHORITY", "XDG_RUNTIME_DIR"] {
                if let Ok(v) = std::env::var(key) {
                    if !v.is_empty() {
                        env.push(format!("{key}={v}"));
                    }
                }
            }
            // X11 socket
            mounts.push(MountConfig {
                host_path: "/tmp/.X11-unix".to_string(),
                container_path: "/tmp/.X11-unix".to_string(),
                read_only: false,
            });
            // Wayland / dbus / XAUTHORITY（$XDG_RUNTIME_DIR）
            if let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR") {
                mounts.push(MountConfig {
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
                    mounts.push(MountConfig {
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
                        mounts.push(MountConfig {
                            host_path: p.clone(),
                            container_path: format!("/usr/share/easytidy-host/{sub}"),
                            read_only: true,
                        });
                    }
                }
            }
            env.extend(env_extra);
        }

        Ok(ContainerConfig {
            name: name.to_string(),
            image: self.image.clone(),
            entry: self.entry.clone(),
            silent_boot: false,
            persistent: true,
            mounts,
            network: self.network.clone(),
            env,
            // gui=true 恒开用户一致性映射（GUI 应用需以宿主用户身份读写宿主挂载目录）；
            // 非 GUI flavor 默认开启、可显式 user_home=false 关闭
            user_home: self.gui || self.user_home.unwrap_or(true),
        })
    }
}
