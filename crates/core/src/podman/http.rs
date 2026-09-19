//! podman unix-socket HTTP 传输（自研极简客户端）。
//!
//! 只封装我们用到的能力：
//! - JSON 请求/响应（list/inspect/create/remove 等全部 compat + libpod 端点）
//! - 原始字节流（`/containers/{id}/archive` 读文件 tar）
//! - 流式响应 + 可选 stdin（exec attach / logs / pull / events）
//! - Docker 非 tty 多路复用流 demux（8 字节头：[stream,0,0,0,size BE]）
//!
//! 版本协商：首连 `GET /version` 取 ApiVersion（如 "5.4.2"）→ compat/libpod
//! 路径统一加 `/v{major}.{minor}` 前缀（podman 实测无前缀的 libpod 路径 404）。

use std::path::PathBuf;
use std::pin::Pin;
use std::task::{Context, Poll};

use http_body::Body;
use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::Request;
use hyper_util::client::legacy::Client;
use hyper_util::rt::{TokioExecutor, TokioIo};
use serde_json::Value;
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

/// podman unix-socket HTTP 客户端。
pub(crate) struct HttpClient {
    full: Client<UnixConnector, Full<Bytes>>,
    api_prefix: String,
    /// 完整版本串（如 "5.4.2"）
    api_version: String,
}

impl HttpClient {
    /// 连接（复用 Podman::connect_fork_or_system_socket 的 socket 选择逻辑）
    /// 并协商 API 版本。
    pub async fn connect() -> Result<Self> {
        let socket_path = crate::podman::Podman::connect_fork_or_system_socket()?;
        let connector = UnixConnector {
            socket_path,
        };
        let full: Client<UnixConnector, Full<Bytes>> =
            Client::builder(TokioExecutor::new()).build(connector);

        // GET /version（无前缀）取 ApiVersion → /v{major}.{minor} 前缀
        let (status, body) = Self::raw_request(&full, "GET", "/version", None).await?;
        if status != 200 {
            return Err(Error::Connect(format!("/version 返回 {status}")));
        }
        let v: Value = serde_json::from_slice(&body)
            .map_err(|e| Error::Connect(format!("解析 /version 失败：{e}")))?;
        let api_version = v
            .get("ApiVersion")
            .or_else(|| v.get("api_version"))
            .and_then(|x| x.as_str())
            .unwrap_or("5.0.0")
            .to_string();
        let mut parts = api_version.split('.');
        let major = parts.next().unwrap_or("5");
        let minor = parts.next().unwrap_or("0");
        let api_prefix = format!("/v{major}.{minor}");
        tracing::debug!("podman ApiVersion：{api_version}（路径前缀 {api_prefix}）");

        Ok(Self {
            full,
            api_prefix,
            api_version,
        })
    }

    /// 完整 API 版本串（如 "5.4.2"）。
    pub fn api_version(&self) -> &str {
        &self.api_version
    }

    fn url(&self, path_and_query: &str) -> String {
        // 显式带 /v 前缀的路径原样使用；否则补协商好的版本前缀
        if path_and_query.starts_with("/v") {
            format!("http://podman{path_and_query}")
        } else {
            format!("http://podman{}{path_and_query}", self.api_prefix)
        }
    }

    /// 底层一次性请求（Full body）。
    async fn raw_request(
        client: &Client<UnixConnector, Full<Bytes>>,
        method: &str,
        path_and_query: &str,
        body: Option<Bytes>,
    ) -> Result<(u16, Bytes)> {
        let method_m = hyper::Method::from_bytes(method.as_bytes())
            .map_err(|e| Error::Connect(format!("非法 HTTP 方法 {method}：{e}")))?;
        let builder = Request::builder()
            .method(method_m)
            .uri(format!("http://podman{path_and_query}"))
            .header("content-type", "application/json");
        let req = match body {
            Some(b) => builder.body(Full::new(b)),
            None => builder.body(Full::default()),
        }
        .map_err(|e| Error::Connect(format!("构造请求失败：{e}")))?;
        let resp = client
            .request(req)
            .await
            .map_err(|e| Error::Connect(format!("{method} {path_and_query} 请求失败：{e}")))?;
        let status = resp.status().as_u16();
        let bytes = resp
            .into_body()
            .collect()
            .await
            .map_err(|e| Error::Connect(format!("{method} {path_and_query} 读响应失败：{e}")))?
            .to_bytes();
        Ok((status, bytes))
    }

