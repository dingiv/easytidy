//! podman socket API 客户端（bollard 封装）。
//!
//! 连接 rootless podman user socket（$XDG_RUNTIME_DIR/podman/podman.sock），
//! 通过 bollard Docker compat API + libpod 扩展端点管理容器生命周期。
//!
//! ## 标签方案
//!
//! - `manager=easytidy`: 标识由 easytidy 管理的容器
//! - `easytidy.name=<name>`: 容器名（用于快速过滤）
//!
//! 铁律：零 podman CLI 调用（见 docs/08-requirements.md L2）。

use std::path::{Path, PathBuf};
use bollard::Docker;
use crate::error::{Error, Result};
use crate::models::{
    ContainerConfig, ContainerConfigView, ContainerSummary, MountConfig, NetworkConfig,
    NetworkMode, PortMapping,
};

/// 宿主侧 exec PTY（root 终端通道；见 exec.rs）
pub mod exec;
pub use exec::ExecPty;

/// Podman 客户端封装。
pub struct Podman {
    /// bollard Docker 实例（Docker compat API）
    pub(super) docker: Docker,
}

impl Podman {
    /// 连接到 rootless podman socket 并协商 API 版本。
    ///
    /// 路径规则：$XDG_RUNTIME_DIR/podman/podman.sock（缺失则 Error::NoXdgRuntime）。
    /// 连接后调用 `ping()` 验证并协商版本（Docker-v29 教训：永不硬编码 API 版本）。
    pub async fn connect() -> Result<Self> {
        let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
            .map_err(|_| Error::NoXdgRuntime)?;

        let socket_path = PathBuf::from(runtime_dir)
            .join("podman/podman.sock");

        if !socket_path.exists() {
            return Err(Error::Connect(format!(
                "podman socket 不存在：{}（请启用 podman.socket user unit）",
                socket_path.display()
            )));
        }

        // bollard 的 UnixStream 需要明确路径字符串
        let socket_str = socket_path.to_str()
            .ok_or_else(|| Error::Connect("socket 路径非法 UTF-8".to_string()))?;

        // 使用 bollard 的 connect_with_unix 方法
        let docker = Docker::connect_with_unix(socket_str, 120, bollard::API_DEFAULT_VERSION)
            .map_err(|e| Error::Connect(format!("连接 podman 失败：{e}")))?;

        let mut client = Self { docker };

        // 协商版本（验证连接并获取 podman 支持的最高 API 版本）
        client.negotiate_version().await?;

        Ok(client)
    }

    /// 协商 API 版本（ping podman 并获取服务器版本）。
    ///
    /// bollard 默认使用最新 API 版本；某些 podman 版本可能不支持。
    /// 此方法验证连接并可选地降级到特定版本（失败时 pin ClientVersion）。
    async fn negotiate_version(&mut self) -> Result<()> {
        let version = self.docker.version().await
            .map_err(|e| Error::Connect(format!("版本协商失败：{e}")))?;

        tracing::debug!("podman 版本：API={}, OS={}",
            version.api_version.unwrap_or_default(),
            version.os.unwrap_or_default());

        // TODO: 如果 API 版本过老，可以在这里降级
        // 目前 bollard 0.18 支持 Docker API 1.43+，对应 podman 5.2+

        Ok(())
    }

    /// 列出所有容器（包括停止的）。
    ///
    /// 从 podman 投影为 `ContainerSummary`，按 `manager=easytidy` 标签判定 `managed`。
    pub async fn list_containers(&self) -> Result<Vec<ContainerSummary>> {
        use bollard::container::ListContainersOptions;

        let opts = ListContainersOptions::<String> {
            all: true,
            ..Default::default()
        };

        let containers = self.docker.list_containers(Some(opts)).await?;

        let result: Vec<ContainerSummary> = containers
            .into_iter()
            .filter_map(|c| self.map_container_summary(c))
            .collect();

        Ok(result)
    }

    /// 从 bollard ContainerSummary 映射为我们的模型。
    fn map_container_summary(&self, c: bollard::models::ContainerSummary) -> Option<ContainerSummary> {
        let name = c.names?.first()?.trim_start_matches('/').to_string();
        let id = c.id?;
        let image = c.image.unwrap_or_default();
        let status = c.state.unwrap_or_else(|| "unknown".to_string());

        // 按标签判定是否为 easytidy 管理
        let managed = c.labels
            .as_ref()
            .and_then(|labels| labels.get("manager"))
            .map(|v| v == "easytidy")
            .unwrap_or(false);

        Some(ContainerSummary {
            name,
            id: id.chars().take(12).collect(), // 短 ID
            image,
            status,
            managed,
        })
    }

