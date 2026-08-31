//! libpod 扩展端点（手写，经 podman unix socket 的 raw HTTP）。
//!
//! 背景：Docker compat 端点（bollard 使用）不支持 podman 特有的
//! `--userns=keep-id`（容器 uid = 宿主 uid 真对齐）。keep-id 仅在
//! libpod 端点可用：`POST /libpod/containers/create` 的
//! `namespaces.userns.nsmode = "keep-id"`。
//!
//! keep-id 语义（rootless，实测文件属主/访问行为，2026-08-07）：
//! - **容器内 uid 1000（node 用户）= 宿主当前登录用户（uid 1000）**：
//!   读写宿主挂载 home 直接可用（无需 sudo，宿主侧属主正确）、
//!   可访问宿主 /run/user/1000（Wayland/dbus/XAUTHORITY）——GUI 窗口、
//!   宿主 home 读写全部自然打通，且拥有的是宿主用户权限而非 root
//! - 容器内 uid 0（root）= 装包身份：能写容器系统文件（apt/sudo 可用）；
//!   对宿主 home 的写文件以 subuid(100000) 属主呈现（尽量经 sudo 或
//!   node 身份操作宿主文件）
//! - 免密 sudo（/etc/sudoers.d/easytidy-node）作为提升通道
//! - 注意：/proc/self/uid_map 的字面映射（1000→0）不代表实际文件属主
//!   行为——以 keep-id 层的真实身份为准（实测文件属主 = 宿主用户）

use std::path::PathBuf;
use std::task::{Context, Poll};

use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper_util::client::legacy::Client;
use hyper_util::rt::{TokioExecutor, TokioIo};
use serde_json::{json, Value};
use tower_service::Service;

use crate::error::{Error, Result};

/// 基于 unix socket 的连接器（hyper-util legacy client 的 Connect 约束）。
///
/// 注意：hyper 1 的 `hyper::service::Service` 是封死 trait，外部不可实现；
/// 必须实现 `tower_service::Service<Uri>`（hyper-util 内部使用）。
#[derive(Clone)]
struct UnixConnector {
    socket_path: PathBuf,
}

impl Service<hyper::Uri> for UnixConnector {
    type Response = TokioIo<tokio::net::UnixStream>;
    type Error = std::io::Error;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = std::result::Result<Self::Response, Self::Error>> + Send>,
    >;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<std::result::Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _req: hyper::Uri) -> Self::Future {
        let path = self.socket_path.clone();
        Box::pin(async move {
            let stream = tokio::net::UnixStream::connect(path).await?;
            Ok(TokioIo::new(stream))
        })
    }
}

/// libpod 客户端（仅覆盖我们用到的端点）。
///
/// libpod 端点需要 API 版本前缀（`/v<ApiVersion>/libpod/...`，裸 `/libpod/...`
/// 返回 404——podman 实测）；ApiVersion 取自 `GET /version`。
pub struct Libpod {
    client: Client<UnixConnector, Full<Bytes>>,
    api_version: String,
}

fn socket_path() -> Result<PathBuf> {
    let runtime = std::env::var("XDG_RUNTIME_DIR").map_err(|_| Error::NoXdgRuntime)?;
    Ok(PathBuf::from(runtime).join("podman/podman.sock"))
}

impl Libpod {
    pub async fn new() -> Result<Self> {
        let connector = UnixConnector {
            socket_path: socket_path()?,
        };
        let client = Client::builder(TokioExecutor::new()).build(connector);

        // 取 API 版本（libpod 路径前缀需要）
        // URI host 为占位（unix socket 传输，见 UnixConnector 注释）
        let req = hyper::Request::get("http://podman/version")
            .body(Full::new(Bytes::new()))
            .map_err(|e| Error::Connect(format!("构造 /version 请求失败：{e}")))?;
        let resp = client
            .request(req)
            .await
            .map_err(|e| Error::Connect(format!("/version 请求失败：{e}")))?;
        let bytes = resp
            .into_body()
            .collect()
            .await
            .map_err(|e| Error::Connect(format!("读取 /version 失败：{e}")))?
            .to_bytes();
        let v: Value = serde_json::from_slice(&bytes)
            .map_err(|e| Error::Connect(format!("解析 /version 失败：{e}")))?;
        let api_version = v
            .get("ApiVersion")
            .or_else(|| v.get("api_version"))
            .and_then(|x| x.as_str())
            .unwrap_or("5.0.0")
            .to_string();
        tracing::debug!("podman ApiVersion：{api_version}");

        Ok(Self { client, api_version })
    }