    /// 请求并返回 (status, body bytes)。不抛非 2xx 错误（调用方自行判定，
    /// 404 常是合法语义：不存在/幂等删除）。
    pub async fn request_bytes(
        &self,
        method: &str,
        path_and_query: &str,
        body: Option<Bytes>,
    ) -> Result<(u16, Bytes)> {
        Self::raw_request(&self.full, method, path_and_query, body).await
    }

    /// JSON 请求：返回 (status, 解析后的 body；空 body → Value::Null)。
    pub async fn json(
        &self,
        method: &str,
        path_and_query: &str,
        body: Option<Value>,
    ) -> Result<(u16, Value)> {
        let bytes = body
            .map(|v| {
                serde_json::to_vec(&v)
                    .map(Bytes::from)
                    .map_err(|e| Error::Connect(format!("序列化请求体失败：{e}")))
            })
            .transpose()?;
        let (status, body) = self.request_bytes(method, path_and_query, bytes).await?;
        let value = if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body).unwrap_or(Value::Null)
        };
        Ok((status, value))
    }

    /// JSON 请求且要求 2xx：非 2xx 时以 body 内 `message`/`cause` 构造可读错误。
    pub async fn json_ok(
        &self,
        method: &str,
        path_and_query: &str,
        body: Option<Value>,
    ) -> Result<Value> {
        let (status, value) = self.json(method, path_and_query, body).await?;
        if (200..300).contains(&status) {
            return Ok(value);
        }
        Err(Error::Connect(format!(
            "{method} {path_and_query} 返回 {status}：{}",
            value
                .get("message")
                .or_else(|| value.get("cause"))
                .and_then(|m| m.as_str())
                .unwrap_or("(无错误详情)")
        )))
    }

    /// 打开一个流式响应（logs / pull / events / archive / exec start）。
    pub async fn open_stream(
        &self,
        method: &str,
        path_and_query: &str,
    ) -> Result<(u16, Incoming)> {
        let method_m = hyper::Method::from_bytes(method.as_bytes())
            .map_err(|e| Error::Connect(format!("非法 HTTP 方法 {method}：{e}")))?;
        let req = Request::builder()
            .method(method_m)
            .uri(self.url(path_and_query))
            .body(Full::default())
            .map_err(|e| Error::Connect(format!("构造流请求失败：{e}")))?;
        let resp = self
            .full
            .request(req)
            .await
            .map_err(|e| Error::Connect(format!("{method} {path_and_query} 流请求失败：{e}")))?;
        let status = resp.status().as_u16();
        Ok((status, resp.into_body()))
    }

    /// 发送 upgrade 请求（exec start 的 hijack 语义），返回升级后的裸双向 IO。
    pub async fn request_upgrade(
        &self,
        method: &str,
        path_and_query: &str,
    ) -> Result<(u16, hyper::upgrade::Upgraded)> {
        let method_m = hyper::Method::from_bytes(method.as_bytes())
            .map_err(|e| Error::Connect(format!("非法 HTTP 方法 {method}：{e}")))?;
        let req = Request::builder()
            .method(method_m)
            .uri(self.url(path_and_query))
            .header("connection", "upgrade")
            .header("upgrade", "tcp")
            .body(Full::default())
            .map_err(|e| Error::Connect(format!("构造升级请求失败：{e}")))?;
        let resp = self
            .full
            .request(req)
            .await
            .map_err(|e| Error::Connect(format!("{method} {path_and_query} 升级请求失败：{e}")))?;
        let status = resp.status().as_u16();
        let upgraded = hyper::upgrade::on(resp)
            .await
            .map_err(|e| Error::Connect(format!("连接升级失败：{e}")))?;
        Ok((status, upgraded))
    }

    /// 容器日志 / exec 非 tty 输出等 Docker 多路复用流的解包读取：
    /// 阻塞式读取整个流，按 8 字节帧头 demux，分别累积 stdout/stderr。
    pub async fn read_demux_stream(
        &self,
        method: &str,
        path_and_query: &str,
    ) -> Result<(String, String)> {
        let (status, body) = self.open_stream(method, path_and_query).await?;
        if !(200..300).contains(&status) {
            let bytes = body
                .collect()
                .await
                .map_err(|e| Error::Connect(format!("读流失败：{e}")))?
                .to_bytes();
            return Err(Error::Connect(format!(
                "{method} {path_and_query} 返回 {status}：{}",
                String::from_utf8_lossy(&bytes).trim()
            )));
        }
        let bytes = body
            .collect()
            .await
            .map_err(|e| Error::Connect(format!("读流失败：{e}")))?
            .to_bytes();
        let mut demux = Demuxer::default();
        demux.feed(&bytes);
        Ok(demux.finish())
    }
}