    /// 创建容器（默认配置，保持既有语义）。
    ///
    /// 既有语义：podman 默认 bridge 网络、无端口映射（即 `NetworkMode::Mapped`
    /// 且无端口 → 不设 `network_mode`、无 ExposedPorts/PortBindings，与旧行为逐字节一致）。
    ///
    /// 新代码请使用 [`Podman::create_with_config`]（cli/GUI 均走新入口）。
    pub async fn create(
        &self,
        name: &str,
        image: &str,
        server_bin_path: &Path,
    ) -> Result<String> {
        let config = ContainerConfig {
            name: name.to_string(),
            image: image.to_string(),
            // 保持既有语义：默认 bridge 网络 + 无端口映射
            network: NetworkConfig {
                mode: NetworkMode::Mapped,
                ports: Vec::new(),
            },
            ..Default::default()
        };
        self.create_with_config(name, image, server_bin_path, &config)
            .await
    }

    /// 创建容器并应用完整配置（mounts / 网络映射）。
    ///
    /// 参数：
    /// - name: 容器名
    /// - image: 镜像（如 "docker.io/library/alpine:latest"）
    /// - server_bin_path: server 二进制路径（宿主绝对路径，将 ro bind-mount 进容器）
    /// - config: 容器配置（mounts + 网络模式/端口映射）
    ///
    /// 在 `create()` 的既有基础之上追加（rebuild 保留同一套基础）：
    /// - HostConfig.init = true（catatonit = PID 1）
    /// - Cmd = [<server-bin>, "--socket", "/run/easytidy/server.sock"]
    /// - Bind mounts: server 二进制（ro）+ socket 目录（rw）+ config.mounts（宿主路径须已存在）
    /// - 标签: manager=easytidy + easytidy.name=<name>
    /// - 网络: `Host` → `network_mode = "host"`（端口映射无意义，忽略并告警）；
    ///   `Mapped` → 不设 network_mode（podman 默认 bridge）+ ExposedPorts + PortBindings
    /// - 用户一致性映射（`config.user_home`，distrobox 式）：`$HOME` → `$HOME`（rw）
    ///   与注入 `EASYTIDY_USER_NAME/UID/GID/HOME`，容器内 server 据此创建同名用户并
    ///   经 su 拉起应用（见 crates/server）；`host_user()` 失败时跳过并告警
    ///
    /// 镜像不存在则先拉取。
    pub async fn create_with_config(
        &self,
        name: &str,
        image: &str,
        server_bin_path: &Path,
        config: &ContainerConfig,
    ) -> Result<String> {
        use bollard::models::{HostConfig, Mount, MountTypeEnum, PortBinding};
        use std::collections::HashMap;

        // 检查镜像是否存在，不存在则直接报错（不自动拉取——拉取是显式用户动作）
        if !self.image_exists(image).await? {
            return Err(Error::Config(format!(
                "镜像不存在：{image}\n请先拉取镜像（如：podman pull {image}）"
            )));
        }

        // 创建宿主 socket 目录（$XDG_RUNTIME_DIR/easytidy/<name>）
        let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
            .map_err(|_| Error::NoXdgRuntime)?;
        let socket_host_dir = PathBuf::from(runtime_dir)
            .join("easytidy")
            .join(name);

        tokio::fs::create_dir_all(&socket_host_dir).await
            .map_err(|e| Error::Connect(format!("创建 socket 目录失败：{e}")))?;

        // 构建标签
        let mut labels = HashMap::new();
        labels.insert("manager".to_string(), "easytidy".to_string());
        labels.insert("easytidy.name".to_string(), name.to_string());

        // 构建挂载：server 二进制 + socket 目录 + 用户配置的 bind mounts
        let mut mounts = vec![
            // Server 二进制（只读）
            Mount {
                typ: Some(MountTypeEnum::BIND),
                source: Some(server_bin_path.to_string_lossy().to_string()),
                target: Some("/usr/bin/easytidy-server".to_string()),
                read_only: Some(true),
                ..Default::default()
            },
            // Socket 目录（可写）
            Mount {
                typ: Some(MountTypeEnum::BIND),
                source: Some(socket_host_dir.to_string_lossy().to_string()),
                target: Some("/run/easytidy".to_string()),
                read_only: Some(false),
                ..Default::default()
            },
        ];
        for m in &config.mounts {
            validate_mount(m)?;
            mounts.push(Mount {
                typ: Some(MountTypeEnum::BIND),
                source: Some(m.host_path.clone()),
                target: Some(m.container_path.clone()),
                read_only: Some(m.read_only),
                ..Default::default()
            });
        }

        // 用户一致性映射（distrobox 式，config.user_home）：映射宿主用户目录 +
        // 注入 EASYTIDY_USER_*（容器内 server 据此创建同名/同 uid/gid 用户，
        // 应用经 su 以该用户运行而非 root）。
        //
        // host_user() 失败（无法解析用户名/home）时仅告警并跳过——容器仍以 root
        // 运行，行为与旧版一致。
        let mut env = config.env.clone();
        if config.user_home {
            if let Some(user) = crate::userenv::host_user() {
                if !mount_has_target(&mounts, &user.home) && Path::new(&user.home).exists() {
                    mounts.push(Mount {
                        typ: Some(MountTypeEnum::BIND),
                        source: Some(user.home.clone()),
                        target: Some(user.home.clone()),
                        read_only: Some(false),
                        ..Default::default()
                    });
                }
                // 容器内用户固定名 node（server 侧），仅需 uid/gid 对齐宿主
                env.push(format!("EASYTIDY_USER_UID={}", user.uid));
                env.push(format!("EASYTIDY_USER_GID={}", user.gid));
            } else {
                tracing::warn!(
                    "容器 {}：user_home=true 但无法探测宿主用户（host_user() 失败），\
                     跳过用户映射，容器内以 root 运行",
                    name
                );
            }
        }

        // 构建 HostConfig
        let mut host_config = HostConfig {
            init: Some(true), // catatonit = PID 1
            mounts: Some(mounts),
            ..Default::default()
        };

        // 网络配置（host ⇄ bridge+端口映射 切换）
        let mut exposed_ports: Option<HashMap<String, HashMap<(), ()>>> = None;
        let mut port_bindings: Option<HashMap<String, Option<Vec<PortBinding>>>> = None;
        match config.network.mode {
            NetworkMode::Host => {
                host_config.network_mode = Some("host".to_string());
                if !config.network.ports.is_empty() {
                    tracing::warn!(
                        "容器 {} 网络模式为 host，端口映射不生效（已忽略）：{:?}",
                        name,
                        config.network.ports
                    );
                }
            }
            NetworkMode::Mapped => {
                // 不设 network_mode → podman 默认 bridge
                if !config.network.ports.is_empty() {
                    let mut exposed = HashMap::new();
                    let mut bindings: HashMap<String, Option<Vec<PortBinding>>> = HashMap::new();
                    for p in &config.network.ports {
                        let protocol = if p.protocol.is_empty() { "tcp" } else { p.protocol.as_str() };
                        if p.container_port == 0 || p.host_port == 0 {
                            return Err(Error::Config(format!(
                                "端口映射无效（{}:{} -> {}:{}）：端口不能为 0",
                                p.host_port, p.container_port, p.container_port, protocol
                            )));
                        }
                        let key = format!("{}/{}", p.container_port, protocol);
                        exposed.insert(key.clone(), HashMap::new());
                        bindings.insert(
                            key,
                            Some(vec![PortBinding {
                                host_ip: None,
                                host_port: Some(p.host_port.to_string()),
                            }]),
                        );
                    }
                    exposed_ports = Some(exposed);
                    port_bindings = Some(bindings);
                }
            }
        }
        host_config.port_bindings = port_bindings;

        // 用户一致性映射（user_home=true）→ keep-id 必须走 libpod 端点
        // （Docker compat 端点不支持 userns.keep-id，实测；见 libpod.rs）。
        // keep-id 使容器内 uid 1000（node 用户）= 宿主当前登录用户：
        // 宿主 home 读写 / /run/user/1000（显示 socket）自然可达（GUI 窗口可用）。
        // ⚠️ 最终模型（2026-08-17 定案）：
        // - 容器 User = root（PID 1 init 与 server 均 root，OCI 单 User 字段；
        //   podman exec 默认 root 是容器程序限制，无法单独设默认用户）
        // - keep-id：宿主 1000 ↔ 容器 1000（easytidy 用户，server 启动时创建）
        // - server(root) 拉起子进程（bash 等）经 fork+exec+setuid+setgid
        //   降权到 easytidy（见 server services/pty.rs，不再经 su）
        if config.user_home {
            let libpod = crate::libpod::Libpod::new().await?;
            // mounts / port_bindings 已在 host_config 中（早于本分支 move），从 host_config 取
            let mounts_json = serde_json::to_value(host_config.mounts.clone().unwrap_or_default())
                .map_err(|e| Error::Config(format!("序列化 mounts 失败：{e}")))?;
            let port_bindings_json = match &host_config.port_bindings {
                Some(pb) => Some(serde_json::to_value(pb)
                    .map_err(|e| Error::Config(format!("序列化 port_bindings 失败：{e}")))?),
                None => None,
            };
            let body = crate::libpod::keep_id_create_body(
                name,
                image,
                vec![
                    "/usr/bin/easytidy-server".to_string(),
                    "--socket".to_string(),
                    "/run/easytidy/server.sock".to_string(),
                ],
                env.clone(),
                labels.clone(),
                mounts_json.as_array().cloned().unwrap_or_default(),
                host_config.network_mode.clone(),
                exposed_ports.clone(),
                port_bindings_json,
                None,
                None,
                true,
            );
            let id = libpod.create_container(name, body).await?;
            tracing::info!("容器 {} 创建成功（ID: {}，keep-id）", name, id);
            return Ok(id);
        }
        // 非 user_home 路径同样走 libpod 端点创建（仅支持 podman；
        // 不带 userns，容器以 root 运行，行为与旧版一致）
        let mounts_json = serde_json::to_value(host_config.mounts.clone().unwrap_or_default())
            .map_err(|e| Error::Config(format!("序列化 mounts 失败：{e}")))?;
        let port_bindings_json = match &host_config.port_bindings {
            Some(pb) => Some(serde_json::to_value(pb)
                .map_err(|e| Error::Config(format!("序列化 port_bindings 失败：{e}")))?),
            None => None,
        };
        let libpod = crate::libpod::Libpod::new().await?;
        let body = crate::libpod::keep_id_create_body(
            name,
            image,
            vec![
                "/usr/bin/easytidy-server".to_string(),
                "--socket".to_string(),
                "/run/easytidy/server.sock".to_string(),
            ],
            env,
            labels,
            mounts_json.as_array().cloned().unwrap_or_default(),
            host_config.network_mode.clone(),
            exposed_ports,
            port_bindings_json,
            None,
            None,
            false,
        );
        let id = libpod.create_container(name, body).await?;
        tracing::info!("容器 {} 创建成功（ID: {}，libpod）", name, id);
        Ok(id)
    }

