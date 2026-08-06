//! easytidy-core 错误类型。

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("XDG_RUNTIME_DIR 未设置（无法定位 rootless podman socket）")]
    NoXdgRuntime,
    #[error("podman 连接失败：{0}")]
    Connect(String),
    #[error("podman API 调用失败：{0}")]
    Api(#[from] bollard::errors::Error),
    #[error("IO 错误：{0}")]
    Io(#[from] std::io::Error),
    #[error("配置文件解析失败：{0}")]
    Config(String),
    #[error("容器不存在：{0}")]
    NotFound(String),
}

pub type Result<T> = std::result::Result<T, Error>;
