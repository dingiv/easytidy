//! 文件系统命令：容器内 fs 代理 + 宿主↔容器传输（拖入上传/导出/复制）。

use serde::{Deserialize, Serialize};
use tracing::info;

use easytidy_protocol::ops::{
    FsCopy, FsCopyResp, FsList, FsListResp, FsMkdir, FsRead, FsReadResp, FsWrite,
};

use crate::commands::socket::send_json_request;
use crate::state::GuiSession;

// ============================================================================
// 文件系统命令
// ============================================================================

/// 文件系统条目（前端）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FsEntry {
    pub name: String,
    pub is_dir: bool,
    /// 符号链接（前端图标区分：链接目录/链接文件）
    pub is_symlink: bool,
    pub size: Option<u64>,
    pub mtime: i64,
}

/// 列出目录内容
#[tauri::command]
pub async fn fs_list(
    session: tauri::State<'_, Option<GuiSession>>,
    path: String,
) -> Result<Vec<FsEntry>, String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;

    let list_req = FsList { path };
    let resp = send_json_request(
        sess,
        "fs.list".to_string(),
        serde_json::to_value(list_req).map_err(|e| e.to_string())?,
    )
    .await
    .map_err(|e| e.to_string())?;

    let list_resp: FsListResp = serde_json::from_value(resp.payload)
        .map_err(|e| format!("解析 fs.list 响应失败：{}", e))?;

    let entries: Vec<FsEntry> = list_resp
        .entries
        .into_iter()
        .map(|e| FsEntry {
            name: e.name,
            is_dir: matches!(e.entry_type, easytidy_protocol::ops::FsEntryType::Dir),
            is_symlink: e.is_symlink,
            size: e.size,
            mtime: 0, // TODO: 从 FsStatResp 获取
        })
        .collect();

    Ok(entries)
}

/// 读取文件内容（返回 base64）
#[tauri::command]
pub async fn fs_read(
    session: tauri::State<'_, Option<GuiSession>>,
    path: String,
) -> Result<String, String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;

    let read_req = FsRead {
        path,
        offset: None,
        len: None,
    };
    let resp = send_json_request(
        sess,
        "fs.read".to_string(),
        serde_json::to_value(read_req).map_err(|e| e.to_string())?,
    )
    .await
    .map_err(|e| e.to_string())?;

    let read_resp: FsReadResp = serde_json::from_value(resp.payload)
        .map_err(|e| format!("解析 fs.read 响应失败：{}", e))?;

    Ok(read_resp.data_b64)
}

/// 写入文件内容（接收 base64）
#[tauri::command]
pub async fn fs_write(
    session: tauri::State<'_, Option<GuiSession>>,
    path: String,
    data_b64: String,
) -> Result<(), String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;

    let write_req = FsWrite {
        path,
        data_b64,
        offset: None, // 全量覆盖（编辑器保存）
    };
    send_json_request(
        sess,
        "fs.write".to_string(),
        serde_json::to_value(write_req).map_err(|e| e.to_string())?,
    )
    .await
    .map_err(|e| e.to_string())?;

    Ok(())
}

pub(crate) async fn fetch_container_file(sess: &GuiSession, path: &str) -> Result<Vec<u8>, String> {
    let read_req = FsRead {
        path: path.to_string(),
        offset: None,
        len: None,
    };
    let resp = send_json_request(
        sess,
        "fs.read".to_string(),
        serde_json::to_value(read_req).map_err(|e| e.to_string())?,
    )
    .await
    .map_err(|e| e.to_string())?;
    if let Some(err) = resp.err {
        return Err(format!("{} {}", err.code, err.message));
    }
    let read_resp: FsReadResp =
        serde_json::from_value(resp.payload).map_err(|e| format!("解析 fs.read 响应失败：{e}"))?;
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(read_resp.data_b64)
        .map_err(|e| format!("图标 base64 解码失败：{e}"))
}

/// 拖入进度（Channel 事件：每块上传后推送，前端进度条）
#[derive(Debug, Clone, serde::Serialize)]
pub struct ImportProgress {
    done: u64,
    total: u64,
    /// 当前上传的文件（容器目标路径）
    file: String,
}

/// 拖入内容探测（前端决定是否弹确认窗：含文件夹时询问）
#[derive(Debug, Clone, serde::Serialize)]
pub struct ImportInspectResp {
    has_dir: bool,
    total_bytes: u64,
    file_count: u64,
}