    /// 环境快照：commit 当前容器层为快照镜像（`easytidy/snapshot/<name>-<tag>`）。
    ///
    /// 仅容器文件系统层（bind mount 不入快照）；快照是独立资产，
    /// 删除环境不删快照（可被 fork 复用）。见 docs/13-mutable-env-paradigm.md。
    pub async fn snapshot(&self, name: &str, tag: &str) -> Result<String> {
        let image_ref = format!("easytidy/snapshot/{name}-{tag}");
        self.commit_container(name, &image_ref).await?;
        tracing::info!("环境 {} 快照完成：{}", name, image_ref);
        Ok(image_ref)
    }

    /// 重建容器（应用配置变更：mounts / 网络映射，创建后不可变 → 必须重建）。
    ///
    /// 流程：
    /// 1. `commit_container` 当前容器层为镜像 `localhost/easytidy-rebuild:<tag>`
    ///    （仅容器层；bind mount 不入 commit —— 正是所需）
    /// 2. stop（若运行中）→ remove（force）
    /// 3. `create_with_config`（同名，commit 的镜像 + 新配置，保留 Init/server-mount/labels）
    /// 4. start
    /// 5. 返回新容器 ID
    ///
    /// 错误处理：任一步失败给出中文可读错误；create 成功但 start 失败时
    /// 尽力删除新容器，不留下孤儿容器。调用方负责在成功后把 `config`
    /// 回写 configfile（GUI apply / CLI rebuild 均执行）。
    pub async fn rebuild(&self, name: &str, config: &ContainerConfig) -> Result<String> {
        let server_bin = crate::server_binary_path()?;

        // 1. commit 当前容器层（bind mount 不入镜像）
        let tag = Self::rebuild_image_tag(name);
        let image_ref = format!("localhost/easytidy-rebuild:{tag}");
        self.commit_container(name, &image_ref).await?;

        // 2. stop（若运行中）
        if self.is_running(name).await? {
            self.stop(name).await?;
        }

        // 3. remove（force）
        self.remove(name, true).await?;

        // 4. create（同名；失败时旧容器已删除，错误信息附带可恢复的镜像引用）
        let id = self
            .create_with_config(name, &image_ref, &server_bin, config)
            .await
            .map_err(|e| {
                Error::Connect(format!(
                    "重建失败：创建新容器未成功（旧容器已删除，可从镜像 {image_ref} 恢复）：{e}"
                ))
            })?;

        // 5. start；失败则尽力清理新容器（不留下孤儿）
        if let Err(e) = self.start(name).await {
            let _ = self.remove(name, true).await;
            return Err(Error::Connect(format!(
                "重建失败：新容器创建成功但启动失败（已尽力清理）：{e}"
            )));
        }

        Ok(id)
    }

