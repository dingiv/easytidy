//! libpod 扩展端点（经 podman unix socket 的 raw HTTP，共享 [`HttpClient`]）。
//!
//! 背景：Docker compat 端点不支持 podman 特有的 `--userns=keep-id`
//! （容器 uid = 宿主 uid 真对齐）。keep-id 仅在 libpod 端点可用：
//! `POST /libpod/containers/create` 的 `namespaces.userns.nsmode = "keep-id"`。
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

use serde_json::Value;

use crate::error::{Error, Result};
use crate::podman::http::HttpClient;
use hyper::body::Bytes;

/// libpod 客户端（仅覆盖我们用到的端点）。
///
/// libpod 端点需要 API 版本前缀（`/v<ApiVersion>/libpod/...`，裸 `/libpod/...`
/// 返回 404——podman 实测）；ApiVersion 取自 `GET /version`。
pub struct Libpod {
    http: HttpClient,
    api_version: String,
}

impl Libpod {
    pub async fn new() -> Result<Self> {
        let http = HttpClient::connect().await?;
        let api_version = http.api_version().to_string();
        tracing::debug!("podman ApiVersion：{api_version}");
        Ok(Self { http, api_version })
    }

    /// POST /v<version>/libpod/containers/create?name=<name>，body 为
    /// Docker-compat 形状 + libpod 扩展（namespaces.userns.nsmode）。返回容器 ID。
    ///
    /// `fast=true` 时附加 fork 扩展 query 参数 `easytidy_fast=true`（跳过 RW 层
    /// 创建的全树 chown；**仅在同映射直通安全时使用**，如快速重建的 commit 镜像）。
    pub async fn create_container(&self, name: &str, body: Value, fast: bool) -> Result<String> {
        // 最终请求体落日志（pretty JSON；排障对照 libpod SpecGenerator 字段）
        tracing::info!(
            "libpod create 请求（容器 {name}）：\n{}",
            serde_json::to_string_pretty(&body).unwrap_or_default()
        );
        let fast_param = if fast { "&easytidy_fast=true" } else { "" };
        let path = format!(
            "/v{}/libpod/containers/create?name={}{}",
            self.api_version,
            urlencoding(name),
            fast_param
        );
        let body_bytes = serde_json::to_vec(&body)
            .map(Bytes::from)
            .map_err(|e| Error::Connect(format!("序列化 libpod create body 失败：{e}")))?;
        let (status, resp_bytes) = self
            .http
            .request_bytes("POST", &path, Some(body_bytes))
            .await
            .map_err(|e| Error::Connect(format!("libpod create 请求失败：{e}")))?;
        let text = String::from_utf8_lossy(&resp_bytes).to_string();
        if !(200..300).contains(&status) {
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

    /// POST `/v<version>/libpod/commit?container=<name>&repo=<repo>&[tag=<tag>]&squash=<bool>[&changes=…]`
    ///
    /// 把容器当前文件系统打成镜像（squash 与否由调用方决定）。
    ///
    /// - `squash`：
    ///   - `true` 等价 `podman commit --squash`：把多层合并为**单层**,镜像体积更小,
    ///     不再叠加源容器原有历史层。作为"环境快照"的**默认**语义(fork 后镜像层干净)。
    ///   - `false` 普通 commit：保留源容器的**分层历史**（体积 = 源镜像层 + 增量层）。
    /// - `repo` 与 `tag` **分开传**：实测 libpod commit 的 `repo` 参数不允许含 `:`,
    ///   podman 会在其内部按 `<repo>:latest` 解析,遇到已有 `:` 的 repo 会触发
    ///   `parsing reference "<repo>:<tag>:latest": invalid reference format` 500。
    ///   tag 必须走独立 `tag=` query 参数（podman 5.4.2 验证过）。
    ///   `tag=None` → podman 走默认 tag（latest）。
    /// - `message`(OCI 镜像 history 的注释字段)进 image history,便于以后
    ///   `podman inspect` 看见来源备注。**注意:OCI 格式(/libpod/commit)用
    ///   `message`;docker 格式(/commit,已废弃)用 `comment`**——本端点传
    ///   `comment` 会触发 500 "messages are only compatible with the docker
    ///   image format (-f docker)"。
    /// - `changes`（libpod commit 的 schema 字段，**复数**）：多次同名 query 参数
    ///   累积为 `[]string`，每条一条 Dockerfile 指令。实测 `change=` 单数无效
    ///   （podman handler 只识别 `changes` schema tag）。空切片不附加参数。
    ///
    /// 返回 commit 响应里的 `Id`(镜像 ID,与 `repo:tag` 解析到同一镜像)。
    #[allow(clippy::too_many_arguments)]
    pub async fn commit(
        &self,
        container_name: &str,
        repo: &str,
        tag: Option<&str>,
        squash: bool,
        message: &str,
        changes: &[&str],
        fast: bool,
    ) -> Result<String> {
        let mut query = format!(
            "container={}&repo={}&squash={squash}&message={}",
            urlencoding(container_name),
            urlencoding(repo),
            urlencoding(message),
        );
        if let Some(t) = tag {
            query.push_str("&tag=");
            query.push_str(&urlencoding(t));
        }
        for c in changes {
            query.push_str("&changes=");
            query.push_str(&urlencoding(c));
        }
        // easytidy fork 扩展：跳过 commit 的 pause/unpause 与 squash 残余慢操作
        // （fork 专属参数；系统 podman IgnoreUnknownKeys 安全忽略，行为不变）
        if fast {
            query.push_str("&easytidy_fast=true");
        }
        let path = format!("/v{}/libpod/commit?{query}", self.api_version);
        let (status, resp_bytes) = self
            .http
            .request_bytes("POST", &path, None)
            .await
            .map_err(|e| Error::Connect(format!("libpod commit 请求失败：{e}")))?;
        let text = String::from_utf8_lossy(&resp_bytes).to_string();
        if !(200..300).contains(&status) {
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

    /// easytidy 原生重建端点（fork 扩展，docs/22-23 方案链路）：
    ///
    /// `POST /v<version>/libpod/containers/{old}/easytidy-rebuild`
    /// `?new_name=<tmp>&image=<新镜像引用>&easytidy_fast=true`
    ///
    /// 引擎在**服务端单进程内**原子完成：stop 旧容器 → commit RW 层为镜像 →
    /// 用请求 body（与 create 相同的 spec 形状，走完整 CreateContainer 语义：
    /// 运行时默认值 / rlimits / CompleteSpec / ExecuteCreate）创建替代容器
    /// （base = 该镜像）→ start → 删旧容器 → rename 为正式名。
    ///
    /// 与「宿主侧 commit + create 两步拼接」的本质区别：整条链在引擎进程内
    /// 顺序执行，commit 层的 parent 与新容器的挂载链由引擎自身保证一致——
    /// 不存在跨请求时序导致的中间层跳过（历史断链/数据丢失的根源）。
    ///
    /// 失败语义（端点保证）：删除旧容器**之前**的任何失败 → 旧容器被自动
    /// 重启（若调用时它在运行），新容器不会残留运行态；删除**之后**的失败
    /// 不可回滚——commit 镜像即数据兑底，错误信息带 `podman rename` 恢复指引。
    ///
    /// 返回新容器 ID（响应 `Id` 字段）。非 2xx 直接报出引擎错误原文。
    pub async fn easytidy_rebuild(
        &self,
        old_name: &str,
        new_name: &str,
        image_ref: &str,
        body: Value,
    ) -> Result<String> {
        let url = format!(
            "/v{}/libpod/containers/{}/easytidy-rebuild?new_name={}&image={}&easytidy_fast=true",
            self.api_version,
            urlencoding(old_name),
            urlencoding(new_name),
            urlencoding(image_ref)
        );
        let body_bytes = serde_json::to_vec(&body)
            .map(Bytes::from)
            .map_err(|e| Error::Connect(format!("序列化 rebuild body 失败：{e}")))?;
        let (status, resp) = self
            .http
            .request_bytes("POST", &url, Some(body_bytes))
            .await
            .map_err(|e| Error::Connect(format!("easytidy-rebuild 请求失败：{e}")))?;
        let text = String::from_utf8_lossy(&resp).to_string();
        if !(200..300).contains(&status) {
            return Err(Error::Connect(format!(
                "easytidy-rebuild 失败（HTTP {status}）：{}",
                text.trim()
            )));
        }
        let v: Value = serde_json::from_str(&text)
            .map_err(|e| Error::Connect(format!("解析 easytidy-rebuild 响应失败：{e}：{text}")))?;
        v.get("Id")
            .and_then(|x| x.as_str())
            .map(str::to_string)
            .ok_or_else(|| Error::Connect(format!("easytidy-rebuild 响应缺少 Id：{text}")))
    }
}

#[allow(clippy::too_many_arguments)]
pub fn keep_id_create_body(
    name: &str,
    hostname: &str,
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
    // keep-id 用户命名空间；false = 无 userns（容器内 uid 落宿主 subuid 段）。
    // 与 uidmaps/gidmaps 互斥：显式映射非空时此值被忽略（不写 keep-id userns）。
    keep_id: bool,
    // 显式 UID/GID 重映射（podman `--uidmap`/`--gidmap`，形状
    // `{container_id, host_id, length}`）。非空时覆盖 keep-id，写进 libpod 顶层
    // `uidmappings`/`gidmappings`（落 OCI `linux.uidMappings`/`gidMappings`）。
    uidmaps: Vec<crate::models::IdMapping>,
    gidmaps: Vec<crate::models::IdMapping>,
    // 设备直通：裸设备 "host:container[:perms]" 列表（SpecGenerator devices 的 Path）
    devices: Vec<String>,
    // GPU 透传：
    // - gpu_nvidia：NVIDIA 走 CDI `nvidia.com/gpu=all`（宿主 nvidia.yaml 可解析）
    // - amd_gpu_devices：AMD 已探测的裸设备串（"host:container"，见
    //   `env::host::detect_amd_gpu_devices`）；空 = 无 AMD 透传
    // NVIDIA 专属 env 由 `inject_passthrough` 负责（create 前已注入 config.env）。
    gpu_nvidia: bool,
    amd_gpu_devices: Vec<String>,
    // PID 命名空间模式（"host" 等）；None = private。非 private 时禁用 init
    pid: Option<&str>,
    // 额外选项（"label=disable" / "apparmor=unconfined" / "seccomp=..." 原始串）；
    // 解析进 SpecGenerator 对应字段
    extra_opts: Vec<String>,
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
            let typ = m
                .get("Type")
                .and_then(|v| v.as_str())
                .unwrap_or("bind")
                .to_lowercase();
            let source = m.get("Source").cloned().unwrap_or(Value::Null);
            let target = m.get("Target").cloned().unwrap_or(Value::Null);
            let ro = m.get("ReadOnly").and_then(|v| v.as_bool()).unwrap_or(false);
            let options = if ro {
                serde_json::json!(["ro"])
            } else {
                serde_json::json!([])
            };
            serde_json::json!({
                "type": typ,
                "source": source,
                "destination": target,
                "options": options,
            })
        })
        .collect();
    // libpod SpecGenerator 的端口字段是 `portmappings`（扁平 []PortMapping，
    // host_port/container_port 为**数字** uint16）；入参沿用 Docker PortBinding
    // 嵌套形状（{"PORT/PROTO": [{HostIp, HostPort}]，compat serde 标签为
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
                    serde_json::json!({
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
    // podman CLI 的 --device 与 --gpus 都归一化成此。
    // NVIDIA → CDI 引用；AMD → 裸设备（无 amd CDI spec，见 env::host 探测）。
    let mut device_list: Vec<Value> = Vec::new();
    let mut raw_devices: Vec<String> = devices;
    raw_devices.extend(amd_gpu_devices);
    for d in &raw_devices {
        let trimmed = d.trim();
        if !trimmed.is_empty() && !device_list.iter().any(|v| v["path"] == trimmed) {
            device_list.push(serde_json::json!({ "path": trimmed }));
        }
    }
    if gpu_nvidia {
        device_list.push(serde_json::json!({ "path": "nvidia.com/gpu=all" }));
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
    for opt in &extra_opts {
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

    let mut body = serde_json::json!({
        "name": name,
        "image": image,
        // libpod SpecGenerator 用 "command"（Docker compat 才是 "cmd"）
        "command": cmd,
        // 新模型传 "<uid>:<gid>"：server 直接以该用户运行（无 root、无 su）；
        // None 保留 "0:0" 仅作无身份场景的回退（create_with_config 已禁止）
        "user": default_user.unwrap_or("0:0"),
        "env": env_map,
        "labels": labels,
        "hostname": hostname,
        // catatonit = PID 1；PID 命名空间非 private 时禁用（无法注入 init）
        "init": init_enabled,
        "mounts": podman_mounts,
        // libpod 原生字段 `netns.nsmode`，不是 Docker compat 的 `network_mode`
        // （后者 libpod REST API 静默忽略 → 默认走 pasta）
        "netns": match network_mode {
            Some(mode) => serde_json::json!({ "nsmode": mode }),
            None => Value::Null,
        },
        "exposed_ports": exposed_ports,
        "portmappings": portmappings,
        "working_dir": working_dir,
    });
    // 清空镜像 ENTRYPOINT：libpod 语义下 `command` 是追加到 ENTRYPOINT 之后的参数
    // （非「替换」），镜像若设 ENTRYPOINT（如 mysql/postgres/redis 的 `bash -c`、
    // 各类带 ENTRYPOINT 的镜像）会包住我们的 command → server 启动失败（参数被消费）。
    // 显式置空数组 → 镜像 ENTRYPOINT 不生效，`command` 即为 PID 1 的字面命令。
    body["entrypoint"] = serde_json::json!([]);
    // 用户命名空间：显式映射（uidmaps/gidmaps）与 keep-id **互斥**（podman 实测
    // `--uidmap` 与 `--userns` 不能同开）。显式映射非空 → 写 libpod 顶层
    // `uidmappings`/`gidmappings`（落 OCI `linux.uidMappings`），**不**写 keep-id
    // userns；否则 keep_id 开 → keep-id；再否则无 userns（默认 rootless）。
    // 真实映射语义（实测文件属主，2026-08-07；/proc/self/uid_map 字面
    // 不代表最终属主）：keep-id 下容器 uid 1000（node）= 宿主登录用户（1000）；
    // 容器 uid 0（root）= 宿主 subuid 100000（容器文件系统属主，
    // **不是宿主默认用户**——root 写宿主 home 属主呈现 100000）。
    if !uidmaps.is_empty() || !gidmaps.is_empty() {
        // libpod create body 顶层字段（实测：`userns.uidmaps` 无效、顶层 `uidmappings`
        // 生效并落 OCI linux.uidMappings）；形状须为 `{containerID, hostID, size}`
        // （PascalCase + size，非 container_id/length）。
        let to_api = |ms: &[crate::models::IdMapping]| -> Vec<Value> {
            ms.iter()
                .map(|m| {
                    serde_json::json!({ "containerID": m.container_id, "hostID": m.host_id, "size": m.length })
                })
                .collect()
        };
        if !uidmaps.is_empty() {
            body["uidmappings"] = serde_json::json!(to_api(&uidmaps));
        }
        if !gidmaps.is_empty() {
            body["gidmappings"] = serde_json::json!(to_api(&gidmaps));
        }
    } else if keep_id {
        body["userns"] = serde_json::json!({ "nsmode": "keep-id" });
    }
    // 设备直通（裸设备 + GPU CDI 引用）；空则省略
    if !device_list.is_empty() {
        body["devices"] = serde_json::json!(device_list);
    }
    // PID 命名空间（pid=host 等）
    if let Some(mode) = &pid_mode {
        body["pidns"] = serde_json::json!({ "nsmode": mode });
    }
    // 安全选项（解析后的 SpecGenerator 字段）
    if let Some(profile) = &apparmor_profile {
        body["apparmor_profile"] = serde_json::json!(profile);
    }
    if !selinux_opts.is_empty() {
        body["selinux_opts"] = serde_json::json!(selinux_opts);
    }
    if let Some(path) = &seccomp_profile_path {
        body["seccomp_profile_path"] = serde_json::json!(path);
    }
    body
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