    /// POST /v<version>/libpod/containers/create?name=<name>，body 为
    /// Docker-compat 形状 + libpod 扩展（namespaces.userns.nsmode）。返回容器 ID。
    pub async fn create_container(&self, name: &str, body: Value) -> Result<String> {
        // 最终请求体落日志（pretty JSON；排障对照 libpod SpecGenerator 字段）
        tracing::info!(
            "libpod create 请求（容器 {name}）：\n{}",
            serde_json::to_string_pretty(&body).unwrap_or_default()
        );
        // 注：这不是网络请求。URI 中的 "podman" 只是 hyper 强制要求的
        // 绝对 URI 占位主机名——连接层由 UnixConnector 替换为本机
        // $XDG_RUNTIME_DIR/podman/podman.sock 的 unix domain socket
        // （同 podman CLI 自身与 bollard 的传输方式），零网络流量。
        let uri: hyper::Uri = format!(
            "http://podman/v{}/libpod/containers/create?name={}",
            self.api_version,
            urlencoding(name)
        )
        .parse()
        .map_err(|e| Error::Connect(format!("URI 解析失败：{e}")))?;

        let req = hyper::Request::post(uri)
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(serde_json::to_vec(&body).map_err(|e| {
                Error::Config(format!("序列化 libpod create body 失败：{e}"))
            })?)))
            .map_err(|e| Error::Connect(format!("构造请求失败：{e}")))?;

        let resp = self
            .client
            .request(req)
            .await
            .map_err(|e| Error::Connect(format!("libpod create 请求失败：{e}")))?;

        let status = resp.status();
        let resp_bytes = resp
            .into_body()
            .collect()
            .await
            .map_err(|e| Error::Connect(format!("读取响应失败：{e}")))?
            .to_bytes();

        let text = String::from_utf8_lossy(&resp_bytes).to_string();
        if !status.is_success() {
            return Err(Error::Connect(format!(
                "libpod create 失败（HTTP {status}）：{text}"
            )));
        }

        let v: Value = serde_json::from_str(&text)
            .map_err(|e| Error::Connect(format!("解析 libpod create 响应失败：{e}：{text}")))?;
        v.get("Id")
            .or_else(|| v.get("id"))
            .and_then(|id| id.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| Error::Connect(format!("libpod create 响应缺少 Id：{text}")))
    }

    /// POST `/v<version>/libpod/commit?container=<name>&repo=<image_ref>&squash=true`
    ///
    /// 把容器当前文件系统打成一个扁平镜像(单层)。
    ///
    /// - `<image_ref>` 形态 `repo:tag`(或 `repo`,缺省 tag=latest)。libpod 端点把
    ///   `:` 当作 repo/tag 分隔。
    /// - `squash=true` 等价 `podman commit --squash`:把多层合并为单层,镜像体积更小,
    ///   不再叠加源容器原有历史层。适合作为"环境快照"语义(fork 后镜像层是干净的)。
    /// - `message`(OCI 镜像 history 的注释字段)进 image history,便于以后
    ///   `podman inspect` 看见来源备注。**注意:OCI 格式(/libpod/commit)用
    ///   `message`;docker 格式(/commit,已废弃)用 `comment`**——本端点传
    ///   `comment` 会触发 500 "messages are only compatible with the docker
    ///   image format (-f docker)"。
    ///
    /// 返回 commit 响应里的 `Id`(镜像 ID,与 `image_ref` 解析到同一镜像)。
    pub async fn commit_squash(
        &self,
        container_name: &str,
        image_ref: &str,
        message: &str,
    ) -> Result<String> {
        let query = format!(
            "container={}&repo={}&squash=true&message={}",
            urlencoding(container_name),
            urlencoding(image_ref),
            urlencoding(message),
        );
        let uri: hyper::Uri = format!(
            "http://podman/v{}/libpod/commit?{query}",
            self.api_version
        )
        .parse()
        .map_err(|e| Error::Connect(format!("URI 解析失败：{e}")))?;

        let req = hyper::Request::post(uri)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Full::new(Bytes::new()))
            .map_err(|e| Error::Connect(format!("构造 commit 请求失败：{e}")))?;

        let resp = self
            .client
            .request(req)
            .await
            .map_err(|e| Error::Connect(format!("libpod commit 请求失败：{e}")))?;

        let status = resp.status();
        let resp_bytes = resp
            .into_body()
            .collect()
            .await
            .map_err(|e| Error::Connect(format!("读取 commit 响应失败：{e}")))?
            .to_bytes();

        let text = String::from_utf8_lossy(&resp_bytes).to_string();
        if !status.is_success() {
            return Err(Error::Connect(format!(
                "libpod commit 失败（HTTP {status}）：{text}"
            )));
        }

        let v: Value = serde_json::from_str(&text)
            .map_err(|e| Error::Connect(format!("解析 libpod commit 响应失败：{e}：{text}")))?;
        v.get("Id")
            .or_else(|| v.get("id"))
            .and_then(|id| id.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| Error::Connect(format!("libpod commit 响应缺少 Id：{text}")))
    }
}