    /// 检查容器当前生效的 mounts 与网络配置（GUI "当前生效" 状态）。
    ///
    /// 数据来源（等价 `podman inspect`）：
    /// - mounts: 顶层 `Mounts`（MountPoint 列表，podman 实际生效的挂载；
    ///   注意 podman 不回显 `HostConfig.Mounts`——create 时的 mounts 被归一化进
    ///   `HostConfig.Binds`，生效列表见顶层 `Mounts`）
    /// - 网络模式: `HostConfig.NetworkMode`（"host" → Host，其余如 "bridge"/"pasta" → Mapped）
    /// - 端口: `NetworkSettings.Ports`（实际生效的端口绑定）
    pub async fn inspect_config(&self, name: &str) -> Result<ContainerConfigView> {
        let info = self
            .docker
            .inspect_container(name, None)
            .await
            .map_err(|e| Error::Connect(format!("检查容器配置失败：{e}")))?;

        // mounts（仅 bind 类型；MountPoint.RW = true 表示可写）
        let mut mounts = Vec::new();
        if let Some(mount_list) = info.mounts.clone() {
            for m in mount_list {
                if m.typ != Some(bollard::models::MountPointTypeEnum::BIND) {
                    continue;
                }
                mounts.push(MountConfig {
                    host_path: m.source.unwrap_or_default(),
                    container_path: m.destination.unwrap_or_default(),
                    read_only: !m.rw.unwrap_or(true),
                });
            }
        }

        // 网络模式（"host" → Host，其余按 Mapped 展示）
        let host_network = info
            .host_config
            .as_ref()
            .and_then(|h| h.network_mode.as_deref())
            .is_some_and(|m| m == "host");

        // 端口映射（NetworkSettings.Ports：key = "<container_port>/<protocol>"）
        let mut ports = Vec::new();
        if let Some(port_map) = info.network_settings.as_ref().and_then(|n| n.ports.clone()) {
            for (key, bindings) in port_map {
                let Some((port, protocol)) = key.split_once('/') else {
                    continue;
                };
                let Ok(container_port) = port.parse::<u16>() else {
                    continue;
                };
                let host_port = bindings
                    .as_ref()
                    .and_then(|b| b.first())
                    .and_then(|b| b.host_port.as_ref())
                    .and_then(|p| p.parse::<u16>().ok())
                    .unwrap_or(0);
                ports.push(PortMapping {
                    host_port,
                    container_port,
                    protocol: protocol.to_string(),
                });
            }
            ports.sort_by_key(|p| p.container_port);
        }

        // env（含系统注入 EASYTIDY_USER_* 与 podman 自动补的 PATH/HOSTNAME/TERM/HOME——
        // GUI 对比时过滤后子集比较，见 ConfigManager.tsx envRestartEqual）
        let env = info
            .config
            .as_ref()
            .and_then(|c| c.env.clone())
            .unwrap_or_default();

        // 容器进程用户（keep-id 分支 create 时写 "0:0"）
        let user = info.config.as_ref().and_then(|c| c.user.clone());

        // userns 模式（keep-id 容器实际可能回显 "private"/None——语义以 user_home + docs/12 为准）
        let userns_mode = info.host_config.as_ref().and_then(|h| h.userns_mode.clone());

        Ok(ContainerConfigView {
            mounts,
            network: NetworkConfig {
                mode: if host_network {
                    NetworkMode::Host
                } else {
                    NetworkMode::Mapped
                },
                ports,
            },
            env,
            user,
            userns_mode,
        })
    }