/// 递归收集宿主路径（文件夹 → 容器目录结构；文件 → 上传目标）
fn collect_host(
    src: &std::path::Path,
    dst_dir: &str,
    dirs: &mut Vec<String>,
    files: &mut Vec<(std::path::PathBuf, String)>,
) {
    let Some(name) = src.file_name().and_then(|n| n.to_str()) else {
        return;
    };
    let dst = format!("{dst_dir}/{name}");
    if src.is_dir() {
        dirs.push(dst.clone());
        if let Ok(iter) = std::fs::read_dir(src) {
            for entry in iter.flatten() {
                collect_host(&entry.path(), &dst, dirs, files);
            }
        }
    } else {
        files.push((src.to_path_buf(), dst));
    }
}

/// 拖入探测：是否含文件夹 + 总字节（前端据此弹确认窗）
#[tauri::command]
pub async fn import_inspect(paths: Vec<String>) -> Result<ImportInspectResp, String> {
    let mut has_dir = false;
    let mut total_bytes = 0u64;
    let mut file_count = 0u64;
    for p in &paths {
        let path = std::path::Path::new(p);
        if path.is_dir() {
            has_dir = true;
        }
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        collect_host(path, "/", &mut dirs, &mut files);
        for (src, _) in &files {
            if let Ok(m) = src.metadata() {
                total_bytes += m.len();
            }
            file_count += 1;
        }
    }
    Ok(ImportInspectResp {
        has_dir,
        total_bytes,
        file_count,
    })
}

/// 宿主文件/文件夹拖入容器（文件浏览器拖拽上传）。
///
/// 递归：文件夹 → 容器内建目录(fs.mkdir) + 逐文件上传；宿主侧 std::fs
/// 分块读（4MB/块,base64 后 <8MB 帧上限）→ 经共享 socket fs.write{offset}
/// 循环续写;每块后经 Channel 推送进度（前端进度条）。
#[tauri::command]
pub async fn import_files(
    session: tauri::State<'_, Option<GuiSession>>,
    paths: Vec<String>,
    dest_dir: String,
    on_progress: tauri::ipc::Channel<ImportProgress>,
) -> Result<(), String> {
    use base64::Engine as _;
    use std::io::{BufReader, Read};

    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    const CHUNK: usize = 4 * 1024 * 1024;

    // 递归收集：目录列表 + 文件列表
    let mut dirs: Vec<String> = Vec::new();
    let mut files: Vec<(std::path::PathBuf, String)> = Vec::new();
    for p in &paths {
        let path = std::path::Path::new(p);
        if !path.exists() {
            return Err(format!("路径不存在：{p}"));
        }
        collect_host(path, &dest_dir, &mut dirs, &mut files);
    }
    if files.is_empty() && dirs.is_empty() {
        return Err("没有可上传的内容".to_string());
    }
    let total: u64 = files
        .iter()
        .filter_map(|(src, _)| src.metadata().ok())
        .map(|m| m.len())
        .sum();

    // 先创建容器目录（create_dir_all 幂等）
    for d in &dirs {
        let resp = send_json_request(
            sess,
            "fs.mkdir".to_string(),
            serde_json::to_value(FsMkdir { path: d.clone() }).map_err(|e| e.to_string())?,
        )
        .await
        .map_err(|e| format!("创建目录失败（{d}）：{e}"))?;
        if let Some(err) = resp.err {
            return Err(format!("创建目录失败（{d}）：{} {}", err.code, err.message));
        }
    }

    // 逐文件分块上传 + 进度推送
    let mut done: u64 = 0;
    for (src, dest) in &files {
        let file = std::fs::File::open(src)
            .map_err(|e| format!("读取宿主文件失败（{}）：{e}", src.display()))?;
        let mut reader = BufReader::new(file);
        let mut offset: u64 = 0;
        let mut buf = vec![0u8; CHUNK];
        loop {
            let n = reader
                .read(&mut buf)
                .map_err(|e| format!("读取宿主文件失败（{}）：{e}", src.display()))?;
            if n == 0 {
                break;
            }
            let data_b64 = base64::engine::general_purpose::STANDARD.encode(&buf[..n]);
            let req = FsWrite {
                path: dest.clone(),
                data_b64,
                offset: Some(offset),
            };
            let resp = send_json_request(
                sess,
                "fs.write".to_string(),
                serde_json::to_value(req).map_err(|e| e.to_string())?,
            )
            .await
            .map_err(|e| format!("上传 {} 失败：{e}", dest))?;
            if let Some(err) = resp.err {
                return Err(format!("上传 {} 失败：{} {}", dest, err.code, err.message));
            }
            offset += n as u64;
            done += n as u64;
            let _ = on_progress.send(ImportProgress {
                done,
                total,
                file: dest.clone(),
            });
        }
        info!("上传完成：{src:?} → {dest}（{offset} 字节）");
    }
    Ok(())
}

