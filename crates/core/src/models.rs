//! 数据模型（GUI / CLI / engine 共用）。

use serde::de::{self, Deserializer, MapAccess, Visitor};
use serde::ser::{SerializeStruct, Serializer};
use serde::{Deserialize, Serialize};
use std::fmt;

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

/// 下层引擎信息（podman `/info` 只读快照；GUI「环境信息」界面 / 存储健康检测共用）。
///
/// 字段均可为空——podman 不同版本对 `/info` 的填充程度不一，缺字段时前端显示「-」。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EngineInfo {
    /// 引擎版本（ServerVersion，如 "5.4.2"）
    pub version: Option<String>,
    /// 存储驱动（"overlay" / "vfs" / ...）
    pub storage_driver: Option<String>,
    /// 存储驱动细节（[key, value] 对：Backing Filesystem / Native Overlay Diff /
    /// Supports shifting 等；podman 5.x 实测 6 项）
    pub storage_driver_status: Vec<(String, String)>,
    /// 存储根目录（rootless：~/.local/share/containers/storage）
    pub storage_root: Option<String>,
    /// 是否 rootless（SecurityOptions 含 "name=rootless"）
    pub rootless: bool,
    /// 默认 OCI 运行时（crun / runc / ...）
    pub default_runtime: Option<String>,
    /// cgroup 驱动（systemd / cgroupfs）
    pub cgroup_driver: Option<String>,
    /// cgroup 版本（"1" / "2"）
    pub cgroup_version: Option<String>,
    /// 宿主操作系统（如 "ubuntu"）
    pub os: Option<String>,
    /// 内核版本
    pub kernel_version: Option<String>,
    /// 架构（amd64 / arm64 / ...）
    pub arch: Option<String>,
    /// CPU 数
    pub ncpu: Option<u64>,
    /// 总内存（字节）
    pub mem_total: Option<u64>,
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

/// UID/GID 重映射条目（对应 podman `--uidmap container:host:length` /
/// OCI `linux.uidMappings` 的 `{containerID, hostID, size}`）。
///
/// 显式映射与 `keep_id` **互斥**（podman 实测 `--uidmap` 与 `--userns` 不能同开）：
/// `uidmaps`/`gidmaps` 非空时 keep-id 失效，由显式映射完全决定 uid/gid 落位。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdMapping {
    /// 容器内起始 UID/GID
    pub container_id: u32,
    /// 宿主对应起始 UID/GID
    pub host_id: u32,
    /// 映射长度（连续区间个数）
    pub length: u32,
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