    /// 查询容器是否运行中。
    async fn is_running(&self, name_or_id: &str) -> Result<bool> {
        let info = self
            .docker
            .inspect_container(name_or_id, None)
            .await
            .map_err(|e| Error::Connect(format!("检查容器状态失败：{e}")))?;
        Ok(info.state.and_then(|s| s.running).unwrap_or(false))
    }

    /// 提交容器当前层为镜像（bind mount 不入 commit）。
    pub(crate) async fn commit_container(&self, name: &str, image_ref: &str) -> Result<()> {
        use bollard::container::Config;
        use bollard::image::CommitContainerOptions;

        let (repo, tag) = image_ref
            .rsplit_once(':')
            .unwrap_or((image_ref, "latest"));
        let options = CommitContainerOptions {
            container: name.to_string(),
            repo: repo.to_string(),
            tag: tag.to_string(),
            comment: "easytidy rebuild snapshot".to_string(),
            author: "easytidy".to_string(),
            pause: true,
            changes: None,
        };
        let config = Config::<String>::default();
        self.docker
            .commit_container(options, config)
            .await
            .map_err(|e| Error::Connect(format!("提交容器快照失败（重建中止，容器未变更）：{e}")))?;
        Ok(())
    }

    /// 生成重建镜像 tag（容器名净化 + 时间戳，保证唯一）。
    fn rebuild_image_tag(name: &str) -> String {
        let ts = chrono::Utc::now().format("%Y%m%d%H%M%S%3f");
        let safe: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        format!("{safe}-{ts}")
    }

