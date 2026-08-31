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

/// `ContainerParams.network` 的 serde 默认值（旧 config.toml 兼容）。
fn default_network() -> NetworkConfig {
    NetworkConfig::default()
}

/// `ContainerParams.keep_id` 的 serde 默认值（旧配置无该字段 → 默认开启）。
fn default_true() -> bool {
    true
}

/// 容器核心参数——模板与实例共享的基座。
///
/// [`Flavor`](crate::flavor::Flavor)（模板，存意图）与 [`ContainerConfig`]
/// （实例，存快照）经 `#[serde(flatten)]` 组合本结构：序列化形状与拆分前
/// 完全一致（TOML/JSON 字段平铺在外层，旧文件直接兼容；toml 0.8 pretty
/// 序列化器自动把表类字段排到末尾，flatten 无值后置表问题——2026-08-18 实测）。
///
/// 模板同步（config ← flavor 重展开）以本结构为传输单位：`env` / `name` /
/// `silent_boot` / `persistent` 属于实例侧，不参与同步。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerParams {
    /// 基础镜像
    pub image: String,
    /// entry 应用（容器启动时 server 经 `--entry` 链式拉起；类 Docker
    /// ENTRYPOINT。shell 执行（`su -c`），参数经 `entry_args` 追加）
    pub entry: Option<String>,
    /// entry 应用参数（拼接在 entry 后空格分隔；含空格的参数需引号）
    #[serde(default)]
    pub entry_args: Vec<String>,
    /// 路径映射（bind mount）
    #[serde(default)]
    pub mounts: Vec<MountConfig>,
    /// 网络配置（默认 Host 模式）
    #[serde(default = "default_network")]
    pub network: NetworkConfig,
    /// 用户命名空间 keep-id：开启时宿主登录 uid ↔ 容器同 uid 锁死
    /// （podman `userns.keep-id`，docs/12）。与 GUI 透传的关系：
    /// `gui=true` 的 flavor 展开时强制开启。
    /// 旧字段名 `user_home`（用户一致性映射）经 alias 无缝读入。
    #[serde(default = "default_true", alias = "user_home")]
    pub keep_id: bool,
    /// 容器默认用户 uid（`None` = 创建时取宿主登录 uid）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_uid: Option<u32>,
    /// 容器默认用户 gid（`None` = 创建时取宿主登录 gid）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_gid: Option<u32>,
    /// 容器内用户名（可选；设置后创建/重建时经宿主 root exec 幂等 useradd
    /// 建号，否则容器仅按 uid/gid 运行、可能无 passwd 条目）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_name: Option<String>,
    /// GUI 透传（意图字段）：展开/重建时自动注入宿主显示环境
    /// （DISPLAY/WAYLAND_DISPLAY/XDG_RUNTIME_DIR/XDG_DATA_DIRS）+ X11/Wayland
    /// socket、$XDG_RUNTIME_DIR、字体图标只读挂载，并强制 keep_id。存意图，按宿主
    /// 实时探测注入（见 `inject_gui_passthrough`）。旧配置缺省 false。
    #[serde(default)]
    pub gui: bool,
    /// GPU 透传（意图字段）：值为 "all" / 设备名 / "device=<uuid>"；展开/重建时经
    /// `nvidia.com/gpu=<值>` CDI 引用注入设备节点 + NVIDIA_VISIBLE_DEVICES/
    /// NVIDIA_DRIVER_CAPABILITIES env。需宿主 NVIDIA Container Toolkit 已生成 CDI
    /// spec。None = 不透传。通用能力，具体值由配置/flavor 声明。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu: Option<String>,
    /// 设备直通（podman `--device` 列表；裸设备 "host:container[:perms]"，
    /// 或 CDI 引用如 "nvidia.com/gpu=all"）
    #[serde(default)]
    pub devices: Vec<String>,
    /// 安全选项（podman `--security-opt` 列表；如 "label=disable" /
    /// "apparmor=unconfined" / "seccomp=unconfined"。防 SELinux/AppArmor 拦截设备节点）
    #[serde(default)]
    pub security_opts: Vec<String>,
    /// PID 命名空间模式（如 "host"；默认 private。与 init 互斥——设为非 private
    /// 时不注入 init/catatonit）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<String>,
}

