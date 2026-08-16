//! HTTP 静态文件服务（图片预览/大文件下载;动态端口经 server.info 查询）。


use anyhow::{Context, Result};
use tracing::{debug, info};

/// HTTP 静态文件服务端口（server.info 查询;0 = 未启用）
pub(crate) static HTTP_PORT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);

/// 启动 HTTP 静态文件服务（GET /fs/<url-encoded-path> → 读容器文件返回）。
///
/// 用途：GUI 图片预览（<img src="http://127.0.0.1:<port>/fs/<path>">）与
/// 大文件下载。动态端口（bind 0）避免冲突；host 网络下 GUI 直连 localhost。
/// 最小 HTTP/1.1 实现（只处理 GET /fs/），无第三方依赖。
pub(crate) async fn start_http_server() -> u16 {
    let Ok(listener) = tokio::net::TcpListener::bind("0.0.0.0:0").await else {
        return 0;
    };
    let Ok(addr) = listener.local_addr() else {
        return 0;
    };
    let port = addr.port();
    HTTP_PORT.store(port, std::sync::atomic::Ordering::SeqCst);
    info!("HTTP 静态服务已启动：127.0.0.1:{port}");

    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                continue;
            };
            tokio::spawn(async move {
                if let Err(e) = handle_http_request(stream).await {
                    debug!("HTTP 请求处理失败：{e}");
                }
            });
        }
    });
    port
}

/// 处理单个 HTTP 请求（GET /fs/<path>）
pub(crate) async fn handle_http_request(stream: tokio::net::TcpStream) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .await
        .context("读取请求行失败")?;
    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 2 || parts[0] != "GET" {
        return Ok(());
    }
    let target = parts[1];
    let Some(rest) = target.strip_prefix("/fs/") else {
        // 非 /fs/ 路径：404
        let mut w = reader.into_inner();
        let _ = w
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await;
        return Ok(());
    };
    // 忽略请求头
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 || line.trim().is_empty() {
            break;
        }
    }
    let mut writer = reader.into_inner();

    // percent-decode（最小实现：%XX 解码）
    let bytes = rest.as_bytes();
    let mut decoded: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("00");
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                decoded.push(v);
                i += 3;
                continue;
            }
        }
        decoded.push(bytes[i]);
        i += 1;
    }
    let path = String::from_utf8_lossy(&decoded).to_string();

    match tokio::fs::read(&path).await {
        Ok(data) => {
            let mime = mime_for_path(&path);
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                data.len()
            );
            let _ = writer.write_all(header.as_bytes()).await;
            let _ = writer.write_all(&data).await;
            let _ = writer.flush().await;
        }
        Err(_) => {
            let _ = writer
                .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await;
        }
    }
    Ok(())
}

/// 按扩展名推断 MIME（图片预览为主）
pub(crate) fn mime_for_path(path: &str) -> &'static str {
    let ext = path
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "txt" | "md" | "log" | "conf" => "text/plain; charset=utf-8",
        "json" => "application/json",
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css",
        "js" => "application/javascript",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}