/// 读取容器文件为 base64（分块组装,支持大文件——图片预览 data: URI）。
///
/// rootless + pasta 网络下容器 HTTP 端口宿主不可达（实测 Connection
/// refused）,静态托管对 GUI 不可用 → 走 socket 分块通道。
#[tauri::command]
pub async fn fetch_file_b64(
    session: tauri::State<'_, Option<GuiSession>>,
    path: String,
) -> Result<String, String> {
    use base64::Engine as _;
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    const CHUNK: u64 = 4 * 1024 * 1024;

    let mut out = String::new();
    let mut offset: u64 = 0;
    loop {
        let req = FsRead {
            path: path.clone(),
            offset: Some(offset),
            len: Some(CHUNK),
        };
        let resp = send_json_request(
            sess,
            "fs.read".to_string(),
            serde_json::to_value(req).map_err(|e| e.to_string())?,
        )
        .await
        .map_err(|e| e.to_string())?;
        if let Some(err) = resp.err {
            return Err(format!("{} {}", err.code, err.message));
        }
        let read_resp: FsReadResp = serde_json::from_value(resp.payload)
            .map_err(|e| format!("解析 fs.read 响应失败：{e}"))?;
        // 块小于请求长度 = 已读完
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&read_resp.data_b64)
            .map_err(|e| format!("base64 解码失败：{e}"))?
            .len() as u64;
        if bytes == 0 {
            break;
        }
        out.push_str(&read_resp.data_b64);
        offset += bytes;
        if bytes < CHUNK {
            break;
        }
    }
    Ok(out)
}

/// 导出容器文件到宿主（rfd 原生保存对话框;fs_read 分块下载）。
/// 容器→宿主方向（拖出不可行,右键菜单入口）。
#[tauri::command]
pub async fn export_file_dialog(
    session: tauri::State<'_, Option<GuiSession>>,
    path: String,
) -> Result<String, String> {
    use base64::Engine as _;
    use std::io::Write;
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;
    const CHUNK: u64 = 4 * 1024 * 1024;

    let name = path.rsplit('/').next().unwrap_or("file").to_string();
    // 保存对话框（阻塞;spawn_blocking 避免卡 async runtime）
    let dest = tokio::task::spawn_blocking(move || {
        rfd::FileDialog::new()
            .set_file_name(&name)
            .save_file()
            .map(|p| p.to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| format!("保存对话框失败：{e}"))?
    .ok_or_else(|| "已取消".to_string())?;

    let file =
        std::fs::File::create(&dest).map_err(|e| format!("创建宿主文件失败（{dest}）：{e}"))?;
    let mut writer = std::io::BufWriter::new(file);
    let mut offset: u64 = 0;
    loop {
        let req = FsRead {
            path: path.clone(),
            offset: Some(offset),
            len: Some(CHUNK),
        };
        let resp = send_json_request(
            sess,
            "fs.read".to_string(),
            serde_json::to_value(req).map_err(|e| e.to_string())?,
        )
        .await
        .map_err(|e| e.to_string())?;
        if let Some(err) = resp.err {
            return Err(format!("{} {}", err.code, err.message));
        }
        let read_resp: FsReadResp = serde_json::from_value(resp.payload)
            .map_err(|e| format!("解析 fs.read 响应失败：{e}"))?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&read_resp.data_b64)
            .map_err(|e| format!("base64 解码失败：{e}"))?;
        if bytes.is_empty() {
            break;
        }
        writer
            .write_all(&bytes)
            .map_err(|e| format!("写入宿主文件失败：{e}"))?;
        offset += bytes.len() as u64;
        if (bytes.len() as u64) < CHUNK {
            break;
        }
    }
    writer
        .flush()
        .map_err(|e| format!("写入宿主文件失败：{e}"))?;
    info!("导出完成：{path} → {dest}（{offset} 字节）");
    Ok(dest)
}

/// 容器内文件复制（右键复制/粘贴;代理到 server fs.copy）
#[tauri::command]
pub async fn fs_copy(
    session: tauri::State<'_, Option<GuiSession>>,
    src: String,
    dst: String,
) -> Result<u64, String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;

    let req = FsCopy { src, dst };
    let resp = send_json_request(
        sess,
        "fs.copy".to_string(),
        serde_json::to_value(req).map_err(|e| e.to_string())?,
    )
    .await
    .map_err(|e| e.to_string())?;
    if let Some(err) = resp.err {
        return Err(format!("{} {}", err.code, err.message));
    }
    let copy_resp: FsCopyResp =
        serde_json::from_value(resp.payload).map_err(|e| format!("解析 fs.copy 响应失败：{e}"))?;
    Ok(copy_resp.bytes_copied)
}