/// Docker 非 tty 流多路复用解包器（跨 chunk 安全）。
///
/// 同时支持两种消费方式：
/// - 分路累积（`finish()` → (stdout, stderr)）——exec_oneshot / 容器日志
/// - 按到达顺序的合并流增量（`take_merged_delta()`）——exec attach 桥接
#[derive(Default)]
pub(crate) struct Demuxer {
    buf: Vec<u8>,
    stdout: String,
    stderr: String,
    merged: Vec<u8>,
    yielded: usize,
}

impl Demuxer {
    /// 喂入一段原始字节，解出完整帧并累积。
    pub fn feed(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
        loop {
            if self.buf.len() < 8 {
                return;
            }
            let stream_kind = self.buf[0];
            let size = u32::from_be_bytes([
                self.buf[4], self.buf[5], self.buf[6], self.buf[7],
            ]) as usize;
            let end = 8 + size;
            if self.buf.len() < end {
                return;
            }
            let payload = &self.buf[8..end];
            match stream_kind {
                1 => {
                    self.stdout.push_str(&String::from_utf8_lossy(payload));
                    self.merged.extend_from_slice(payload);
                }
                2 => {
                    self.stderr.push_str(&String::from_utf8_lossy(payload));
                    self.merged.extend_from_slice(payload);
                }
                _ => {} // 0=stdin（不会出现在响应里）；tty 模式无帧头（不走此处）
            }
            self.buf.drain(..end);
        }
    }

    /// 取合并流中尚未被取走的部分（按到达顺序，stdout/stderr 交错）。
    pub fn take_merged_delta(&mut self) -> Vec<u8> {
        let delta = self.merged[self.yielded..].to_vec();
        self.yielded = self.merged.len();
        delta
    }

    /// 流结束：清掉残余（不完整帧头/尾部）并返回分路累积输出。
    pub fn finish(mut self) -> (String, String) {
        if self.yielded < self.merged.len() {
            let rest = String::from_utf8_lossy(&self.merged[self.yielded..]).into_owned();
            self.stdout.push_str(&rest);
            self.yielded = self.merged.len();
        }
        (self.stdout, self.stderr)
    }
}

/// Docker 多路复用流的增量 demux 包装（exec 非 tty attach）：
/// 把 Incoming 字节流解包为按到达顺序的 stdout/stderr 合并块流。
///
/// 当前未被使用（exec.rs 直接用 `Demuxer` 处理已读到的字节），保留供
/// 未来「exec attach 流式转发」路径使用——避免每加一次流式路径就重新实现
/// demux 循环（state machine + feed/take_merged_delta + finish）。
#[allow(dead_code)]
pub(crate) struct DemuxStream {
    body: Incoming,
    demux: Demuxer,
}

#[allow(dead_code)]
impl DemuxStream {
    pub fn new(body: Incoming) -> Self {
        Self {
            body,
            demux: Demuxer::default(),
        }
    }
}

#[allow(dead_code)]
impl futures::Stream for DemuxStream {
    type Item = Result<Vec<u8>>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            let delta = self.demux.take_merged_delta();
            if !delta.is_empty() {
                return Poll::Ready(Some(Ok(delta)));
            }
            match Pin::new(&mut self.body).poll_frame(cx) {
                Poll::Ready(Some(Ok(frame))) => {
                    let data = frame.data_ref().map(|d| d.as_ref()).unwrap_or(&[]);
                    self.demux.feed(data);
                }
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Some(Err(Error::Connect(format!(
                        "exec 输出流读取失败：{e}"
                    )))))
                }
                Poll::Ready(None) => {
                    let delta = self.demux.take_merged_delta();
                    if delta.is_empty() {
                        return Poll::Ready(None);
                    }
                    return Poll::Ready(Some(Ok(delta)));
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}