/// URL 编码（仅容器名，实际多为 [a-z0-9_-]）。
fn urlencoding(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' => c.to_string(),
            _ => {
                let mut out = String::new();
                for b in c.to_string().bytes() {
                    out.push_str(&format!("%{:02X}", b));
                }
                out
            }
        })
        .collect()
}

/// 构造 keep-id 容器创建 body（Docker-compat 形状 + libpod namespaces 扩展）。
///
/// 入参沿用 bollard create 的等价字段：镜像/命令/环境/标签/HostConfig
/// （init、mounts、network、ports）。
#[allow(clippy::too_many_arguments)]
pub fn keep_id_create_body(
    name: &str,
    image: &str,
    cmd: Vec<String>,
    env: Vec<String>,
    labels: std::collections::HashMap<String, String>,
    mounts: Vec<Value>,
    network_mode: Option<String>,
    exposed_ports: Option<std::collections::HashMap<String, std::collections::HashMap<(), ()>>>,
    port_bindings: Option<Value>,
    working_dir: Option<String>,
    // 容器默认用户（PID 1 与 podman exec 的默认身份）；None = "0:0"（root）。
    // 新模型恒传 "<uid>:<gid>"——server 直接以该用户运行（无 root、无 su）
    default_user: Option<&str>,
    // keep-id 用户命名空间；false = 无 userns（容器内 uid 落宿主 subuid 段）
    keep_id: bool,
    // 设备直通：裸设备 "host:container[:perms]" 列表（SpecGenerator devices 的 Path）
    devices: Vec<String>,
    // GPU 透传："all" 或设备名 / "device=<uuid>"；经 nvidia.com/gpu=<值> CDI 引用注入
    // 设备节点。None = 无 GPU
    gpu: Option<&str>,
    // PID 命名空间模式（"host" 等）；None = private。非 private 时禁用 init
    pid: Option<&str>,
    // 安全选项（"label=disable" / "apparmor=unconfined" / "seccomp=..." 原始串）；
    // 解析进 SpecGenerator 对应字段
    security_opts: Vec<String>,
) -> Value {
    // libpod SpecGenerator 的 env 是 map[string]string（Docker compat 才是数组）
    let mut env_map = serde_json::Map::new();
    for kv in &env {
        if let Some((k, v)) = kv.split_once('=') {
            env_map.insert(k.to_string(), Value::String(v.to_string()));
        }
    }
    // libpod SpecGenerator 的 mount 格式：type/source/destination/options
    // （Docker 式 Type/Source/Target/ReadOnly 会导致
    // "container directory cannot be empty"，实测）
    let podman_mounts: Vec<Value> = mounts
        .iter()
        .map(|m| {
            let typ = m.get("Type").and_then(|v| v.as_str()).unwrap_or("bind").to_lowercase();
            let source = m.get("Source").cloned().unwrap_or(Value::Null);
            let target = m.get("Target").cloned().unwrap_or(Value::Null);
            let ro = m.get("ReadOnly").and_then(|v| v.as_bool()).unwrap_or(false);
            let options = if ro {
                json!(["ro"])
            } else {
                json!([])
            };
            json!({
                "type": typ,
                "source": source,
                "destination": target,
                "options": options,
            })
        })
        .collect();
    // libpod SpecGenerator 的端口字段是 `portmappings`（扁平 []PortMapping，
    // host_port/container_port 为**数字** uint16）；入参沿用 Docker PortBinding
    // 嵌套形状（{"PORT/PROTO": [{HostIp, HostPort}]，bollard serde 标签为
    // **PascalCase**，HostPort 为字符串），在此归一化——字段名/形状不对时
    // libpod 静默忽略（曾按小写键取值 → host_port 全落空 → podman 随机分配，
    // 2026-08-27 socket 实测）。
    let portmappings: Vec<Value> = match port_bindings {
        Some(pb) => pb
            .as_object()
            .into_iter()
            .flatten()
            .flat_map(|(key, val)| {
                let (port, proto) = key.split_once('/').unwrap_or((key, "tcp"));
                let container_port = port.parse::<u16>().unwrap_or(0);
                val.as_array().into_iter().flatten().map(move |b| {
                    let host_port = b.get("HostPort").and_then(|v| {
                        v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                    }).unwrap_or(0);
                    json!({
                        "host_ip": b.get("HostIp").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                        "host_port": host_port,
                        "container_port": container_port,
                        "protocol": proto,
                    })
                })
            })
            .collect(),
        None => Vec::new(),
    };

    // 设备直通：SpecGenerator 的 devices 字段是 []spec.LinuxDevice，其 Path 既能是
    // 裸设备串（"host:container[:perms]"）也能是 CDI 引用（"nvidia.com/gpu=all"）——
    // podman CLI 的 --device 与 --gpus 都归一化成此（见 FillOutSpecGen：
    // --gpus 逐值拼 "nvidia.com/gpu=<值>" 追加进 devices）。GPU 由此经 CDI 注入设备节点。
    let mut device_list: Vec<Value> = Vec::new();
    for d in &devices {
        if !d.trim().is_empty() {
            device_list.push(json!({ "path": d.trim() }));
        }
    }
    if let Some(gpu) = gpu.map(str::trim).filter(|g| !g.is_empty()) {
        device_list.push(json!({ "path": format!("nvidia.com/gpu={gpu}") }));
    }

    // PID 命名空间：SpecGenerator 的 pidns.nsmode。默认即 private（省略该字段），
    // 非 private（host 等）时 PID 1 是宿主 init，无法再注入 catatonit——init 必须关闭。
    let pid_mode = pid
        .map(str::trim)
        .filter(|p| !p.is_empty() && *p != "private")
        .map(str::to_string);
    let init_enabled = pid_mode.is_none();

    // 安全选项：解析 "key=value" 串进 SpecGenerator 对应字段（对齐 podman CLI
    // --security-opt 的映射）：apparmor→apparmor_profile、label→selinux_opts、
    // seccomp→seccomp_profile_path。其余（mask/unmask 等）暂忽略。
    let mut apparmor_profile: Option<String> = None;
    let mut selinux_opts: Vec<String> = Vec::new();
    let mut seccomp_profile_path: Option<String> = None;
    for opt in &security_opts {
        let (key, val) = match opt.split_once('=') {
            Some(kv) => kv,
            None => continue,
        };
        match key {
            "apparmor" => apparmor_profile = Some(val.to_string()),
            "label" => selinux_opts.push(val.to_string()),
            "seccomp" => seccomp_profile_path = Some(val.to_string()),
            _ => {}
        }
    }

    let mut body = json!({
        "name": name,
        "image": image,
        // libpod SpecGenerator 用 "command"（Docker compat 才是 "cmd"）
        "command": cmd,
        // 新模型传 "<uid>:<gid>"：server 直接以该用户运行（无 root、无 su）；
        // None 保留 "0:0" 仅作无身份场景的回退（create_with_config 已禁止）
        "user": default_user.unwrap_or("0:0"),
        "env": env_map,
        "labels": labels,
        "hostname": name,
        // catatonit = PID 1；PID 命名空间非 private 时禁用（无法注入 init）
        "init": init_enabled,
        "mounts": podman_mounts,
        "network_mode": network_mode,       // Some("host") 或 null（bridge 默认）
        "exposed_ports": exposed_ports,
        "portmappings": portmappings,
        "working_dir": working_dir,
    });
    // libpod 专属：keep-id 用户命名空间。注意：字段放**顶层** userns
    // （实测 namespaces.userns 被忽略）。
    // 真实映射语义（实测文件属主，2026-08-07；/proc/self/uid_map 字面
    // 不代表最终属主）：容器 uid 1000（node）= 宿主登录用户（1000）；
    // 容器 uid 0（root）= 宿主 subuid 100000（容器文件系统属主，
    // **不是宿主默认用户**——root 写宿主 home 属主呈现 100000）
    if keep_id {
        body["userns"] = json!({ "nsmode": "keep-id" });
    }
    // 设备直通（裸设备 + GPU CDI 引用）；空则省略
    if !device_list.is_empty() {
        body["devices"] = json!(device_list);
    }
    // PID 命名空间（pid=host 等）
    if let Some(mode) = &pid_mode {
        body["pidns"] = json!({ "nsmode": mode });
    }
    // 安全选项（解析后的 SpecGenerator 字段）
    if let Some(profile) = &apparmor_profile {
        body["apparmor_profile"] = json!(profile);
    }
    if !selinux_opts.is_empty() {
        body["selinux_opts"] = json!(selinux_opts);
    }
    if let Some(path) = &seccomp_profile_path {
        body["seccomp_profile_path"] = json!(path);
    }
    body
}
