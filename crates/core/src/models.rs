//! 数据模型（GUI / CLI / engine 共用）。

use serde::{Deserialize, Serialize};

/// easytidy 管理的容器概要（从 podman inspect/ps 投影）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerSummary {
    /// 容器名
    pub name: String,
    /// 容器 ID（短）
    pub id: String,
    /// 镜像
    pub image: String,
    /// 运行状态（"running" / "created" / "exited" / ...）
    pub status: String,
    /// 是否为 easytidy 管理（按标签判定）
    pub managed: bool,
}

/// 镜像摘要（GUI 镜像管理）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageSummary {
    /// 镜像 ID（短）
    pub id: String,
    /// 仓库标签（如 ["docker.io/library/ubuntu:24.04"]；悬空镜像为空）
    pub repo_tags: Vec<String>,
    /// 展开后大小（字节）
    pub size: u64,
    /// 创建时间（unix 秒）
    pub created: i64,
}

/// 路径映射（bind mount）。
///
/// 宿主路径必须已存在（`create_with_config` / `rebuild` 时校验）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountConfig {
    /// 宿主路径
    pub host_path: String,
    /// 容器内目标路径
    pub container_path: String,
    /// 是否只读
    pub read_only: bool,
}

/// 端口映射。
///
/// `protocol` 默认 "tcp"（也支持 "udp" / "sctp"）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortMapping {
    /// 宿主端口
    pub host_port: u16,
    /// 容器内端口
    pub container_port: u16,
    /// 协议（"tcp" 默认）
    pub protocol: String,
}

impl Default for PortMapping {
    fn default() -> Self {
        Self {
            host_port: 0,
            container_port: 0,
            protocol: "tcp".to_string(),
        }
    }
}

/// 网络模式。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum NetworkMode {
    /// host 网络（容器直接使用宿主网络栈；distrobox 默认语义，见 docs/08 风险 5）
    #[default]
    #[serde(rename = "host")]
    Host,
    /// 默认 bridge 网络 + 端口映射
    #[serde(rename = "mapped")]
    Mapped,
}

/// 网络配置。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkConfig {
    /// 网络模式（默认 Host）
    #[serde(default)]
    pub mode: NetworkMode,
    /// 端口映射（仅 `Mapped` 模式生效；host 模式下忽略）
    #[serde(default)]
    pub ports: Vec<PortMapping>,
}

/// `ContainerConfig.network` 的 serde 默认值（旧 config.toml 兼容）。
fn default_network() -> NetworkConfig {
    NetworkConfig::default()
}

/// `ContainerConfig.user_home` 的 serde 默认值（旧配置文件无该字段 → 默认开启映射）。
fn default_true() -> bool {
    true
}

/// 容器配置（easytidy 自有元数据，存于宿主共享配置文件）。
///
/// `mounts` / `network` / `user_home` 为 serde 默认值：旧配置文件（无新字段）加载后
/// 得到空挂载列表 + Host 网络模式 + 用户一致性映射开启。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerConfig {
    pub name: String,
    pub image: String,
    /// entry 应用（静默启动时链式拉起；类 Docker ENTRYPOINT）
    pub entry: Option<String>,
    /// 静默启动标志（宿主开机自启）
    pub silent_boot: bool,
    /// 是否常驻（catatonit + server 生命周期）
    pub persistent: bool,
    /// 路径映射（bind mount）
    #[serde(default)]
    pub mounts: Vec<MountConfig>,
    /// 网络配置（默认 Host 模式）
    #[serde(default = "default_network")]
    pub network: NetworkConfig,
    /// 容器环境变量（"KEY=VALUE" 列表，GUI 透传时含宿主 DISPLAY/WAYLAND_DISPLAY/XAUTHORITY）
    #[serde(default)]
    pub env: Vec<String>,
    /// 用户一致性映射（distrobox 式）：默认映射宿主用户目录（`$HOME` → `$HOME` rw）+
    /// 经 `EASYTIDY_USER_*` 告知容器内 server 创建同名/同 uid/gid 用户，
    /// 应用以该用户运行而非 root——避免容器内 root 读写宿主挂载目录的权限问题。
    ///
    /// 与 GUI 透传的关系：`gui=true` 的 flavor 强制开启（GUI 应用写宿主挂载目录、
    /// 读宿主字体/图标都需要正确属主）；非 GUI 容器可显式 `user_home = false` 关闭
    /// （容器内以 root 运行，行为与旧版一致）。
    #[serde(default = "default_true")]
    pub user_home: bool,
}

impl Default for ContainerConfig {
    /// 与 serde 默认保持一致：`user_home` 默认 true（用户一致性映射开启）。
    fn default() -> Self {
        Self {
            name: String::new(),
            image: String::new(),
            entry: None,
            silent_boot: false,
            persistent: false,
            mounts: Vec::new(),
            network: NetworkConfig::default(),
            env: Vec::new(),
            user_home: true,
        }
    }
}