/// 容器核心参数——模板与实例共享的基座。
///
/// [`Flavor`](crate::flavor::Flavor)（模板，存意图）与 [`ContainerConfig`]
/// （实例，存快照）经 `#[serde(flatten)]` 组合本结构：序列化形状与拆分前
/// 完全一致（TOML/JSON 字段平铺在外层，旧文件直接兼容；toml 0.8 pretty
// 序列化器自动把表类字段排到末尾，flatten 无值后置表问题——2026-08-18 实测）。
///
/// 模板仅作**创建期预填**：展开为实例快照后，容器与模板彻底解耦（无血缘
/// 字段、无同步、无漂移检测）。`env` / `name` / `silent_boot` / `persistent`
/// 属于实例侧。
///
/// **手动 `Serialize` / `Deserialize`（impl 在结构体下方）**：保留 `user_home`
/// 作为 `keep_id` 别名；GPU 字段无迁移（程序未发布）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerParams {
    /// 基础镜像
    pub image: String,
    /// entry 应用（容器启动时 server 经 `--entry` 链式拉起；类 Docker
    /// ENTRYPOINT。shell 执行（`su -c`），参数经 `entry_args` 追加）
    pub entry: Option<String>,
    /// entry 应用参数（拼接在 entry 后空格分隔；含空格的参数需引号）
    pub entry_args: Vec<String>,
    /// 路径映射（bind mount）
    pub mounts: Vec<MountConfig>,
    /// 网络配置（默认 Host 模式）
    pub network: NetworkConfig,
    /// 用户命名空间 keep-id：开启时宿主登录 uid ↔ 容器同 uid 锁死
    /// （podman `userns.keep-id`，docs/12）。与 GUI 透传的关系：
    /// `gui=true` 的 flavor 展开时强制开启。
    /// 旧字段名 `user_home`（用户一致性映射）经自定义 Deserialize 自动迁移。
    pub keep_id: bool,
    /// 显式 UID 重映射（podman `--uidmap` 列表）。非空时与 `keep_id` 互斥——
    /// 覆盖 keep-id，由显式映射精确决定容器 uid 落位（见 [`IdMapping`]）。
    /// 旧配置无此字段，缺省空 = 走 keep-id / 默认 rootless 映射。
    pub uidmaps: Vec<IdMapping>,
    /// 显式 GID 重映射（podman `--gidmap` 列表）。非空时与 `keep_id` 互斥。
    pub gidmaps: Vec<IdMapping>,
    /// 容器默认用户 uid（`None` = 创建时取宿主登录 uid）
    pub user_uid: Option<u32>,
    /// 容器默认用户 gid（`None` = 创建时取宿主登录 gid）
    pub user_gid: Option<u32>,
    /// 容器内用户名（可选；设置后创建/重建时经宿主 root exec 幂等 useradd
    /// 建号，否则容器仅按 uid/gid 运行、可能无 passwd 条目）
    pub user_name: Option<String>,
    /// GUI 透传（意图字段）：展开/重建时自动注入宿主显示环境
    /// （DISPLAY/WAYLAND_DISPLAY/XDG_RUNTIME_DIR/XDG_DATA_DIRS）+ X11/Wayland
    /// socket、$XDG_RUNTIME_DIR、字体图标只读挂载，并强制 keep_id。存意图，按宿主
    /// 实时探测注入（见 `inject_gui_passthrough`）。旧配置缺省 false。
    pub gui: bool,
    /// NVIDIA GPU 透传（意图字段）。开启时展开/重建注入：
    /// - `NVIDIA_VISIBLE_DEVICES=all` + `NVIDIA_DRIVER_CAPABILITIES=all` env
    /// - `nvidia.com/gpu=all` CDI 设备节点
    ///
    /// 需宿主已装 NVIDIA Container Toolkit 并生成 CDI spec。
    /// 旧版本单字段 `gpu: "nvidia..."` 经自定义 Deserialize 自动迁移。
    pub gpu_nvidia: bool,
    /// AMD GPU 透传（意图字段）。开启时 create 期探测宿主 AMD 裸设备注入
    /// CDI 设备节点；无 vendor 专属 env（ROCm 容器内自检即可）。
    ///
    /// 需宿主已装 AMD Container Toolkit 并生成 CDI spec。
    /// 旧版本单字段 `gpu: "amd..."` 经自定义 Deserialize 自动迁移。
    pub gpu_amd: bool,
    /// 设备直通（podman `--device` 列表；裸设备 "host:container[:perms]"，
    /// 或 CDI 引用如 "nvidia.com/gpu=all"）
    pub devices: Vec<String>,
    /// 额外选项（podman 透传原始串列表，如 "label=disable" / "apparmor=unconfined" /
    /// "seccomp=unconfined"，解析进 SpecGenerator 对应字段）。
    /// 旧字段名 `security_opts` 经自定义 Deserialize 自动迁移。
    pub extra_opts: Vec<String>,
    /// PID 命名空间模式（如 "host"；默认 private。与 init 互斥——设为非 private
    /// 时不注入 init/catatonit）
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
            uidmaps: Vec::new(),
            gidmaps: Vec::new(),
            user_uid: None,
            user_gid: None,
            user_name: None,
            gui: false,
            gpu_nvidia: false,
            gpu_amd: false,
            devices: Vec::new(),
            extra_opts: Vec::new(),
            pid: None,
        }
    }
}

/// 手动 Serialize：固定字段顺序 + 默认跳过 false / 空 / None（与原 `#[derive(Serialize)]`
/// 行为对齐；旧字段名 `gpu` / `user_home` / `security_opts` 不再写出——避免下一轮 round-trip 时污染）。
impl Serialize for ContainerParams {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut st = s.serialize_struct("ContainerParams", 17)?;
        st.serialize_field("image", &self.image)?;
        st.serialize_field("entry", &self.entry)?;
        st.serialize_field("entry_args", &self.entry_args)?;
        st.serialize_field("mounts", &self.mounts)?;
        st.serialize_field("network", &self.network)?;
        st.serialize_field("keep_id", &self.keep_id)?;
        if !self.uidmaps.is_empty() {
            st.serialize_field("uidmaps", &self.uidmaps)?;
        }
        if !self.gidmaps.is_empty() {
            st.serialize_field("gidmaps", &self.gidmaps)?;
        }
        if self.user_uid.is_some() {
            st.serialize_field("user_uid", &self.user_uid)?;
        }
        if self.user_gid.is_some() {
            st.serialize_field("user_gid", &self.user_gid)?;
        }
        if self.user_name.is_some() {
            st.serialize_field("user_name", &self.user_name)?;
        }
        st.serialize_field("gui", &self.gui)?;
        if self.gpu_nvidia {
            st.serialize_field("gpu_nvidia", &self.gpu_nvidia)?;
        }
        if self.gpu_amd {
            st.serialize_field("gpu_amd", &self.gpu_amd)?;
        }
        st.serialize_field("devices", &self.devices)?;
        st.serialize_field("extra_opts", &self.extra_opts)?;
        if self.pid.is_some() {
            st.serialize_field("pid", &self.pid)?;
        }
        st.end()
    }
}

