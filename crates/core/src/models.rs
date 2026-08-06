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

/// 容器配置（easytidy 自有元数据，存于宿主共享配置文件）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ContainerConfig {
    pub name: String,
    pub image: String,
    /// entry 应用（静默启动时链式拉起；类 Docker ENTRYPOINT）
    pub entry: Option<String>,
    /// 静默启动标志（宿主开机自启）
    pub silent_boot: bool,
    /// 是否常驻（catatonit + server 生命周期）
    pub persistent: bool,
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