impl Default for ContainerParams {
    /// 与 serde 默认保持一致：`keep_id` 默认 true；用户/设备字段缺省 None/空。
    fn default() -> Self {
        Self {
            image: String::new(),
            entry: None,
            entry_args: Vec::new(),
            mounts: Vec::new(),
            network: NetworkConfig::default(),
            keep_id: true,
            user_uid: None,
            user_gid: None,
            user_name: None,
            gui: false,
            gpu: None,
            devices: Vec::new(),
            security_opts: Vec::new(),
            pid: None,
        }
    }
}

/// 容器配置（easytidy 自有元数据，存于宿主共享配置文件）。
///
/// 核心参数在 [`ContainerParams`]（与 flavor 模板共享）；本结构是**实例**
/// 快照：镜像/挂载/网络等展开结果 + 实例专属字段。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerConfig {
    pub name: String,
    /// 核心参数（模板共享基座）
    #[serde(flatten)]
    pub params: ContainerParams,
    /// 容器环境变量（"KEY=VALUE" 列表，GUI 透传时含宿主 DISPLAY/WAYLAND_DISPLAY/XAUTHORITY
    /// ——flavor 展开期的解析快照，随会话可能变化，不参与模板同步）
    #[serde(default)]
    pub env: Vec<String>,
    /// 静默启动标志（宿主开机自启）
    pub silent_boot: bool,
    /// 是否常驻（catatonit + server 生命周期）
    pub persistent: bool,
    /// 血缘：来源 flavor 模板名（展开时盖章）。模板同步与漂移检测依据；
    /// `None` = 自由创建（镜像起步），不参与模板生态
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flavor: Option<String>,
}