/// 手动 Deserialize：保留 `user_home`（旧版 keep_id 别名）以避免破坏老手写
/// yaml/flavor，但 GPU 拆分后 `gpu: "<vendor>"` 字段直接忽略（程序未发布，
/// 无迁移负担）。
#[allow(clippy::field_reassign_with_default)] // deser 默认 + 按字段写回是惯用 pattern
impl<'de> Deserialize<'de> for ContainerParams {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct ParamsVisitor;
        impl<'de> Visitor<'de> for ParamsVisitor {
            type Value = ContainerParams;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("ContainerParams")
            }

            fn visit_map<V: MapAccess<'de>>(self, mut map: V) -> Result<ContainerParams, V::Error> {
                // 重复字段追踪
                #[derive(Default)]
                struct Seen {
                    image: bool,
                    entry: bool,
                    entry_args: bool,
                    mounts: bool,
                    network: bool,
                    keep_id: bool,
                    uidmaps: bool,
                    gidmaps: bool,
                    user_uid: bool,
                    user_gid: bool,
                    user_name: bool,
                    gui: bool,
                    gpu_nvidia: bool,
                    gpu_amd: bool,
                    devices: bool,
                    extra_opts: bool,
                    pid: bool,
                }
                let mut seen = Seen::default();

                // None = 字段缺失（回落到 Default）；Some(_) = 已读入
                let mut image: Option<String> = None;
                let mut entry: Option<Option<String>> = None;
                let mut entry_args: Option<Vec<String>> = None;
                let mut mounts: Option<Vec<MountConfig>> = None;
                let mut network: Option<NetworkConfig> = None;
                let mut keep_id: Option<bool> = None;
                let mut uidmaps: Option<Vec<IdMapping>> = None;
                let mut gidmaps: Option<Vec<IdMapping>> = None;
                let mut user_uid: Option<Option<u32>> = None;
                let mut user_gid: Option<Option<u32>> = None;
                let mut user_name: Option<Option<String>> = None;
                let mut gui: Option<bool> = None;
                let mut gpu_nvidia: Option<bool> = None;
                let mut gpu_amd: Option<bool> = None;
                let mut devices: Option<Vec<String>> = None;
                let mut extra_opts: Option<Vec<String>> = None;
                let mut pid: Option<Option<String>> = None;

                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "image" => {
                            if seen.image {
                                return Err(de::Error::duplicate_field("image"));
                            }
                            image = Some(map.next_value()?);
                            seen.image = true;
                        }
                        "entry" => {
                            if seen.entry {
                                return Err(de::Error::duplicate_field("entry"));
                            }
                            entry = Some(map.next_value()?);
                            seen.entry = true;
                        }
                        "entry_args" => {
                            if seen.entry_args {
                                return Err(de::Error::duplicate_field("entry_args"));
                            }
                            entry_args = Some(map.next_value()?);
                            seen.entry_args = true;
                        }
                        "mounts" => {
                            if seen.mounts {
                                return Err(de::Error::duplicate_field("mounts"));
                            }
                            mounts = Some(map.next_value()?);
                            seen.mounts = true;
                        }
                        "network" => {
                            if seen.network {
                                return Err(de::Error::duplicate_field("network"));
                            }
                            network = Some(map.next_value()?);
                            seen.network = true;
                        }
                        "keep_id" | "user_home" => {
                            if seen.keep_id {
                                return Err(de::Error::duplicate_field("keep_id"));
                            }
                            keep_id = Some(map.next_value()?);
                            seen.keep_id = true;
                        }
                        "uidmaps" => {
                            if seen.uidmaps {
                                return Err(de::Error::duplicate_field("uidmaps"));
                            }
                            uidmaps = Some(map.next_value()?);
                            seen.uidmaps = true;
                        }
                        "gidmaps" => {
                            if seen.gidmaps {
                                return Err(de::Error::duplicate_field("gidmaps"));
                            }
                            gidmaps = Some(map.next_value()?);
                            seen.gidmaps = true;
                        }
                        "user_uid" => {
                            if seen.user_uid {
                                return Err(de::Error::duplicate_field("user_uid"));
                            }
                            user_uid = Some(map.next_value()?);
                            seen.user_uid = true;
                        }
                        "user_gid" => {
                            if seen.user_gid {
                                return Err(de::Error::duplicate_field("user_gid"));
                            }
                            user_gid = Some(map.next_value()?);
                            seen.user_gid = true;
                        }
                        "user_name" => {
                            if seen.user_name {
                                return Err(de::Error::duplicate_field("user_name"));
                            }
                            user_name = Some(map.next_value()?);
                            seen.user_name = true;
                        }
                        "gui" => {
                            if seen.gui {
                                return Err(de::Error::duplicate_field("gui"));
                            }
                            gui = Some(map.next_value()?);
                            seen.gui = true;
                        }
                        "gpu_nvidia" => {
                            if seen.gpu_nvidia {
                                return Err(de::Error::duplicate_field("gpu_nvidia"));
                            }
                            gpu_nvidia = Some(map.next_value()?);
                            seen.gpu_nvidia = true;
                        }
                        "gpu_amd" => {
                            if seen.gpu_amd {
                                return Err(de::Error::duplicate_field("gpu_amd"));
                            }
                            gpu_amd = Some(map.next_value()?);
                            seen.gpu_amd = true;
                        }
                        "devices" => {
                            if seen.devices {
                                return Err(de::Error::duplicate_field("devices"));
                            }
                            devices = Some(map.next_value()?);
                            seen.devices = true;
                        }
                        "extra_opts" | "security_opts" => {
                            // 旧字段名 `security_opts` 作别名接受（同 `user_home` →
                            // `keep_id` 模式），存量磁盘配置不静默丢安全选项
                            if seen.extra_opts {
                                return Err(de::Error::duplicate_field("extra_opts"));
                            }
                            extra_opts = Some(map.next_value()?);
                            seen.extra_opts = true;
                        }
                        "pid" => {
                            if seen.pid {
                                return Err(de::Error::duplicate_field("pid"));
                            }
                            pid = Some(map.next_value()?);
                            seen.pid = true;
                        }
                        _ => {
                            // 未知字段跳过（保持前向兼容）
                            let _: serde::de::IgnoredAny = map.next_value()?;
                        }
                    }
                }

                let image = image.ok_or_else(|| de::Error::missing_field("image"))?;
                let mut p = ContainerParams::default();
                p.image = image;
                if let Some(v) = entry { p.entry = v; }
                if let Some(v) = entry_args { p.entry_args = v; }
                if let Some(v) = mounts { p.mounts = v; }
                if let Some(v) = network { p.network = v; }
                if let Some(v) = keep_id { p.keep_id = v; }
                if let Some(v) = uidmaps { p.uidmaps = v; }
                if let Some(v) = gidmaps { p.gidmaps = v; }
                if let Some(v) = user_uid { p.user_uid = v; }
                if let Some(v) = user_gid { p.user_gid = v; }
                if let Some(v) = user_name { p.user_name = v; }
                if let Some(v) = gui { p.gui = v; }
                if let Some(v) = gpu_nvidia { p.gpu_nvidia = v; }
                if let Some(v) = gpu_amd { p.gpu_amd = v; }
                if let Some(v) = devices { p.devices = v; }
                if let Some(v) = extra_opts { p.extra_opts = v; }
                if let Some(v) = pid { p.pid = v; }

                Ok(p)
            }
        }

        d.deserialize_map(ParamsVisitor)
    }
}

