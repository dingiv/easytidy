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
use crate::models::ContainerSummary;

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

    /// 创建容器。
    ///
    /// 参数：
    /// - name: 容器名
    /// - image: 镜像（如 "docker.io/library/alpine:latest"）
    /// - server_bin_path: server 二进制路径（宿主绝对路径，将 ro bind-mount 进容器）
    ///
    /// 配置：
    /// - HostConfig.init = true（catatonit = PID 1）
    /// - Cmd = [<server-bin>, "--socket", "/run/easytidy/server.sock"]
    /// - Bind mounts: server 二进制（ro）+ socket 目录（rw）
    /// - 标签: manager=easytidy + easytidy.name=<name>
    ///
    /// 镜如不存在则先拉取。
    pub async fn create(
        &self,
        name: &str,
        image: &str,
        server_bin_path: &Path,
    ) -> Result<String> {
        use bollard::container::{CreateContainerOptions, Config};
        use bollard::models::{HostConfig, Mount, MountTypeEnum};
        use std::collections::HashMap;

        // 检查镜像是否存在，不存在则拉取
        if !self.image_exists(image).await? {
            tracing::info!("镜像 {} 不存在，开始拉取...", image);
            self.pull_image(image).await?;
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

        // 构建挂载配置
        let mounts = vec![
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

        // 构建 HostConfig
        let host_config = HostConfig {
            init: Some(true), // catatonit = PID 1
            mounts: Some(mounts),
            ..Default::default()
        };

        // 构建容器配置
        let config = Config {
            image: Some(image.to_string()),
            cmd: Some(vec![
                "/usr/bin/easytidy-server".to_string(),
                "--socket".to_string(),
                "/run/easytidy/server.sock".to_string(),
            ]),
            labels: Some(labels),
            host_config: Some(host_config),
            ..Default::default()
        };

        let opts = CreateContainerOptions {
            name: name.to_string(),
            ..Default::default()
        };

        let result = self.docker.create_container(
            Some(opts),
            config,
        ).await.map_err(|e| Error::Connect(format!("创建容器失败：{e}")))?;

        let id = result.id;
        tracing::info!("容器 {} 创建成功（ID: {}）", name, id);
        Ok(id)
    }

    /// 启动容器（按名或 ID）。
    pub async fn start(&self, name_or_id: &str) -> Result<()> {
        use bollard::container::StartContainerOptions;

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
    pub async fn restart(&self, name_or_id: &str) -> Result<()> {
        use bollard::container::RestartContainerOptions;

        // 默认 10 秒超时
        let opts = RestartContainerOptions {
            t: 10,
        };

        self.docker.restart_container(name_or_id, Some(opts)).await
            .map_err(|e| Error::Connect(format!("重启容器失败：{e}")))?;

        tracing::info!("容器 {} 重启成功", name_or_id);
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
    async fn pull_image(&self, image: &str) -> Result<()> {
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
