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

use std::collections::HashSet;
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
    /// easytidy 容器内二进制的挂载目录（tmpfs `/run` 下，学 podman-init）。
    ///
    /// 用 `/run/easytidy-bin/`（**不是** `/run/easytidy/bin`）——`/run/easytidy`
    /// 已被宿主 socket 目录 bind-mount 占住，bin 放其下会落成宿主侧残留文件。
    /// 三个二进制都 bind-mount 到此处，容器镜像不污染 `/usr/bin`，进程命令行
    /// 统一归到 `/run` 下。
    pub const BIN_DIR: &str = "/run/easytidy-bin";

    /// server 二进制的容器内挂载目标（容器 PID 1 入口）。
    pub const SERVER_TARGET: &str = "/run/easytidy-bin/easytidy-server";

    /// dock 二进制的容器内挂载目标（容器内 root 工具：prepare 容器准备 +
    /// daemon/bootstrap/client root 终端通道，源自 root-channel + ctool 合并）。
    ///
    /// - `prepare`：prepare_container 的 exec 目标（fontconfig/建号/家目录）。
    /// - `daemon`/`bootstrap`/`client`：root 终端通道（`bootstrap` 起 daemon、
    ///   `client` 桥 stdio）。宿主页通过 `podman exec --user 0` 拉起，daemon
    ///   与容器共生死。
    ///
    /// **exec 必须用容器内路径**（宿主侧 `dock_binary_path()` 是 dev
    /// 相对路径 `crates/core/../../target/...`，runc 在容器命名空间 stat 不到
    /// → "no such file or directory"）。GUI/CLI 一律 exec 本常量。
    pub const DOCK_TARGET: &str = "/run/easytidy-bin/easytidy-dock";

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
    /// - bins: 容器内二进制（server + dock，宿主绝对路径，均 ro bind-mount 进容器）
    /// - config: 容器配置（mounts + 网络模式/端口映射）
    ///
    /// 在 `create()` 的既有基础之上追加（rebuild 保留同一套基础）：
    /// - HostConfig.init = true（catatonit = PID 1）
    /// - Cmd = [<server-bin>, "--socket", "/run/easytidy/server.sock"]
    /// - Bind mounts: server 二进制（ro）+ dock 二进制（ro）+ socket 目录（rw）
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

        // 创建宿主 socket 目录（$XDG_RUNTIME_DIR/easytidy/<name>；bind-mount 源须
        // 先于容器存在，按容器名寻址——纯 name 不依赖 config，避免 config 漂移分叉）
        let socket_host_dir = crate::socket_dir_for(name)?;

        tokio::fs::create_dir_all(&socket_host_dir).await
            .map_err(|e| Error::Connect(format!("创建 socket 目录失败：{e}")))?;

        // 构建标签
        let mut labels = HashMap::new();
        labels.insert("manager".to_string(), "easytidy".to_string());
        labels.insert("easytidy.name".to_string(), name.to_string());

        // 构建挂载：server 二进制 + dock 二进制 + socket 目录 + 用户配置的
        // bind mounts
        let mut mounts = vec![
            // Server 二进制（只读）
            Mount {
                typ: Some(MountTypeEnum::BIND),
                source: Some(bins.server.to_string_lossy().to_string()),
                target: Some(Self::SERVER_TARGET.to_string()),
                read_only: Some(true),
                ..Default::default()
            },
            // dock 二进制（只读）：容器内 root 工具（prepare 容器准备 + root
            // 终端通道 daemon/client；musl 静态，零容器内命令依赖）
            Mount {
                typ: Some(MountTypeEnum::BIND),
                source: Some(bins.dock.to_string_lossy().to_string()),
                target: Some(Self::DOCK_TARGET.to_string()),
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
        // 容器默认用户解析（新模型）：配置值优先，缺省取宿主登录用户；
        // 均不可得 → 报错（不再静默回退 root——root 模型已移除）。
        // 上移到挂载展开前——容器侧 ${HOME}/${USER} 展开需容器 uid/gid/user_name。
        let host = crate::userenv::host_user();
        let (user_uid, user_gid) = resolve_container_user(&config.params, host.as_ref())?;

        // 挂载路径变量展开（${HOME}/${USER}/${UID}/${GID}，宿主侧/容器侧上下文
        // 相关）：占位符 → 具体路径，随后 validate_mount 校验。容器侧 HOME/USER
        // 且未配 user_name 时探测镜像 /etc/passwd（缓存）解析容器用户 home/name。
        // 无 ${} 的挂载走快速路径（零探测零成本）。
        let user_mounts = self
            .expand_user_mounts(
                image,
                &config.params.mounts,
                host.as_ref(),
                user_uid,
                user_gid,
                config.params.user_name.as_deref(),
            )
            .await?;

        for m in &user_mounts {
            validate_mount(m)?;
            mounts.push(Mount {
                typ: Some(MountTypeEnum::BIND),
                source: Some(m.host_path.clone()),
                target: Some(m.container_path.clone()),
                read_only: Some(m.read_only),
                ..Default::default()
            });
        }

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
            Self::SERVER_TARGET.to_string(),
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

        // AMD GPU 裸设备探测（create 期，不落盘——render 节点号随重启漂移，配置
        // 只存意图 bool）。gpu_amd 开了但宿主探测不到任何 AMD 设备 → 直接报可读
        // 错误（避免静默无 GPU 或 podman unresolvable CDI 玄学报错）。
        let amd_gpu_devices = if config.params.gpu_amd {
            let devs = crate::env::host::detect_amd_gpu_devices();
            if devs.is_empty() {
                return Err(Error::Config(format!(
                    "容器 {name} 开启了 AMD GPU 透传（gpu_amd），但宿主未探测到 \
                     AMD GPU：/dev/dri 无 AMD（PCI vendor 0x1002）render 节点且 \
                     无 /dev/kfd"
                )));
            }
            devs
        } else {
            Vec::new()
        };

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
                config.params.gpu_nvidia,
                amd_gpu_devices,
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
            config.params.gpu_nvidia,
            amd_gpu_devices,
            config.params.pid.as_deref(),
            config.params.security_opts.clone(),
        );
        let id = libpod.create_container(name, body).await?;
        tracing::info!("容器 {} 创建成功（ID: {}，libpod）", name, id);
        Ok(id)
    }

    /// 挂载去重（最后一道防墙）：按 `container_path` 去重，保留先出现的；同目标
    /// 不同 host_path 或 read_only 不同的告警（用户最可能需要知道）。空
    /// `container_path` 视为无效丢弃（libpod 会拒绝）。
    ///
    /// **加在 `expand_user_mounts` 末尾**：路径变量已展开为绝对路径，比较无歧义；
    /// 在 mount 进入 `host_config.mounts` 之前 → podman 不会再因重复 destination
    /// 报 HTTP 500。引擎保留目标（`/run/easytidy-bin/easytidy-server` 等）由后续 push 阶段
    /// 处理（用户 mount 撞引擎目标会让 podman 拒——这是预期行为，不在此去重）。
    fn dedup_mounts(mounts: &mut Vec<MountConfig>) {
        let mut seen: HashSet<String> = HashSet::with_capacity(mounts.len());
        let original_len = mounts.len();
        // 保留**最后**出现的一项（2026-09-02）：容器创建的挂载顺序是
        // [模板声明…, GUI 注入…, 用户手动添加…]——用户手动项在最后。按
        // container_path 去重时若「用户手动覆盖模板同路径」应让用户赢。
        // 此前保留先出现的 → 模板/GUI 项静默压过用户手动项（手动挂载「没生效」）。
        // 反向迭代 + retain 改最后一条命中。
        mounts.reverse();
        mounts.retain(|m| {
            if m.container_path.is_empty() {
                tracing::warn!(
                    "挂载去重：container_path 为空，已丢弃（host_path={:?}）",
                    m.host_path
                );
                return false;
            }
            if seen.insert(m.container_path.clone()) {
                true
            } else {
                tracing::warn!(
                    "挂载去重：container_path={} 重复，跳过先前定义（host_path={:?}, read_only={}）——用户手动添加的（靠后）优先",
                    m.container_path, m.host_path, m.read_only
                );
                false
            }
        });
        mounts.reverse();
        let dropped = original_len - mounts.len();
        if dropped > 0 {
            tracing::debug!("挂载去重：丢弃 {dropped} 项（保留 {} 项）", mounts.len());
        }
    }

    /// 展开用户配置的挂载路径变量（`${HOME}`/`${USER}`/`${UID}`/`${GID}`）。
    ///
    /// 宿主侧/容器侧上下文相关：同一变量名在 `host_path` 取宿主值、在
    /// `container_path` 取容器值。无 `${}` 的挂载走快速路径（零探测零成本）。
    /// 容器侧 HOME/USER 且未配 user_name 时探测镜像 /etc/passwd 解析容器用户
    /// home/name（与容器内运行时 `$HOME` 精确一致）；探测失败退化为 uid 默认
    /// （告警，不阻断创建）。
    async fn expand_user_mounts(
        &self,
        image: &str,
        mounts: &[MountConfig],
        host: Option<&crate::userenv::HostUser>,
        uid: u32,
        gid: u32,
        user_name: Option<&str>,
    ) -> Result<Vec<MountConfig>> {
        use crate::pathvars::{
            container_path_vars, expand_mounts, host_path_vars, needs_image_passwd, PathVars,
        };

        // 快速路径：无任何 ${} → 原样返回（绝大多数容器，零成本）
        if !mounts
            .iter()
            .any(|m| m.host_path.contains("${") || m.container_path.contains("${"))
        {
            let mut out = mounts.to_vec();
            Self::dedup_mounts(&mut out);
            return Ok(out);
        }

        // 宿主侧：仅当 host_path 实际用到变量时才需要宿主用户（否则空占位，
        // expand 不会触碰宿主侧值）。
        let host_pv = if mounts.iter().any(|m| m.host_path.contains("${")) {
            host.map(host_path_vars).ok_or_else(|| {
                Error::Config(
                    "挂载 host_path 用了路径变量（${HOME}/${USER}/${UID}/${GID}），但宿主用户探测失败（host_user()=None），无法展开".to_string(),
                )
            })?
        } else {
            host.map(host_path_vars).unwrap_or_else(PathVars::empty)
        };

        // 容器侧：HOME/USER 且未配 user_name → 探测镜像 /etc/passwd（缓存）
        let image_passwd = if needs_image_passwd(mounts, user_name) {
            match self.image_passwd(image).await {
                Ok(pw) => Some(pw),
                Err(e) => {
                    tracing::warn!(
                        "探测镜像 {image} 的 /etc/passwd 失败，容器侧 ${{HOME}}/${{USER}} 退化为 uid 默认：{e}"
                    );
                    None
                }
            }
        } else {
            None
        };
        let container_pv = container_path_vars(uid, gid, user_name, image_passwd.as_deref());

        let mut out = expand_mounts(mounts, &host_pv, &container_pv)?;
        Self::dedup_mounts(&mut out);
        Ok(out)
    }

    /// 探测镜像的 /etc/passwd（建一次性普通容器 → 读 archive → 删除）。
    ///
    /// 用于挂载路径容器侧 `${HOME}`/`${USER}` 展开（未配 user_name 时）。
    /// 进程内按 image 名缓存（镜像 passwd 会话内视为不可变）。纯 bollard/libpod
    /// API，不依赖 podman CLI（铁律：零 podman CLI 调用）。
    async fn image_passwd(&self, image: &str) -> Result<String> {
        // 缓存命中 → 直接返回
        if let Some(pw) = crate::pathvars::cached_image_passwd(image) {
            return Ok(pw);
        }

        let probe_name = format!("easytidy-probe-{}", uuid::Uuid::new_v4().simple());
        let options = bollard::container::CreateContainerOptions {
            name: probe_name,
            platform: None,
        };
        let config = bollard::container::Config {
            image: Some(image.to_string()),
            ..Default::default()
        };

        let created = self
            .docker
            .create_container(Some(options), config)
            .await
            .map_err(|e| Error::Connect(format!("探测容器创建失败（{image}）：{e}")))?;

        // 读 /etc/passwd（archive）；无论读成功与否都清理探测容器
        let read = self.read_container_file(&created.id, "/etc/passwd").await;
        if let Err(e) = self.docker.remove_container(&created.id, None).await {
            tracing::warn!("探测容器清理失败（忽略）：{e}");
        }

        let passwd = read
            .map_err(|e| Error::Connect(format!("读取镜像 {image} 的 /etc/passwd 失败：{e}")))?;
        crate::pathvars::store_image_passwd(image, passwd.clone());
        Ok(passwd)
    }

    /// 从已创建（未启动）容器读取单个文件（archive GET → tar → 解出）。
    async fn read_container_file(&self, id: &str, path: &str) -> Result<String> {
        use bollard::container::DownloadFromContainerOptions;
        use futures::StreamExt;

        let options = DownloadFromContainerOptions {
            path: path.to_string(),
        };
        let mut stream = self.docker.download_from_container(id, Some(options));
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| Error::Connect(format!("读取 {path} 失败：{e}")))?;
            buf.extend_from_slice(&chunk);
        }
        parse_passwd_from_tar(&buf)
    }

    /// 环境快照:把运行中容器 commit 成镜像 `easytidy/snapshot/<snapshot_name>`
    /// （`snapshot_name` 即用户输入的全名，不再拼环境名前缀——命名即用户意图，
    /// 容器来源由 commit 决定；未提供时兜底为可读默认名 `<容器名>-<YYYYmmdd-HHMM>`）。
    ///
    /// 实现:走 libpod 直连(`libpod::commit`),与项目既有的 keep-id
    /// 创建路径走同一条 podman socket 直连栈。
    ///
    /// `squash` 决定镜像形态:
    /// - `true`（**默认**）= `commit --squash`:扁平文件系统镜像(单层),不保留容器
    ///   原本的分层历史。与多层相比体积更小、fork 出新容器时不再叠加源容器的
    ///   所有中间层,作为"环境快照"的默认语义。
    /// - `false` = 普通 commit:保留源容器的分层历史（体积 = 源镜像层 + 增量层）。
    ///
    /// 已知限制:
    /// - **运行中容器不保证快照一致性**(`commit` 配合 `pause=true` 默认,
    ///   与 `export` 不同——导出文件层时容器进程短暂暂停后继续运行,
    ///   暂停期间应用通常感知不到)。若对一致性敏感可先 `env_stop` 再快照。
    /// - **bind mount 不入快照**(podman 自身行为,与 commit 是否 squash 无关)。
    ///
    /// 快照是独立资产,删除环境不删快照(可被 fork 复用)。
    /// 见 docs/13-mutable-env-paradigm.md。
    pub async fn snapshot(
        &self,
        name: &str,
        snapshot_name: Option<&str>,
        squash: bool,
    ) -> Result<String> {
        // 未提供（或空白）→ 可读默认名 <容器名>-<YYYYmmdd-HHMM>（本地时间）
        let snapshot_name = match snapshot_name {
            Some(s) if !s.trim().is_empty() => s.trim().to_string(),
            _ => default_snapshot_name(name),
        };
        // 感知 `name:version`：用户在 CLI/GUI 输入的快照名可以带 `:` 表示
        // tag。libpod commit 的 `repo` 参数不允许含 `:`（会被解析成
        // `<repo>:<tag>:latest` 报 invalid reference format 500），必须把
        // repo 和 tag 分开传。
        let (snap_repo, snap_tag) = parse_snapshot_ref(&snapshot_name);
        let image_ref = match &snap_tag {
            Some(t) => format!("easytidy/snapshot/{snap_repo}:{t}"),
            None => format!("easytidy/snapshot/{snap_repo}"),
        };
        let libpod = crate::libpod::Libpod::new().await?;

        // 快照镜像默认清空容器继承的 labels（devcontainer.* / io.buildah.* /
        // manager 等）。快照是独立资产，不直接被 `create_with_config` 用作
        // 新容器 base——下次 create 时再注入 easytidy 自己的 labels，所以这里
        // 清空所有 label 不影响后续识别逻辑。podman 5.4.2 无 `--unsetlabel`，
        // 唯一可控路径是 commit 时传 `changes=LABEL=foo=`（空值覆盖）。
        let changes = self.build_label_clear_changes(name).await;
        tracing::info!(
            "环境 {} 快照:清空 {} 个 labels 后 commit → {}",
            name,
            changes.len(),
            image_ref
        );

        let change_refs: Vec<&str> = changes.iter().map(|s| s.as_str()).collect();
        // OCI history 备注按形态区分，便于 podman inspect 溯源
        let message = if squash {
            "easytidy snapshot via commit --squash"
        } else {
            "easytidy snapshot via commit"
        };
        libpod
            .commit(name, &snap_repo, snap_tag.as_deref(), squash, message, &change_refs)
            .await?;
        tracing::info!("环境 {} 快照完成（squash={}）:{}", name, squash, image_ref);
        Ok(image_ref)
    }

    /// 读取容器 labels，对每个 key 生成 `LABEL=foo=`（空值覆盖）changes。
    ///
    /// podman 5.4.2 commit 不支持真正删除 label——commit 时传 `LABEL=foo=`
    /// 把 image 的 label value 改成空串，敏感内容（路径 / env / 端口）消失。
    /// key 仍存在但 value 清空（podman 设计上不允许从 image 删 label key）。
    async fn build_label_clear_changes(&self, name: &str) -> Vec<String> {
        let labels = match self.docker.inspect_container(name, None).await {
            Ok(detail) => detail
                .config
                .and_then(|c| c.labels)
                .unwrap_or_default(),
            Err(e) => {
                tracing::warn!(
                    "快照时读取容器 labels 失败（labels 会原样保留在快照镜像）：{e}"
                );
                return Vec::new();
            }
        };
        labels
            .into_keys()
            .map(|k| format!("LABEL={k}="))
            .collect()
    }

    /// 重建容器（应用配置变更：mounts / 网络映射，创建后不可变 → 必须重建）。
    ///
    /// 「新容器确认就绪后才删旧」的安全流程（旧容器全程保留到确认，失败自动回滚）：
    /// 1. `commit_container` 当前容器层为镜像 `localhost/easytidy-rebuild:<tag>`
    ///    （仅容器层；bind mount 不入 commit —— 正是所需）——**数据保险**
    /// 2. `rename` 旧容器为临时名 `<name>-old-<tag>`（**保留**！释放正式名，
    ///    旧容器数据仍在容器层 + 镜像双重保存）
    /// 3. `stop` 旧（释放 socket——旧 dock daemon 退出，`dock.sock` 让出给新容器；
    ///    这是重建必然的短暂中断）
    /// 4. `create_with_config`（正式名 `<name>`，commit 的镜像 + 新配置，label/
    ///    socket/server 注册全按 `<name>` 绑定）
    /// 5. `start_and_confirm`（start + 确认容器 running 且 dock daemon 就绪）
    /// 6. **确认就绪** → `remove` 旧（此时安全：新容器已起、数据已在新容器）
    /// 7. 返回新容器 ID
    ///
    /// 错误处理：第 4 / 5 步任一失败 → **回滚**（删未就绪的新容器，把旧容器
    /// rename 回正式名并重新 start，环境恢复运行）；数据始终有 commit 镜像兜底。
    /// 调用方负责在成功后把 `config` 回写 configfile（GUI apply / CLI rebuild 均执行）。
    pub async fn rebuild(
        &self,
        name: &str,
        config: &ContainerConfig,
        bins: &crate::ContainerBins,
    ) -> Result<String> {
        // 先记下现有 socket 目录（重建后 socket 目录 = easytidy/<name>；成功后清理
        // 旧代孤儿 `<name>-<hash>`。纯 name 目录即当前代——按新目录做白名单，见第 7 步）
        let legacy_socket_dirs = crate::resolve_socket_dirs(name);

        // 1. commit 当前容器层（bind mount 不入镜像）→ 数据保险
        let tag = Self::rebuild_image_tag(name);
        let image_ref = format!("localhost/easytidy-rebuild:{tag}");
        self.commit_container(name, &image_ref).await?;

        // 2. rename 旧容器为临时名（保留！释放 <name> 的名字）
        let old_name = format!("{name}-old-{tag}");
        self.rename(name, &old_name).await?;

        // 3. stop 旧（释放 socket——旧 dock daemon 退出，dock.sock 让出给新容器）
        if self.is_running(&old_name).await? {
            self.stop(&old_name).await?;
        }

        // 4. create 新容器（正式名 <name>；失败 → 回滚，旧容器数据未损）
        let id = match self.create_with_config(name, &image_ref, bins, config).await {
            Ok(id) => id,
            Err(e) => {
                self.rollback(&old_name, name).await;
                return Err(Error::Connect(format!(
                    "重建失败：创建新容器未成功（已回滚，旧容器已恢复运行）：{e}"
                )));
            }
        };

        // 5. start + 确认新容器真正就绪（running + dock daemon）；失败 → 回滚
        if let Err(e) = self.start_and_confirm(name).await {
            let _ = self.remove(name, true).await; // 清理未就绪的新容器
            self.rollback(&old_name, name).await; // 恢复旧容器运行
            return Err(Error::Connect(format!(
                "重建失败：新容器未能确认就绪（已回滚，旧容器已恢复运行）：{e}"
            )));
        }

        // 6. 新容器确认就绪 → 删旧（安全：新已起、数据已在新容器）
        if let Err(e) = self.remove(&old_name, true).await {
            tracing::warn!("重建后清理旧容器 {old_name} 失败（不影响新容器运行）：{e}");
        }

        // 7. 清理旧代 socket 目录（新代 = easytidy/<name>，由 create 建；旧
        //    `<name>-<hash>` 代目录清理，纯 name 目录即当前代 → 跳过，避免误删
        //    正在使用的目录）
        let new_socket_dir = crate::socket_dir_for(name)?;
        for legacy in legacy_socket_dirs {
            if legacy != new_socket_dir {
                tracing::debug!("重建后清理旧代 socket 目录：{}", legacy.display());
                let _ = std::fs::remove_dir_all(&legacy);
            }
        }

        Ok(id)
    }

    /// start 容器并确认「真正起来了」：容器进入 running 且 dock daemon 就绪。
    ///
    /// - 容器 running：轮询 `is_running`（`start` 返回 Ok 后容器可能需数秒进入 running）
    /// - dock daemon 就绪：轮询 `dock_alive`（bootstrap/prepare 有延迟）
    ///
    /// 两级都通过才返回 Ok——用于重建时「确认新容器真的可用」后再删旧容器。
    async fn start_and_confirm(&self, name: &str) -> Result<()> {
        self.start(name).await?;

        // 等容器进入 running（最多 ~15s）
        let mut running = false;
        for _ in 0..30 {
            if self.is_running(name).await? {
                running = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        if !running {
            return Err(Error::Connect(format!(
                "容器 {name} 启动后未进入 running 状态"
            )));
        }

        // 等 dock daemon 就绪（最多 ~15s；bootstrap/prepare 有延迟）
        let mut alive = false;
        for _ in 0..30 {
            if self.dock_alive(name).await {
                alive = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        if !alive {
            return Err(Error::Connect(format!(
                "容器 {name} 已 running 但 dock daemon 未就绪（easytidy-dock client ping 无响应）"
            )));
        }
        Ok(())
    }

    /// 回滚重建：把临时名的旧容器 rename 回正式名并重新 start（尽力而为）。
    ///
    /// 任一步失败仅 warn（回滚是尽力恢复，不阻断错误上报）；数据始终有 commit
    /// 镜像兜底。
    async fn rollback(&self, old_name: &str, name: &str) {
        if let Err(e) = self.rename(old_name, name).await {
            tracing::warn!("回滚 rename（{old_name} → {name}）失败：{e}");
            return;
        }
        match self.is_running(name).await {
            Ok(true) => {} // 已在运行（防御；正常 stop 后不会）
            Ok(false) | Err(_) => {
                if let Err(e) = self.start(name).await {
                    tracing::warn!("回滚 start（{name}）失败：{e}");
                }
            }
        }
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

    /// 重命名容器（重建时释放正式名 / 回滚时换回）。
    async fn rename(&self, old: &str, new: &str) -> Result<()> {
        use bollard::container::RenameContainerOptions;

        self.docker
            .rename_container(old, RenameContainerOptions { name: new.to_string() })
            .await
            .map_err(|e| Error::Connect(format!("重命名容器失败（{old} → {new}）：{e}")))?;
        Ok(())
    }

    /// dock daemon 存活确认（容器内 `easytidy-dock client ping`，fire-and-forget）。
    ///
    /// 容器 running 但 dock daemon 尚未就绪时返回 false（bootstrap/prepare 有延迟，
    /// 调用方应轮询重试）。exec 不通 / 无响应 / 输出非 `alive: true` 均返回 false。
    async fn dock_alive(&self, container: &str) -> bool {
        use futures::StreamExt;

        let cmd = vec![
            Self::DOCK_TARGET.to_string(),
            "client".to_string(),
            "ping".to_string(),
        ];
        let exec = match self.exec_no_tty(container, "0", cmd).await {
            Ok(e) => e,
            Err(_) => return false,
        };
        let mut stream = exec.output;
        let mut buf = String::new();
        while let Some(item) = stream.next().await {
            if let Ok(bytes) = item {
                buf.push_str(&String::from_utf8_lossy(&bytes));
            }
        }
        buf.contains("alive: true")
    }

    /// 容器详细状态（启动失败展示用）：`running` / `status` / `exit_code` / `error`。
    ///
    /// 取 `podman inspect` 的 `State` 字段（Docker compat API 标准字段，对应
    /// `docker inspect` 的 `State.Status/ExitCode/Error`）。容器不存在 → None。
    pub async fn container_state(
        &self,
        name: &str,
    ) -> Result<Option<crate::models::ContainerStateView>> {
        let info = match self.docker.inspect_container(name, None).await {
            Ok(i) => i,
            Err(e) => {
                // bollard 对不存在的容器返回 404；按"不存在"返回 None 而非错误
                let msg = e.to_string();
                if msg.contains("404") || msg.contains("No such") || msg.contains("not found") {
                    return Ok(None);
                }
                return Err(Error::Connect(format!("检查容器状态失败：{e}")));
            }
        };
        let Some(state) = info.state else {
            return Ok(None);
        };
        // bollard 的 ContainerStateStatusEnum 是封闭枚举，通过 Debug 取大写变体名
        // （如 "Running" / "Exited" / "Created"）→ 转小写得到 podman 原生状态字符串。
        // 比硬编码 match 列表更鲁棒（bollard 新增变体时自动兼容）。
        let status_str = state
            .status
            .map(|s| format!("{:?}", s).to_lowercase())
            .unwrap_or_else(|| "unknown".into());

        Ok(Some(crate::models::ContainerStateView {
            running: state.running.unwrap_or(false),
            status: status_str,
            exit_code: state.exit_code,
            error: state.error.filter(|s| !s.is_empty()),
        }))
    }

    /// 容器日志（podman logs 等价）：合并 stdout + stderr，按 `tail_lines` 取尾。
    ///
    /// `tail_lines = 0` → bollard 传 "all"（全部历史日志）。容器不存在 / 已删除
    /// → 返回 Err（调用方按"不存在"处理）。`tail_lines` 上限 10000 行避免单次返回过大。
    pub async fn container_logs(&self, name: &str, tail_lines: usize) -> Result<String> {
        use bollard::container::{LogOutput, LogsOptions};
        use futures::StreamExt;

        let tail = if tail_lines == 0 {
            "all".to_string()
        } else {
            tail_lines.min(10000).to_string()
        };
        let opts = Some(LogsOptions {
            stdout: true,
            stderr: true,
            follow: false,
            since: 0,
            until: 0,
            timestamps: false,
            tail,
        });

        let mut stream = self.docker.logs(name, opts);
        let mut output = String::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(LogOutput::StdOut { message })
                | Ok(LogOutput::StdErr { message })
                | Ok(LogOutput::Console { message })
                | Ok(LogOutput::StdIn { message }) => {
                    output.push_str(&String::from_utf8_lossy(&message));
                }
                Err(e) => {
                    return Err(Error::Connect(format!("读取容器日志失败：{e}")));
                }
            }
        }
        Ok(output)
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

/// 默认快照名：`<容器名>-<YYYYmmdd-HHMM>`（本地时间）。
///
/// 用户未输入快照名时的兜底——可读、能看出来源容器（纯 Unix 时间戳难读
/// 且无法区分容器）。
fn default_snapshot_name(container: &str) -> String {
    use chrono::Local;
    format!("{container}-{}", Local::now().format("%Y%m%d-%H%M"))
}

/// 从 archive tar（docker/podman `download_from_container` 返回）解出 passwd 文本。
///
/// 条目名可能是 `passwd` 或 `etc/passwd`（按 Docker API 行为），都识别。
fn parse_passwd_from_tar(buf: &[u8]) -> Result<String> {
    use std::io::Read;

    let mut archive = tar::Archive::new(buf);
    for entry in archive
        .entries()
        .map_err(|e| Error::Config(format!("解析 passwd tar 失败：{e}")))?
    {
        let mut entry = entry.map_err(|e| Error::Config(format!("解析 passwd tar 条目失败：{e}")))?;
        let name = entry
            .path()
            .map_err(|e| Error::Config(format!("tar 条目路径非法：{e}")))?
            .to_string_lossy()
            .to_string();
        if name == "passwd" || name == "etc/passwd" || name.ends_with("/passwd") {
            let mut content = Vec::new();
            entry
                .read_to_end(&mut content)
                .map_err(|e| Error::Config(format!("读 passwd 内容失败：{e}")))?;
            return String::from_utf8(content).map_err(|e| Error::Config(format!("passwd 非 UTF-8：{e}")));
        }
    }
    Err(Error::Config(
        "镜像 /etc/passwd tar 中未找到 passwd 文件".to_string(),
    ))
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

/// 解析快照名为 `(repo, tag)`：第一个 `:` 切分，余下 `:` 视为 name/tag 一部分（podman 切最后 `:`）。
/// `name` 仅允许 `[A-Za-z0-9_.-]+`（同 podman repo 命名规则）；含非法字符直接走原样透传（podman 自己报具体错误）。
/// 返回的 `tag=None` 表示无 tag（podman 走默认 `:latest`）。
fn parse_snapshot_ref(input: &str) -> (String, Option<String>) {
    // 第一个 `:` 切（避免被 tag 内部的 `:` 干扰：name 不能再含 `:`）
    match input.split_once(':') {
        Some((name, tag)) => {
            if name.is_empty() {
                return (input.to_string(), None);
            }
            if !is_valid_image_name_component(name) {
                return (input.to_string(), None);
            }
            if tag.is_empty() || !is_valid_image_name_component(tag) {
                // 空 tag 或非法 tag：把 name 部分当纯名透传（不保留 `:` 痕迹）
                return (name.to_string(), None);
            }
            (name.to_string(), Some(tag.to_string()))
        }
        None => {
            if is_valid_image_name_component(input) {
                (input.to_string(), None)
            } else {
                // 含非法字符的纯 name 也直接透传（podman 会自己报错）
                (input.to_string(), None)
            }
        }
    }
}

/// podman image name 合法字符：`[a-zA-Z0-9_.-]`（路径分隔符 `/` 由调用方处理）
fn is_valid_image_name_component(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
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
    fn test_default_snapshot_name_shape() {
        // <容器名>-<YYYYmmdd-HHMM>：容器名前缀 + 13 位数字时间（4 连 2 连 2 连 2 连 4）
        let name = default_snapshot_name("chrome");
        let rest = name.strip_prefix("chrome-").expect("应以容器名开头");
        assert_eq!(rest.len(), 13, "时间部分应为 YYYYmmdd-HHMM：{name}");
        let (date, time) = rest.split_once('-').expect("日期与时间以 - 分隔");
        assert_eq!(date.len(), 8);
        assert_eq!(time.len(), 4);
        assert!(date.bytes().all(|b| b.is_ascii_digit()));
        assert!(time.bytes().all(|b| b.is_ascii_digit()));
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
            false,
            Vec::new(),
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
            false,
            Vec::new(),
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
            false,
            Vec::new(),
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
        let make = |devices: Vec<String>,
                    gpu_nvidia: bool,
                    amd_devices: Vec<String>,
                    pid: Option<&str>,
                    security: Vec<String>| {
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
                gpu_nvidia,
                amd_devices,
                pid,
                security,
            )
        };

        let to_vec = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        // 全部为空（旧行为）：不出现 devices/pidns/security 字段，init 保持 true
        let body = make(Vec::new(), false, Vec::new(), None, Vec::new());
        assert!(body.get("devices").is_none());
        assert!(body.get("pidns").is_none());
        assert!(body.get("apparmor_profile").is_none());
        assert!(body.get("selinux_opts").is_none());
        assert!(body.get("seccomp_profile_path").is_none());
        assert_eq!(body["init"], true);
        // entrypoint 始终置空（清镜像 ENTRYPOINT；command 即 PID 1 字面命令）
        assert_eq!(body["entrypoint"], serde_json::json!([]));

        // gpu_nvidia=true → nvidia.com/gpu=all CDI 引用
        let body = make(Vec::new(), true, Vec::new(), None, Vec::new());
        assert_eq!(body["devices"][0]["path"], "nvidia.com/gpu=all");

        // 裸设备直通
        let body = make(to_vec(&["/dev/uinput:/dev/uinput"]), false, Vec::new(), None, Vec::new());
        assert_eq!(body["devices"][0]["path"], "/dev/uinput:/dev/uinput");

        // gpu_nvidia + 裸设备共存
        let body = make(
            to_vec(&["/dev/uinput:/dev/uinput"]),
            true,
            Vec::new(),
            None,
            Vec::new(),
        );
        assert_eq!(body["devices"][0]["path"], "/dev/uinput:/dev/uinput");
        assert_eq!(body["devices"][1]["path"], "nvidia.com/gpu=all");

        // gpu_amd=true（探测出的裸设备）→ 原样落 devices，非 amd.com/gpu CDI
        let body = make(
            Vec::new(),
            false,
            to_vec(&["/dev/dri/renderD129:/dev/dri/renderD129"]),
            None,
            Vec::new(),
        );
        assert_eq!(body["devices"][0]["path"], "/dev/dri/renderD129:/dev/dri/renderD129");

        // 双开 → 裸设备(uinput) + nvidia CDI + amd 裸设备按序落
        let body = make(
            to_vec(&["/dev/uinput:/dev/uinput"]),
            true,
            to_vec(&["/dev/dri/renderD129:/dev/dri/renderD129"]),
            None,
            Vec::new(),
        );
        assert_eq!(body["devices"][0]["path"], "/dev/uinput:/dev/uinput");
        assert_eq!(body["devices"][1]["path"], "/dev/dri/renderD129:/dev/dri/renderD129");
        assert_eq!(body["devices"][2]["path"], "nvidia.com/gpu=all");

        // AMD 裸设备与用户手动设备串相同 → 去重只落一次
        let body = make(
            to_vec(&["/dev/dri/renderD129:/dev/dri/renderD129"]),
            false,
            to_vec(&["/dev/dri/renderD129:/dev/dri/renderD129"]),
            None,
            Vec::new(),
        );
        assert_eq!(body["devices"].as_array().unwrap().len(), 1);
        assert_eq!(body["devices"][0]["path"], "/dev/dri/renderD129:/dev/dri/renderD129");

        // pid = host → pidns.nsmode = host 且 init 禁用（catatonit 无法进 host PID ns）
        let body = make(Vec::new(), false, Vec::new(), Some("host"), Vec::new());
        assert_eq!(body["pidns"]["nsmode"], "host");
        assert_eq!(body["init"], false);

        // pid = private（显式）→ 无 pidns 字段，init 保持
        let body = make(Vec::new(), false, Vec::new(), Some("private"), Vec::new());
        assert!(body.get("pidns").is_none());
        assert_eq!(body["init"], true);

        // security_opts 解析：label → selinux_opts，apparmor → apparmor_profile，
        // seccomp → seccomp_profile_path（与 podman CLI --security-opt 映射一致）
        let body = make(
            Vec::new(),
            false,
            Vec::new(),
            None,
            to_vec(&["label=disable", "apparmor=unconfined", "seccomp=unconfined"]),
        );
        assert_eq!(body["selinux_opts"][0], "disable");
        assert_eq!(body["apparmor_profile"], "unconfined");
        assert_eq!(body["seccomp_profile_path"], "unconfined");

        // 未知 key 忽略，无 '=' 的串忽略（不 panic、不产生字段）
        let body = make(Vec::new(), false, Vec::new(), None, to_vec(&["mask=/foo", "nonsense"]));
        assert!(body.get("apparmor_profile").is_none());
        assert!(body.get("selinux_opts").is_none());
    }

    /// 真机端到端：探测镜像 /etc/passwd → 容器侧 `${HOME}` 展开为镜像默认用户
    /// home（chrome 场景：ubuntu 镜像 uid 1000 → `/home/ubuntu`），宿主侧 `${HOME}`
    /// 展开为宿主 home。需 rootless podman socket + ubuntu 镜像，缺失则跳过。
    #[tokio::test]
    async fn test_container_home_expansion_via_image_probe() {
        let socket = match std::env::var("XDG_RUNTIME_DIR") {
            Ok(rd) => std::path::PathBuf::from(rd).join("podman/podman.sock"),
            Err(_) => return,
        };
        if !socket.exists() {
            eprintln!("skip: podman socket 不存在");
            return;
        }
        let Ok(podman) = Podman::connect().await else {
            eprintln!("skip: 无法连接 podman");
            return;
        };
        let image = "docker.io/library/ubuntu:24.04";
        if !podman.image_exists(image).await.unwrap_or(false) {
            eprintln!("skip: {image} 镜像不存在");
            return;
        }

        // 探测镜像 passwd（ubuntu 镜像 uid 1000 home = /home/ubuntu）
        let passwd = podman
            .image_passwd(image)
            .await
            .expect("探测镜像 /etc/passwd 应成功");
        assert!(
            passwd.contains("ubuntu:x:1000:1000:Ubuntu:/home/ubuntu"),
            "ubuntu 镜像 uid 1000 home 应为 /home/ubuntu：{passwd}"
        );

        let host = HostUser {
            name: "div".into(),
            uid: 1000,
            gid: 1000,
            home: "/home/div".into(),
        };

        // 容器侧 ${HOME}（chrome 场景：无 user_name，uid 1000）→ /home/ubuntu
        let mounts = vec![MountConfig {
            host_path: "/tmp/.X11-unix".into(),
            container_path: "${HOME}/workspace".into(),
            read_only: false,
        }];
        let out = podman
            .expand_user_mounts(image, &mounts, Some(&host), 1000, 1000, None)
            .await
            .expect("展开应成功");
        assert_eq!(out[0].container_path, "/home/ubuntu/workspace");
        assert_eq!(out[0].host_path, "/tmp/.X11-unix");

        // 宿主侧 ${HOME} → 宿主 home（容器侧无变量 → 不触发探测）
        let mounts2 = vec![MountConfig {
            host_path: "${HOME}/workspace".into(),
            container_path: "/workspace".into(),
            read_only: false,
        }];
        let out2 = podman
            .expand_user_mounts(image, &mounts2, Some(&host), 1000, 1000, None)
            .await
            .expect("展开应成功");
        assert_eq!(out2[0].host_path, "/home/div/workspace");
        assert_eq!(out2[0].container_path, "/workspace");
    }

    #[test]
    fn test_dedup_mounts_keeps_last() {
        // 同 container_path 重复 → 保留**最后**一条（用户手动添加的靠后，应赢过
        // 模板/GUI 注入的先前定义），先出现的被丢弃（warn 日志）
        let mut mounts = vec![
            MountConfig {
                host_path: "/data/a".into(),
                container_path: "/home/ubuntu/foo".into(),
                read_only: false,
            },
            MountConfig {
                host_path: "/data/b".into(),
                container_path: "/home/ubuntu/foo".into(),
                read_only: true,
            },
            MountConfig {
                host_path: "/data/c".into(),
                container_path: "/home/ubuntu/bar".into(),
                read_only: false,
            },
        ];
        Podman::dedup_mounts(&mut mounts);
        assert_eq!(mounts.len(), 2, "重复 container_path 应被丢弃");
        assert_eq!(mounts[0].host_path, "/data/b", "保留最后出现的（用户手动项优先）");
        assert_eq!(mounts[1].host_path, "/data/c");
    }

    #[test]
    fn test_dedup_mounts_drops_empty_container_path() {
        let mut mounts = vec![
            MountConfig {
                host_path: "/data/a".into(),
                container_path: "".into(),
                read_only: false,
            },
            MountConfig {
                host_path: "/data/b".into(),
                container_path: "/valid".into(),
                read_only: false,
            },
        ];
        Podman::dedup_mounts(&mut mounts);
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].container_path, "/valid");
    }

    #[test]
    fn test_dedup_mounts_all_unique() {
        let mut mounts = vec![
            MountConfig {
                host_path: "/data/a".into(),
                container_path: "/foo".into(),
                read_only: false,
            },
            MountConfig {
                host_path: "/data/b".into(),
                container_path: "/bar".into(),
                read_only: false,
            },
        ];
        Podman::dedup_mounts(&mut mounts);
        assert_eq!(mounts.len(), 2, "无重复时不动");
    }

    #[test]
    fn test_dedup_mounts_fast_path_dedup() {
        // fast-path（无 ${}）也走 dedup_mounts：直接验证 dedup_mounts 对
        // 无变量 mount 同样生效（expand_user_mounts 的 fast-path 返回前调用）。
        // 这里不调 expand_user_mounts（需 Podman 实例），仅覆盖 dedup 本身。
        let mut mounts = vec![
            MountConfig {
                host_path: "/data/a".into(),
                container_path: "/x".into(),
                read_only: false,
            },
            MountConfig {
                host_path: "/data/b".into(),
                container_path: "/x".into(),
                read_only: true,
            },
        ];
        Podman::dedup_mounts(&mut mounts);
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].host_path, "/data/b", "保留最后出现的（用户手动项优先）");
    }

    /// snapshot_name 解析（name:version → (name, Some(version))）：
    /// libpod commit 的 `repo` 不允许含 `:`，必须把 name / version 拆开分别传。
    #[test]
    fn test_parse_snapshot_ref() {
        // 纯 name：tag=None
        assert_eq!(super::parse_snapshot_ref("myimage"), ("myimage".into(), None));
        assert_eq!(
            super::parse_snapshot_ref("desk_pilot.v9"),
            ("desk_pilot.v9".into(), None)
        );

        // name:version：第一个 `:` 切
        assert_eq!(
            super::parse_snapshot_ref("myimage:v1"),
            ("myimage".into(), Some("v1".into()))
        );
        assert_eq!(
            super::parse_snapshot_ref("desk_pilot:9.3.2"),
            ("desk_pilot".into(), Some("9.3.2".into()))
        );

        // 非法字符（带 `/` / `:` 后跟非合法 tag）：fallback 整体当 name，tag=None
        // （podman 自己会报具体错误）
        assert_eq!(
            super::parse_snapshot_ref("foo/bar"),
            ("foo/bar".into(), None)
        );
        assert_eq!(super::parse_snapshot_ref("foo:"), ("foo".into(), None));

        // 空字符串：原样返回（让 podman 报错）
        assert_eq!(super::parse_snapshot_ref(""), ("".into(), None));
    }
}