/// 容器配置（easytidy 自有元数据，存于宿主共享配置文件）。
///
/// 核心参数在 [`ContainerParams`]（与 flavor 模板共享）；本结构是**实例**
/// 快照：镜像/挂载/网络等展开结果 + 实例专属字段。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerConfig {
    pub name: String,
    /// 核心参数（创建期可来自模板预填的共享基座）
    #[serde(flatten)]
    pub params: ContainerParams,
    /// 容器环境变量（"KEY=VALUE" 列表，GUI 透传时含宿主 DISPLAY/WAYLAND_DISPLAY/XAUTHORITY
    /// ——创建期解析快照，随会话可能变化）
    #[serde(default)]
    pub env: Vec<String>,
    /// 静默启动标志（宿主开机自启）
    pub silent_boot: bool,
    /// 是否常驻（catatonit + server 生命周期）
    pub persistent: bool,
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

/// 容器当前详细状态（启动失败展示用）：来源 `podman inspect` 的 `State` 字段。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerStateView {
    /// 是否运行中（与 `status == "running"` 一致）
    pub running: bool,
    /// podman 原生状态字符串（"running" / "exited" / "created" / "configured" / "stopped"）
    pub status: String,
    /// 退出码（仅 exited 容器有值）
    #[serde(default)]
    pub exit_code: Option<i64>,
    /// podman 上报的错误信息（如 OCI hook 失败、镜像损坏、device 不可用等）
    #[serde(default)]
    pub error: Option<String>,
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
        // 设备/安全/PID/GUI/GPU 新字段：旧配置无 → 缺省（None/空/false）
        assert!(!config.params.gui);
        assert!(!config.params.gpu_nvidia);
        assert!(!config.params.gpu_amd);
        assert!(config.params.devices.is_empty());
        assert!(config.params.extra_opts.is_empty());
        assert!(config.params.pid.is_none());
    }

    #[test]
    fn test_device_fields_roundtrip() {
        // 设备/安全/PID/GUI/GPU 字段：TOML 平铺形状读写（与 conf YAML 同走 serde 数据模型）
        let toml_str = r#"
name = "chrome"
image = "ubuntu:24.04"
gui = true
gpu_nvidia = true
devices = ["/dev/uinput:/dev/uinput"]
extra_opts = ["label=disable", "apparmor=unconfined"]
pid = "host"
silent_boot = false
persistent = true
"#;
        let config: ContainerConfig = toml::from_str(toml_str).unwrap();
        assert!(config.params.gui);
        assert!(config.params.gpu_nvidia);
        assert!(!config.params.gpu_amd);
        assert_eq!(config.params.devices, vec!["/dev/uinput:/dev/uinput".to_string()]);
        assert_eq!(
            config.params.extra_opts,
            vec!["label=disable".to_string(), "apparmor=unconfined".to_string()]
        );
        assert_eq!(config.params.pid.as_deref(), Some("host"));

        // 序列化形状：pid None 时省略；gpu_* true 时写出；devices/security 始终保留
        let v = serde_json::to_value(&config).unwrap();
        assert_eq!(v["gpu_nvidia"], true);
        assert_eq!(v["gui"], true);
        assert_eq!(v["pid"], "host");
        assert_eq!(v["devices"][0], "/dev/uinput:/dev/uinput");
        assert_eq!(v["extra_opts"][1], "apparmor=unconfined");
    }

    #[test]
    fn test_security_opts_legacy_field_migrates_to_extra_opts() {
        // 旧字段名 `security_opts`（重命名前磁盘存量）自动迁移到 `extra_opts`
        // （同 `user_home` → `keep_id` 别名模式）——不得静默丢安全选项
        let toml_str = r#"
name = "chrome"
image = "ubuntu:24.04"
security_opts = ["label=disable", "apparmor=unconfined"]
silent_boot = false
persistent = true
"#;
        let config: ContainerConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.params.extra_opts,
            vec!["label=disable".to_string(), "apparmor=unconfined".to_string()]
        );

        // round-trip 只写新字段名（旧名不再写出，避免下一轮污染）
        let v = serde_json::to_value(&config).unwrap();
        assert!(v.get("security_opts").is_none());
        assert_eq!(v["extra_opts"][0], "label=disable");
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
                uidmaps: vec![IdMapping {
                    container_id: 0,
                    host_id: 1000,
                    length: 1,
                }],
                gidmaps: vec![],
                user_uid: Some(1000),
                user_gid: Some(1000),
                user_name: Some("tidy".to_string()),
                gui: true,
                gpu_nvidia: true,
                gpu_amd: false,
                devices: vec!["/dev/uinput:/dev/uinput".to_string()],
                extra_opts: vec!["label=disable".to_string(), "apparmor=unconfined".to_string()],
                pid: Some("host".to_string()),
            },
            env: vec!["DISPLAY=:0".to_string()],
            silent_boot: true,
            persistent: true,
        };

        // TOML 往返（configfile 格式；flatten 平铺形状不变）
        let toml_str = toml::to_string(&config).unwrap();
        let back: ContainerConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(back.params, config.params);

        // JSON 往返（Tauri 命令格式）
        let json = serde_json::to_value(&config).unwrap();
        let back: ContainerConfig = serde_json::from_value(json).unwrap();
        assert_eq!(back.name, "app");
        assert_eq!(back.params, config.params);
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