    /// 确保宿主 socket 目录存在（$XDG_RUNTIME_DIR/easytidy/<name>）。
    ///
    /// 容器配置 bind-mount 了该目录（容器内 /run/easytidy），而 bind 挂载
    /// 要求宿主源目录已存在——开机后 XDG_RUNTIME_DIR 被系统重建、目录消失，
    /// 未先建目录直接 start 报 runc mount 错误（实测 2026-08-09 重启复现）。
    async fn ensure_socket_dir(&self, name: &str) -> Result<()> {
        let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
            .map_err(|_| Error::NoXdgRuntime)?;
        let dir = PathBuf::from(runtime_dir).join("easytidy").join(name);
        tokio::fs::create_dir_all(&dir).await
            .map_err(|e| Error::Connect(format!("创建 socket 目录失败：{e}")))?;
        Ok(())
    }

    /// 启动容器（按名或 ID）。
    ///
    /// 启动成功后触发 passthrough auto-start 拉起（await——不能 spawn：
    /// CLI/GUI 进程短命，spawn 任务会在 runtime 关闭时被丢弃，实测不拉起；
    /// autostart_apps 内部 2s 连接重试兜底 server 就绪延迟，失败仅日志）。
    pub async fn start(&self, name_or_id: &str) -> Result<()> {
        use bollard::container::StartContainerOptions;

        // bind-mount 源目录须已存在：开机后重建（见 ensure_socket_dir）
        self.ensure_socket_dir(name_or_id).await?;

        self.docker.start_container(name_or_id, None::<StartContainerOptions<String>>).await
            .map_err(|e| Error::Connect(format!("启动容器失败：{e}")))?;

        tracing::info!("容器 {} 启动成功", name_or_id);
        crate::passthrough::autostart_apps(name_or_id).await;
        Ok(())
    }

    /// 停止容器（按名或 ID）。
    pub async fn stop(&self, name_or_id: &str) -> Result<()> {
        use bollard::container::StopContainerOptions;

        // 默认 10 秒超时
        let opts = StopContainerOptions {
            t: 10,
        };

        self.docker.stop_container(name_or_id, Some(opts)).await
            .map_err(|e| Error::Connect(format!("停止容器失败：{e}")))?;

        tracing::info!("容器 {} 停止成功", name_or_id);
        Ok(())
    }

