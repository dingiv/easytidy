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
    ContainerConfig, ContainerConfigView, ContainerParams, ContainerSummary, MountConfig,
    NetworkConfig, NetworkMode, PortMapping,
};

/// 宿主侧 exec（PTY 会话 + 非 tty 一次性；见 exec.rs）
pub mod exec;
pub use exec::{ExecOnce, ExecPty};
/// 容器内用户准备（root 一次性 exec：useradd/fontconfig；见 user.rs）
pub mod user;

/// Podman 客户端封装。
pub struct Podman {
    /// bollard Docker 实例（Docker compat API）
    pub(super) docker: Docker,
}

impl Podman {
    /// ctool 二进制的容器内挂载目标（prepare_container 的 exec 目标）。
    pub(crate) const CTOOL_TARGET: &str = "/usr/bin/easytidy-ctool";

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
        bins: &crate::ContainerBins,
    ) -> Result<String> {
        let config = ContainerConfig {
            name: name.to_string(),
            params: ContainerParams {
                image: image.to_string(),
                // 保持既有语义：默认 bridge 网络 + 无端口映射
                network: NetworkConfig {
                    mode: NetworkMode::Mapped,
                    ports: Vec::new(),
                },
                ..Default::default()
            },
            ..Default::default()
        };
        self.create_with_config(name, image, bins, &config).await
    }

    /// 创建容器并应用完整配置（mounts / 网络映射）。
    ///
    /// 参数：
    /// - name: 容器名
    /// - image: 镜像（如 "docker.io/library/alpine:latest"）
    /// - bins: 容器内二进制（server + ctool，宿主绝对路径，均 ro bind-mount 进容器）
    /// - config: 容器配置（mounts + 网络模式/端口映射）
    ///
    /// 在 `create()` 的既有基础之上追加（rebuild 保留同一套基础）：
    /// - HostConfig.init = true（catatonit = PID 1）
    /// - Cmd = [<server-bin>, "--socket", "/run/easytidy/server.sock"]
    /// - Bind mounts: server 二进制（ro）+ ctool 二进制（ro）+ socket 目录（rw）
    ///   + config.params.mounts（宿主路径须已存在）
    /// - 标签: manager=easytidy + easytidy.name=<name>
    /// - 网络: `Host` → `network_mode = "host"`（端口映射无意义，忽略并告警）；
    ///   `Mapped` → 不设 network_mode（podman 默认 bridge）+ ExposedPorts + PortBindings
    /// - 容器默认用户（新模型）：`User` 字段 = 配置 uid:gid（缺省 = 宿主登录用户），
    ///   server 直接以该用户运行（无 root、无 su，见 crates/server）
    /// - keep-id（`config.params.keep_id`）：仅影响用户命名空间映射，**不再
    ///   绑定挂载宿主 home**（2026-08-28 定案：家目录 = 容器默认用户的
    ///   passwd home，容器层持久，由 `prepare_container` 的 ensure-home
    ///   创建/chown）
    /// - 身份提示 env：`EASYTIDY_USER_NAME`（配置用户名时注入，server
    ///   身份自发现用）
    ///
    /// 镜像不存在则先拉取。
    pub async fn create_with_config(
        &self,
        name: &str,
        image: &str,
        bins: &crate::ContainerBins,
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

        // 创建宿主 socket 目录（$XDG_RUNTIME_DIR/easytidy/<name>-<config-hash>；
        // bind-mount 源须先于容器存在，目录名由最终配置哈希派生——同一容器因
        // 配置变化换代时目录名跟着变，各代互不混淆）
        let socket_host_dir = crate::socket_dir_for(name, config)?;

        tokio::fs::create_dir_all(&socket_host_dir).await
            .map_err(|e| Error::Connect(format!("创建 socket 目录失败：{e}")))?;

        // 构建标签
        let mut labels = HashMap::new();
        labels.insert("manager".to_string(), "easytidy".to_string());
        labels.insert("easytidy.name".to_string(), name.to_string());

        // 构建挂载：server 二进制 + ctool 二进制 + socket 目录 + 用户配置的 bind mounts
        let mut mounts = vec![
            // Server 二进制（只读）
            Mount {
                typ: Some(MountTypeEnum::BIND),
                source: Some(bins.server.to_string_lossy().to_string()),
                target: Some("/usr/bin/easytidy-server".to_string()),
                read_only: Some(true),
                ..Default::default()
            },
            // ctool 二进制（只读）：容器内 root 一次性工具（prepare_container
            // 的 exec 目标；musl 静态，零容器内命令依赖）
            Mount {
                typ: Some(MountTypeEnum::BIND),
                source: Some(bins.ctool.to_string_lossy().to_string()),
                target: Some(Self::CTOOL_TARGET.to_string()),
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
        for m in &config.params.mounts {
            validate_mount(m)?;
            mounts.push(Mount {
                typ: Some(MountTypeEnum::BIND),
                source: Some(m.host_path.clone()),
                target: Some(m.container_path.clone()),
                read_only: Some(m.read_only),
                ..Default::default()
            });
        }

        // 容器默认用户解析（新模型）：配置值优先，缺省取宿主登录用户；
        // 均不可得 → 报错（不再静默回退 root——root 模型已移除）
        let host = crate::userenv::host_user();
        let (user_uid, user_gid) = resolve_container_user(&config.params, host.as_ref())?;

        // 身份提示 env（server 侧身份自发现的兜底输入）：
        // EASYTIDY_USER_NAME = 配置用户名（与 prepare_container 的 useradd
        // 命名一致；镜像无该 uid 真实条目且 useradd 尚未执行时的名字来源）。
        // 不再注入 EASYTIDY_HOME（2026-08-28 定案：家目录跟随容器默认用户
        // 的 passwd home、容器层持久，keep-id 不再绑定挂载宿主 home）
        let mut env = config.env.clone();
        if let Some(name) = &config.params.user_name {
            env.push(format!("EASYTIDY_USER_NAME={name}"));
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
        match config.params.network.mode {
            NetworkMode::Host => {
                host_config.network_mode = Some("host".to_string());
                if !config.params.network.ports.is_empty() {
                    tracing::warn!(
                        "容器 {} 网络模式为 host，端口映射不生效（已忽略）：{:?}",
                        name,
                        config.params.network.ports
                    );
                }
            }
            NetworkMode::Mapped => {
                // 不设 network_mode → podman 默认 bridge
                if !config.params.network.ports.is_empty() {
                    let mut exposed = HashMap::new();
                    let mut bindings: HashMap<String, Option<Vec<PortBinding>>> = HashMap::new();
                    for p in &config.params.network.ports {
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

        // server Cmd：entry 链式拉起接通（此前 `entry` 字段存而不用——server
        // 支持 --entry 但创建时从未传入）。有 entry 才追加；entry + args
        // 拼为一条命令串（server 经 su -c shell 执行，含空格参数需引号）
        //
        // --log-file 把 server 的 tracing + eprintln 都写到 bind-mount 的
        // /run/easytidy 下的日志文件，方便开发期 `tail` 容器外看 server
        // 内部报错（不依赖 `podman logs`，且容器重启不丢历史）。
        let mut server_cmd = vec![
            "/usr/bin/easytidy-server".to_string(),
            "--socket".to_string(),
            "/run/easytidy/server.sock".to_string(),
            "--log-file".to_string(),
            "/run/easytidy/server.log".to_string(),
        ];
        if let Some(entry) = config
            .params
            .entry
            .as_ref()
            .filter(|e| !e.trim().is_empty())
        {
            let mut entry_cmd = entry.trim().to_string();
            for arg in config.params.entry_args.iter().filter(|a| !a.trim().is_empty()) {
                entry_cmd.push(' ');
                entry_cmd.push_str(arg.trim());
            }
            server_cmd.push("--entry".to_string());
            server_cmd.push(entry_cmd);
        }

        // 容器默认用户（新模型）：User = <uid>:<gid>，server 直接以该用户运行
        // （无 root、无 su）。keep-id 时宿主登录 uid ↔ 容器同 uid 锁死（docs/12）。
        let user_spec = format!("{user_uid}:{user_gid}");

        // keep-id → 必须走 libpod 端点（Docker compat 端点不支持 userns.keep-id，
        // 实测；见 libpod.rs）。keep-id 使容器内 uid 1000 = 宿主当前登录用户：
        // 宿主 home 读写 / /run/user/1000（显示 socket）自然可达（GUI 窗口可用）。
        if config.params.keep_id {
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
                server_cmd.clone(),
                env.clone(),
                labels.clone(),
                mounts_json.as_array().cloned().unwrap_or_default(),
                host_config.network_mode.clone(),
                exposed_ports.clone(),
                port_bindings_json,
                None,
                Some(user_spec.as_str()),
                true,
                config.params.devices.clone(),
                config.params.gpu.as_deref(),
                config.params.pid.as_deref(),
                config.params.security_opts.clone(),
            );
            let id = libpod.create_container(name, body).await?;
            tracing::info!("容器 {} 创建成功（ID: {}，keep-id）", name, id);
            return Ok(id);
        }
        // 非 keep-id 路径同样走 libpod 端点创建（仅支持 podman；
        // 不带 userns，容器内 uid 落在宿主 subuid 段，User 字段仍为配置 uid:gid）
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
            server_cmd,
            env,
            labels,
            mounts_json.as_array().cloned().unwrap_or_default(),
            host_config.network_mode.clone(),
            exposed_ports,
            port_bindings_json,
            None,
            Some(user_spec.as_str()),
            false,
            config.params.devices.clone(),
            config.params.gpu.as_deref(),
            config.params.pid.as_deref(),
            config.params.security_opts.clone(),
        );
        let id = libpod.create_container(name, body).await?;
        tracing::info!("容器 {} 创建成功（ID: {}，libpod）", name, id);
        Ok(id)
    }

    /// 环境快照:以 `commit --squash` 把运行中容器打成扁平镜像
    /// `easytidy/snapshot/<name>-<tag>`。
    ///
    /// 实现:走 libpod 直连(`libpod::commit_squash`),与项目既有的 keep-id
    /// 创建路径走同一条 podman socket 直连栈。
    ///
    /// 选用 `--squash` 的语义:扁平文件系统镜像(单层),不保留容器原本的分层历史。
    /// 与早期 `commit_container`(多层)相比,体积更小、fork 出新容器时不再
    /// 叠加源容器的所有中间层,适合作为"环境快照"语义使用。
    ///
    /// 已知限制:
    /// - **运行中容器不保证快照一致性**(`commit` 配合 `pause=true` 默认,
    ///   与 `export` 不同——导出文件层时容器进程短暂暂停后继续运行,
    ///   暂停期间应用通常感知不到)。若对一致性敏感可先 `env_stop` 再快照。
    /// - **bind mount 不入快照**(podman 自身行为,与 commit 是否 squash 无关)。
    ///
    /// 快照是独立资产,删除环境不删快照(可被 fork 复用)。
    /// 见 docs/13-mutable-env-paradigm.md。
    pub async fn snapshot(&self, name: &str, tag: &str) -> Result<String> {
        let image_ref = format!("easytidy/snapshot/{name}-{tag}");
        let libpod = crate::libpod::Libpod::new().await?;
        libpod
            .commit_squash(name, &image_ref, "easytidy snapshot via commit --squash")
            .await?;
        tracing::info!("环境 {} 快照完成:{}", name, image_ref);
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
    pub async fn rebuild(
        &self,
        name: &str,
        config: &ContainerConfig,
        bins: &crate::ContainerBins,
    ) -> Result<String> {
        // 先记下现有 socket 目录（重建会以新 hash 换代命名；成功后清理旧代孤儿。
        // config 未变时新旧同名——按新目录做白名单，见第 6 步）
        let legacy_socket_dirs = crate::resolve_socket_dirs(name);

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
            .create_with_config(name, &image_ref, bins, config)
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

        // 6. 清理旧代 socket 目录（新代已由 create 以新 hash 命名；config 未变时
        //    新旧同名 → 跳过当前代，避免误删正在使用的目录）
        let new_socket_dir = crate::socket_dir_for(name, config)?;
        for legacy in legacy_socket_dirs {
            if legacy != new_socket_dir {
                tracing::debug!("重建后清理旧代 socket 目录：{}", legacy.display());
                let _ = std::fs::remove_dir_all(&legacy);
            }
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

        // 容器进程用户（新模型 = 配置值 <uid>:<gid>；旧 root 容器为 "0:0"）
        let user = info.config.as_ref().and_then(|c| c.user.clone());

        // userns 模式（keep-id 容器实际可能回显 "private"/None——语义以 keep_id + docs/12 为准）
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

    /// 确保宿主 socket 目录存在（`$XDG_RUNTIME_DIR/easytidy/<name>-<hash>`，兼容旧 `<name>`）。
    ///
    /// 容器配置 bind-mount 了该目录（容器内 /run/easytidy），而 bind 挂载要求宿主源
    /// 目录已存在——开机后 XDG_RUNTIME_DIR 被系统重建、目录消失，未先建目录直接 start
    /// 报 runc mount 错误（实测 2026-08-09 重启复现）。解析走 `host_socket_path`
    /// （现有目录优先，无则按 configfile 重算 hash），不依赖注册时序、兼容新旧命名。
    async fn ensure_socket_dir(&self, name: &str) -> Result<()> {
        let sock = crate::host_socket_path(name)?;
        let Some(dir) = sock.parent() else {
            return Err(Error::Connect("socket 路径无父目录".to_string()));
        };
        tokio::fs::create_dir_all(dir).await
            .map_err(|e| Error::Connect(format!("创建 socket 目录失败：{e}")))?;
        Ok(())
    }

    /// 启动容器（按名或 ID）。
    ///
    /// auto-start 应用由**容器内 server 启动自读拉起**（配置在容器内，容器自包含），
    /// 宿主侧不再推送。
    pub async fn start(&self, name_or_id: &str) -> Result<()> {
        use bollard::container::StartContainerOptions;

        // bind-mount 源目录须已存在：开机后重建（见 ensure_socket_dir）
        self.ensure_socket_dir(name_or_id).await?;

        self.docker.start_container(name_or_id, None::<StartContainerOptions<String>>).await
            .map_err(|e| Error::Connect(format!("启动容器失败：{e}")))?;

        tracing::info!("容器 {} 启动成功", name_or_id);
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
        Ok(())
    }

    /// 删除容器（按名或 ID）。**幂等**：容器本就不存在（被外部 Podman 客户端删过）
    /// 时按成功处理——删除目标已达成，调用方因此能继续后续清理（配置注销/图标/socket）。
    ///
    /// force: 是否强制删除（运行中的容器需要 force=true）。
    pub async fn remove(&self, name_or_id: &str, force: bool) -> Result<()> {
        use bollard::container::RemoveContainerOptions;

        let opts = RemoveContainerOptions {
            force,
            v: false, // 不删除匿名卷
            ..Default::default()
        };

        match self.docker.remove_container(name_or_id, Some(opts)).await {
            Ok(_) => {}
            // 容器本就不存在（被外部 Podman 客户端删过）——删除幂等，目标已达成，不报错
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => {
                tracing::info!("容器 {name_or_id} 本就不存在，视为已删除（外部删除）");
            }
            Err(e) => return Err(Error::Connect(format!("删除容器失败：{e}"))),
        }

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

    /// 镜像占用查询：返回每个镜像被哪些容器使用（入参原样 -> 容器名列表）。
    ///
    /// 按镜像 **内容寻址 ID** 精确匹配：先把每个目标镜像（tag / 短 ID / 完整 ID
    /// 均可）解析成完整 ID，再取容器列表里每个容器的 `ImageID` 比对命中。故
    /// 「按 tag 创建后被 re-tag」「悬空镜像仍被引用」等场景也能正确识别；未被
    /// 任何容器引用的镜像返回空列表（可安全删除）。镜像本身不存在（inspect 失败）
    /// 按无占用处理（必然可删）。
    pub async fn images_used_by(
        &self,
        images: &[String],
    ) -> Result<std::collections::HashMap<String, Vec<String>>> {
        use bollard::container::ListContainersOptions;

        // 1. 解析每个目标镜像的完整 ID（去 `sha256:` 前缀归一，便于比对）。
        let norm = |s: String| s.trim_start_matches("sha256:").to_string();
        let mut image_ids: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for image in images {
            match self.docker.inspect_image(image).await {
                Ok(img) => {
                    image_ids.insert(image.clone(), norm(img.id.unwrap_or_default()));
                }
                Err(_) => {
                    // 镜像不存在 → 无容器占用（必然可删）
                    image_ids.insert(image.clone(), String::new());
                }
            }
        }

        // 2. 列出所有容器（list 响应已带 ImageID，all=true 含已停止）。
        let containers = self
            .docker
            .list_containers(Some(ListContainersOptions::<String> {
                all: true,
                ..Default::default()
            }))
            .await
            .map_err(Error::Api)?;

        // 3. 按 ImageID 命中收集容器名。
        let mut result: std::collections::HashMap<String, Vec<String>> = images
            .iter()
            .map(|i| (i.clone(), Vec::new()))
            .collect();
        for c in containers {
            let image_id = norm(c.image_id.unwrap_or_default());
            if image_id.is_empty() {
                continue;
            }
            let name = c
                .names
                .as_ref()
                .and_then(|n| n.first())
                .map(|s| s.trim_start_matches('/').to_string())
                .unwrap_or_default();
            if name.is_empty() {
                continue;
            }
            for (image, id) in &image_ids {
                if !id.is_empty() && *id == image_id {
                    result.get_mut(image).unwrap().push(name.clone());
                }
            }
        }
        Ok(result)
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

/// 解析容器默认用户 uid/gid（新模型）：配置值优先，缺省取宿主登录用户。
///
/// 两者均不可得（未配置 + `host_user()` 探测失败）→ `Error::Config`——
/// 不再静默回退 root（root 模型已移除，`user="0:0"` 创建被禁止）。
fn resolve_container_user(
    params: &ContainerParams,
    host: Option<&crate::userenv::HostUser>,
) -> Result<(u32, u32)> {
    let uid = params
        .user_uid
        .or_else(|| host.map(|h| h.uid))
        .ok_or_else(|| {
            Error::Config(
                "无法确定容器默认用户：未配置 user_uid 且宿主用户探测失败（host_user() = None）".to_string(),
            )
        })?;
    let gid = params
        .user_gid
        .or_else(|| host.map(|h| h.gid))
        .ok_or_else(|| {
            Error::Config(
                "无法确定容器默认用户：未配置 user_gid 且宿主用户探测失败（host_user() = None）".to_string(),
            )
        })?;
    Ok((uid, gid))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::userenv::HostUser;

    fn host(uid: u32, gid: u32) -> HostUser {
        HostUser {
            name: "tester".into(),
            uid,
            gid,
            home: "/home/tester".into(),
        }
    }

    #[test]
    fn test_resolve_container_user_config_wins() {
        // 配置值优先于宿主值
        let params = ContainerParams {
            user_uid: Some(1001),
            user_gid: Some(1002),
            ..Default::default()
        };
        assert_eq!(resolve_container_user(&params, Some(&host(1000, 1000))).unwrap(), (1001, 1002));
    }

    #[test]
    fn test_resolve_container_user_host_fallback() {
        // 缺省 → 宿主登录 uid/gid
        let params = ContainerParams::default();
        assert_eq!(resolve_container_user(&params, Some(&host(1000, 1000))).unwrap(), (1000, 1000));
    }

    #[test]
    fn test_resolve_container_user_mixed() {
        // uid 配置、gid 缺省 → 混用（uid 配置值 + gid 宿主值）
        let params = ContainerParams {
            user_uid: Some(1001),
            ..Default::default()
        };
        assert_eq!(resolve_container_user(&params, Some(&host(1000, 1000))).unwrap(), (1001, 1000));
    }

    #[test]
    fn test_resolve_container_user_both_missing() {
        // 未配置 + 宿主探测失败 → 报错（禁止静默回退 root）
        let params = ContainerParams::default();
        let err = resolve_container_user(&params, None).unwrap_err();
        assert!(matches!(err, Error::Config(_)));
        assert!(err.to_string().contains("无法确定容器默认用户"));
    }

    #[test]
    fn test_libpod_body_user_field() {
        // keep_id_create_body：default_user 透传 + keep-id 时顶层 userns 块
        let body = crate::libpod::keep_id_create_body(
            "c1",
            "alpine:latest",
            vec!["/bin/sh".into()],
            Vec::new(),
            std::collections::HashMap::new(),
            Vec::new(),
            None,
            None,
            None,
            None,
            Some("1000:1000"),
            true,
            Vec::new(),
            None,
            None,
            Vec::new(),
        );
        assert_eq!(body["user"], "1000:1000");
        assert_eq!(body["userns"]["nsmode"], "keep-id");

        // default_user = None → "0:0"（旧行为保留，仅供无身份场景）
        let body = crate::libpod::keep_id_create_body(
            "c1",
            "alpine:latest",
            vec!["/bin/sh".into()],
            Vec::new(),
            std::collections::HashMap::new(),
            Vec::new(),
            None,
            None,
            None,
            None,
            None,
            false,
            Vec::new(),
            None,
            None,
            Vec::new(),
        );
        assert_eq!(body["user"], "0:0");
        assert!(body.get("userns").is_none());
    }

    #[test]
    fn test_libpod_body_portmappings_libpod_shape() {
        // 入参为 bollard PortBinding 序列化的嵌套形状（**PascalCase**
        // HostIp/HostPort，HostPort 为字符串）；libpod SpecGenerator 需要
        // 扁平 `portmappings` []PortMapping（host_port/container_port 为
        // 数字，container_port/protocol 从键推导）——形状不对时 libpod 静默
        // 忽略（socket 实测）。
        let bindings = serde_json::json!({
            "80/tcp": [
                { "HostIp": null, "HostPort": "18080" }
            ]
        });
        let body = crate::libpod::keep_id_create_body(
            "c1",
            "alpine:latest",
            vec!["/bin/sh".into()],
            Vec::new(),
            std::collections::HashMap::new(),
            Vec::new(),
            None,
            None,
            Some(bindings),
            None,
            Some("1000:1000"),
            false,
            Vec::new(),
            None,
            None,
            Vec::new(),
        );
        let pm = &body["portmappings"][0];
        assert_eq!(pm["container_port"], 80);
        assert_eq!(pm["protocol"], "tcp");
        assert_eq!(pm["host_port"], 18080); // 数字（uint16），非字符串
        assert!(body.get("port_bindings").is_none());
    }

    #[test]
    fn test_libpod_body_device_fields() {
        let make = |devices: Vec<String>, gpu: Option<&str>, pid: Option<&str>, security: Vec<String>| {
            crate::libpod::keep_id_create_body(
                "c1",
                "alpine:latest",
                vec!["/bin/sh".into()],
                Vec::new(),
                std::collections::HashMap::new(),
                Vec::new(),
                None,
                None,
                None,
                None,
                Some("1000:1000"),
                true,
                devices,
                gpu,
                pid,
                security,
            )
        };

        let to_vec = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        // 全部为空（旧行为）：不出现 devices/pidns/security 字段，init 保持 true
        let body = make(Vec::new(), None, None, Vec::new());
        assert!(body.get("devices").is_none());
        assert!(body.get("pidns").is_none());
        assert!(body.get("apparmor_profile").is_none());
        assert!(body.get("selinux_opts").is_none());
        assert!(body.get("seccomp_profile_path").is_none());
        assert_eq!(body["init"], true);

        // gpu = "all" → nvidia.com/gpu=all CDI 引用（与 podman --gpus all 等价）
        let body = make(Vec::new(), Some("all"), None, Vec::new());
        assert_eq!(body["devices"][0]["path"], "nvidia.com/gpu=all");

        // 裸设备直通
        let body = make(to_vec(&["/dev/uinput:/dev/uinput"]), None, None, Vec::new());
        assert_eq!(body["devices"][0]["path"], "/dev/uinput:/dev/uinput");

        // gpu + 裸设备共存
        let body = make(to_vec(&["/dev/uinput:/dev/uinput"]), Some("0"), None, Vec::new());
        assert_eq!(body["devices"][0]["path"], "/dev/uinput:/dev/uinput");
        assert_eq!(body["devices"][1]["path"], "nvidia.com/gpu=0");

        // pid = host → pidns.nsmode = host 且 init 禁用（catatonit 无法进 host PID ns）
        let body = make(Vec::new(), None, Some("host"), Vec::new());
        assert_eq!(body["pidns"]["nsmode"], "host");
        assert_eq!(body["init"], false);

        // pid = private（显式）→ 无 pidns 字段，init 保持
        let body = make(Vec::new(), None, Some("private"), Vec::new());
        assert!(body.get("pidns").is_none());
        assert_eq!(body["init"], true);

        // security_opts 解析：label → selinux_opts，apparmor → apparmor_profile，
        // seccomp → seccomp_profile_path（与 podman CLI --security-opt 映射一致）
        let body = make(
            Vec::new(),
            None,
            None,
            to_vec(&["label=disable", "apparmor=unconfined", "seccomp=unconfined"]),
        );
        assert_eq!(body["selinux_opts"][0], "disable");
        assert_eq!(body["apparmor_profile"], "unconfined");
        assert_eq!(body["seccomp_profile_path"], "unconfined");

        // 未知 key 忽略，无 '=' 的串忽略（不 panic、不产生字段）
        let body = make(Vec::new(), None, None, to_vec(&["mask=/foo", "nonsense"]));
        assert!(body.get("apparmor_profile").is_none());
        assert!(body.get("selinux_opts").is_none());
    }
}
