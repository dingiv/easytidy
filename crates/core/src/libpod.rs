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
        // FIXME: ?? 为什么我们本地创建容器需要请求网络
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
    // 容器默认用户（PID 1 与 podman exec 的默认身份）；None = "0:0"（root，
    // 旧行为）。node 化容器传 "<uid>:<gid>"——node 用户经 init 镜像烘焙预置
    default_user: Option<&str>,
    // keep-id 用户命名空间（user_home 映射）；false = 无 userns（root 容器）
    keep_id: bool,
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
    let mut body = json!({
        "name": name,
        "image": image,
        // libpod SpecGenerator 用 "command"（Docker compat 才是 "cmd"）
        "command": cmd,
        // keep-id 下容器进程默认被设为 uid 1000（keep-id 值）——旧模型显式
        // "0:0" 让 server 以容器 root 运行（装包能力，应用经 su 降权）；
        // 新模型传 "<uid>:<gid>"：server 直接以 node 运行（用户已由 init
        // 镜像烘焙预置），root 需求走宿主 exec 通道
        "user": default_user.unwrap_or("0:0"),
        "env": env_map,
        "labels": labels,
        "hostname": name,
        "init": true,                       // catatonit = PID 1（与 bollard 路径一致）
        "mounts": podman_mounts,
        "network_mode": network_mode,       // Some("host") 或 null（bridge 默认）
        "exposed_ports": exposed_ports,
        "port_bindings": port_bindings,
        "working_dir": working_dir,
    });
    // libpod 专属：keep-id（user_home 映射容器）。注意：字段放**顶层** userns
    // （实测 namespaces.userns 被忽略）。
    // 真实映射语义（实测文件属主，2026-08-07；/proc/self/uid_map 字面
    // 不代表最终属主）：容器 uid 1000（node）= 宿主登录用户（1000）；
    // 容器 uid 0（root）= 宿主 subuid 100000（容器文件系统属主，
    // **不是宿主默认用户**——root 写宿主 home 属主呈现 100000）
    if keep_id {
        body["userns"] = json!({ "nsmode": "keep-id" });
    }
    body
}