/// 容器配置视图（`Podman::inspect_config` 投影：当前生效的 mounts / 网络 / env）。
///
/// 与 [`ContainerConfig`] 同形状（不含 entry/silent_boot/persistent——这些无
/// "生效"概念），但来自 podman inspect 的实际状态，供 GUI "当前生效" 面板与
/// configfile 期望配置对比。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerConfigView {
    /// 当前生效的 bind mounts（含 engine 内部挂载：server 二进制 + socket 目录）
    pub mounts: Vec<MountConfig>,
    /// 当前生效的网络配置
    pub network: NetworkConfig,
    /// 当前生效的环境变量（含系统注入 `EASYTIDY_USER_*` 与 podman 自动
    /// 补的 PATH/HOSTNAME/TERM/HOME——GUI 对比时需过滤后子集比较）
    #[serde(default)]
    pub env: Vec<String>,
    /// 容器进程用户（inspect `Config.User`；keep-id 下为 "0:0"，未设则 None）
    #[serde(default)]
    pub user: Option<String>,
    /// userns 模式（inspect `HostConfig.UsernsMode`；keep-id 容器实际回显
    /// 可能为 "private" 或 None——语义以 `user_home` 配置 + docs/12 为准）
    #[serde(default)]
    pub userns_mode: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_old_format_config_defaults() {
        // 旧配置文件（无 mounts/network 字段）→ 空挂载 + Host 网络
        let toml_str = r#"
name = "legacy"
image = "alpine:latest"
entry = "/bin/sh"
silent_boot = false
persistent = true
"#;
        let config: ContainerConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.name, "legacy");
        assert!(config.mounts.is_empty());
        assert_eq!(config.network.mode, NetworkMode::Host);
        assert!(config.network.ports.is_empty());
        assert!(config.env.is_empty());
        assert!(config.user_home);
    }

    #[test]
    fn test_config_view_new_fields_defaults() {
        // 旧 JSON（无 env/user/userns_mode 字段）→ 空 env + None
        let view: ContainerConfigView =
            serde_json::from_str(r#"{"mounts":[],"network":{"mode":"host","ports":[]}}"#).unwrap();
        assert!(view.env.is_empty());
        assert_eq!(view.user, None);
        assert_eq!(view.userns_mode, None);
    }

    #[test]
    fn test_network_mode_json_serde() {
        // 反序列化："host" / "mapped" 字符串
        let net: NetworkConfig = serde_json::from_str(r#"{"mode":"host","ports":[]}"#).unwrap();
        assert_eq!(net.mode, NetworkMode::Host);

        let net: NetworkConfig = serde_json::from_str(
            r#"{"mode":"mapped","ports":[{"host_port":8080,"container_port":80,"protocol":"tcp"}]}"#,
        )
        .unwrap();
        assert_eq!(net.mode, NetworkMode::Mapped);
        assert_eq!(net.ports[0].host_port, 8080);
        assert_eq!(net.ports[0].container_port, 80);
        assert_eq!(net.ports[0].protocol, "tcp");

        // 序列化：仍是 "host" / "mapped" 字符串（前端契约）
        let v = serde_json::to_value(NetworkConfig {
            mode: NetworkMode::Mapped,
            ports: Vec::new(),
        })
        .unwrap();
        assert_eq!(v["mode"], "mapped");
        let v = serde_json::to_value(NetworkMode::Host).unwrap();
        assert_eq!(v, "host");
    }

    #[test]
    fn test_user_home_serde_default_true() {
        // 旧配置文件（无 user_home 字段）→ 默认 true（用户一致性映射开启）
        let toml_str = r#"
name = "legacy"
image = "alpine:latest"
silent_boot = false
persistent = true
"#;
        let config: ContainerConfig = toml::from_str(toml_str).unwrap();
        assert!(config.user_home, "旧配置缺失 user_home 字段应默认 true");

        // 显式 false → 关闭映射（非 GUI 容器可选项）
        let toml_str = r#"
name = "no-home"
image = "alpine:latest"
silent_boot = false
persistent = true
user_home = false
"#;
        let config: ContainerConfig = toml::from_str(toml_str).unwrap();
        assert!(!config.user_home);

        // Rust 侧 Default 与 serde 默认一致（struct literal 走 ..Default::default() 的入口同语义）
        assert!(ContainerConfig::default().user_home);
    }

    #[test]
    fn test_container_config_roundtrip() {
        let config = ContainerConfig {
            name: "app".to_string(),
            image: "alpine:latest".to_string(),
            entry: Some("/bin/sh".to_string()),
            silent_boot: true,
            persistent: true,
            mounts: vec![MountConfig {
                host_path: "/tmp".to_string(),
                container_path: "/data".to_string(),
                read_only: true,
            }],
            network: NetworkConfig {
                mode: NetworkMode::Mapped,
                ports: vec![PortMapping {
                    host_port: 8080,
                    container_port: 80,
                    protocol: "tcp".to_string(),
                }],
            },
            env: vec!["DISPLAY=:0".to_string()],
            user_home: true,
        };

        // TOML 往返（configfile 格式）
        let toml_str = toml::to_string(&config).unwrap();
        let back: ContainerConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(back.mounts, config.mounts);
        assert_eq!(back.network, config.network);

        // JSON 往返（Tauri 命令格式）
        let json = serde_json::to_value(&config).unwrap();
        let back: ContainerConfig = serde_json::from_value(json).unwrap();
        assert_eq!(back.name, "app");
        assert_eq!(back.mounts, config.mounts);
        assert_eq!(back.network, config.network);
    }
}

/// 容器事件（从 podman /events 流映射）。
///
/// 仅包含我们关心的事件类型；客户端侧过滤（见 docs/08-requirements.md #23712）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EngineEvent {
    /// 容器创建（create）
    ContainerCreated { container_id: String, name: String },
    /// 容器启动（start）
    ContainerStarted { container_id: String, name: String },
    /// 容器停止（die - podman 用 "died"）
    ContainerDied { container_id: String, name: String, exit_code: i64 },
    /// 容器删除（die）
    ContainerRemoved { container_id: String, name: String },
}