    /// 重启容器（按名或 ID）。
    ///
    /// 重启语义即重新 boot：同样触发 passthrough auto-start 拉起（await，
    /// 原因见 [`Podman::start`]）。
    pub async fn restart(&self, name_or_id: &str) -> Result<()> {
        use bollard::container::RestartContainerOptions;

        // bind-mount 源目录须已存在：开机后重建（见 ensure_socket_dir）
        self.ensure_socket_dir(name_or_id).await?;

        // 默认 10 秒超时
        let opts = RestartContainerOptions {
            t: 10,
        };

        self.docker.restart_container(name_or_id, Some(opts)).await
            .map_err(|e| Error::Connect(format!("重启容器失败：{e}")))?;

        tracing::info!("容器 {} 重启成功", name_or_id);
        crate::passthrough::autostart_apps(name_or_id).await;
        Ok(())
    }

    /// 删除容器（按名或 ID）。
    ///
    /// force: 是否强制删除（运行中的容器需要 force=true）。
    pub async fn remove(&self, name_or_id: &str, force: bool) -> Result<()> {
        use bollard::container::RemoveContainerOptions;

        let opts = RemoveContainerOptions {
            force,
            v: false, // 不删除匿名卷
            ..Default::default()
        };

        self.docker.remove_container(name_or_id, Some(opts)).await
            .map_err(|e| Error::Connect(format!("删除容器失败：{e}")))?;

        tracing::info!("容器 {} 删除成功", name_or_id);
        Ok(())
    }

    /// 检查镜像是否存在。
    async fn image_exists(&self, image: &str) -> Result<bool> {
        match self.docker.inspect_image(image).await {
            Ok(_) => Ok(true),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(false),
            Err(e) => Err(Error::Api(e)),
        }
    }

    /// 拉取镜像。
    /// 列出所有镜像（GUI 镜像管理）。
    pub async fn list_images(&self) -> Result<Vec<crate::models::ImageSummary>> {
        use bollard::image::ListImagesOptions;

        let images = self
            .docker
            .list_images(Some(ListImagesOptions::<String> {
                all: false,
                ..Default::default()
            }))
            .await
            .map_err(Error::Api)?;

        Ok(images
            .into_iter()
            .map(|i| crate::models::ImageSummary {
                // bollard ImageSummary 字段非 Option（id: String, repo_tags: Vec, ...）
                id: i
                    .id
                    .trim_start_matches("sha256:")
                    .chars()
                    .take(12)
                    .collect(),
                repo_tags: i.repo_tags,
                size: i.size as u64,
                created: i.created,
            })
            .collect())
    }

    /// 删除镜像（force 强制删除被引用镜像）。
    pub async fn remove_image(&self, name: &str, force: bool) -> Result<()> {
        use bollard::image::RemoveImageOptions;

        self.docker
            .remove_image(
                name,
                Some(RemoveImageOptions {
                    force,
                    ..Default::default()
                }),
                None,
            )
            .await
            .map_err(Error::Api)?;
        tracing::info!("镜像已删除：{name}");
        Ok(())
    }

    pub async fn pull_image(&self, image: &str) -> Result<()> {
        use bollard::image::CreateImageOptions;
        use futures::StreamExt;

        let opts = CreateImageOptions {
            from_image: image,
            ..Default::default()
        };

        let mut stream = self.docker.create_image(Some(opts), None, None);

        while let Some(result) = stream.next().await {
            match result {
                Ok(progress) => {
                    if let Some(id) = progress.id {
                        tracing::debug!("拉取进度：{} - {:?}", id, progress.status);
                    }
                }
                Err(e) => return Err(Error::Connect(format!("拉取镜像失败：{e}"))),
            }
        }

        tracing::info!("镜像 {} 拉取成功", image);
        Ok(())
    }
}

/// 挂载列表是否已包含目标路径（避免与用户配置/引擎挂载重复目标）。
fn mount_has_target(mounts: &[bollard::models::Mount], target: &str) -> bool {
    mounts.iter().any(|m| m.target.as_deref() == Some(target))
}

/// 校验 bind mount 配置（bind 类型要求宿主路径已存在）。
fn validate_mount(m: &MountConfig) -> Result<()> {
    if m.host_path.is_empty() {
        return Err(Error::Config(
            "挂载配置缺少宿主路径（host_path 为空）".to_string(),
        ));
    }
    if m.container_path.is_empty() {
        return Err(Error::Config(
            "挂载配置缺少容器内路径（container_path 为空）".to_string(),
        ));
    }
    if !Path::new(&m.host_path).exists() {
        return Err(Error::Config(format!(
            "挂载路径不存在：{}（bind mount 的宿主路径必须已存在，请先创建该目录/文件）",
            m.host_path
        )));
    }
    Ok(())
}