impl Default for ContainerConfig {
    /// 与 serde 默认保持一致：`params.user_home` 默认 true。
    fn default() -> Self {
        Self {
            name: String::new(),
            params: ContainerParams::default(),
            env: Vec::new(),
            silent_boot: false,
            persistent: false,
            flavor: None,
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
    /// 容器进程用户（inspect `Config.User`；新模型下为配置值 `<uid>:<gid>`，
    /// 旧 root 容器为 "0:0"，未设则 None）
    #[serde(default)]
    pub user: Option<String>,
    /// userns 模式（inspect `HostConfig.UsernsMode`；keep-id 容器实际回显
    /// 可能为 "private" 或 None——语义以 `keep_id` 配置 + docs/12 为准）
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
        assert!(config.params.mounts.is_empty());
        assert_eq!(config.params.network.mode, NetworkMode::Host);
        assert!(config.params.network.ports.is_empty());
        assert!(config.env.is_empty());
        assert!(config.params.keep_id);
        assert!(config.params.user_uid.is_none());
        assert!(config.params.user_gid.is_none());
        assert!(config.params.user_name.is_none());
        // flatten 形状：基座字段平铺在外层（旧文件直接兼容）
        assert_eq!(config.params.image, "alpine:latest");
        assert_eq!(config.params.entry.as_deref(), Some("/bin/sh"));
        assert_eq!(config.flavor, None);
        // 设备/安全/PID/GUI/GPU 新字段：旧配置无 → 缺省（None/空/false）
        assert!(!config.params.gui);
        assert!(config.params.gpu.is_none());
        assert!(config.params.devices.is_empty());
        assert!(config.params.security_opts.is_empty());
        assert!(config.params.pid.is_none());
    }

    #[test]
    fn test_device_fields_roundtrip() {
        // 设备/安全/PID/GUI/GPU 字段：TOML 平铺形状读写（与 conf YAML 同走 serde 数据模型）
        let toml_str = r#"
name = "chrome"
image = "ubuntu:24.04"
gui = true
gpu = "all"
devices = ["/dev/uinput:/dev/uinput"]
security_opts = ["label=disable", "apparmor=unconfined"]
pid = "host"
silent_boot = false
persistent = true
"#;
        let config: ContainerConfig = toml::from_str(toml_str).unwrap();
        assert!(config.params.gui);
        assert_eq!(config.params.gpu.as_deref(), Some("all"));
        assert_eq!(config.params.devices, vec!["/dev/uinput:/dev/uinput".to_string()]);
        assert_eq!(
            config.params.security_opts,
            vec!["label=disable".to_string(), "apparmor=unconfined".to_string()]
        );
        assert_eq!(config.params.pid.as_deref(), Some("host"));

        // 序列化形状：gpu/pid 为 None 时省略（skip_serializing_if），devices/security 空时保留
        let v = serde_json::to_value(&config).unwrap();
        assert_eq!(v["gpu"], "all");
        assert_eq!(v["gui"], true);
        assert_eq!(v["pid"], "host");
        assert_eq!(v["devices"][0], "/dev/uinput:/dev/uinput");
        assert_eq!(v["security_opts"][1], "apparmor=unconfined");
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
    fn test_keep_id_serde_default_true() {
        // 旧配置文件（无 keep_id 字段）→ 默认 true
        let toml_str = r#"
name = "legacy"
image = "alpine:latest"
silent_boot = false
persistent = true
"#;
        let config: ContainerConfig = toml::from_str(toml_str).unwrap();
        assert!(config.params.keep_id, "旧配置缺失 keep_id 字段应默认 true");

        // 旧字段名 user_home 经 alias 读入
        let toml_str = r#"
name = "legacy-name"
image = "alpine:latest"
silent_boot = false
persistent = true
user_home = false
"#;
        let config: ContainerConfig = toml::from_str(toml_str).unwrap();
        assert!(!config.params.keep_id, "旧字段名 user_home 应经 alias 读入");

        // 显式 false → 关闭 keep-id（非 GUI 容器可选项）
        let toml_str = r#"
name = "no-keep"
image = "alpine:latest"
silent_boot = false
persistent = true
keep_id = false
"#;
        let config: ContainerConfig = toml::from_str(toml_str).unwrap();
        assert!(!config.params.keep_id);

        // Rust 侧 Default 与 serde 默认一致
        assert!(ContainerConfig::default().params.keep_id);
    }

    #[test]
    fn test_user_fields_serde_default_none() {
        // 缺省 → None（创建时取宿主值）；显式值往返不丢
        let toml_str = r#"
name = "uid"
image = "alpine:latest"
silent_boot = false
persistent = true
user_uid = 1000
user_gid = 1000
user_name = "tidy"
"#;
        let config: ContainerConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.params.user_uid, Some(1000));
        assert_eq!(config.params.user_gid, Some(1000));
        assert_eq!(config.params.user_name.as_deref(), Some("tidy"));

        // skip_serializing_if：None 字段不落盘
        let bare = ContainerConfig {
            name: "bare".to_string(),
            ..Default::default()
        };
        let toml_str = toml::to_string(&bare).unwrap();
        assert!(!toml_str.contains("user_uid"));
        assert!(!toml_str.contains("user_gid"));
        assert!(!toml_str.contains("user_name"));
    }

    #[test]
    fn test_container_config_roundtrip() {
        let config = ContainerConfig {
            name: "app".to_string(),
            params: ContainerParams {
                image: "alpine:latest".to_string(),
                entry: Some("/bin/sh".to_string()),
                entry_args: vec!["--verbose".to_string()],
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
                keep_id: true,
                user_uid: Some(1000),
                user_gid: Some(1000),
                user_name: Some("tidy".to_string()),
                gui: true,
                gpu: Some("all".to_string()),
                devices: vec!["/dev/uinput:/dev/uinput".to_string()],
                security_opts: vec!["label=disable".to_string(), "apparmor=unconfined".to_string()],
                pid: Some("host".to_string()),
            },
            env: vec!["DISPLAY=:0".to_string()],
            silent_boot: true,
            persistent: true,
            flavor: Some("chrome".to_string()),
        };

        // TOML 往返（configfile 格式；flatten 平铺形状不变）
        let toml_str = toml::to_string(&config).unwrap();
        let back: ContainerConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(back.params, config.params);
        assert_eq!(back.flavor, config.flavor);
        // 血缘 skip_serializing_if=None：无血缘时不落盘
        let no_lineage = ContainerConfig {
            flavor: None,
            ..config.clone()
        };
        let toml_str = toml::to_string(&no_lineage).unwrap();
        assert!(!toml_str.contains("flavor"));

        // JSON 往返（Tauri 命令格式）
        let json = serde_json::to_value(&config).unwrap();
        let back: ContainerConfig = serde_json::from_value(json).unwrap();
        assert_eq!(back.name, "app");
        assert_eq!(back.params, config.params);
        assert_eq!(back.flavor, config.flavor);
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
