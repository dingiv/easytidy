//! 文件系统服务：list/stat/read/write/mkdir/copy。

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use base64::Engine;
use easytidy_protocol::{
    Frame, FsEntry, FsEntryType,
    FsCopy, FsCopyResp, FsMkdir, FsMkdirResp, FsList, FsListResp, FsRead, FsReadResp, FsStat, FsStatResp, FsWrite, FsWriteResp, Message, MsgKind,
};

/// Handle fs.list
pub(crate) async fn handle_fs_list(msg: Message) -> Result<Frame> {
    let req: FsList = serde_json::from_value(msg.payload)
        .context("Failed to parse FsList")?;

    let path = Path::new(&req.path);

    let mut entries = Vec::new();

    if let Ok(iter) = fs::read_dir(path) {
        for entry in iter.flatten() {
            let metadata = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };

            let name = entry.file_name().to_string_lossy().to_string();
            let entry_type = if metadata.is_dir() {
                FsEntryType::Dir
            } else if metadata.is_symlink() {
                FsEntryType::Symlink
            } else {
                FsEntryType::File
            };

            let size = if metadata.is_file() {
                Some(metadata.len())
            } else {
                None
            };

            let mode = Some(format!("{:o}", metadata.permissions().mode() & 0o777));

            entries.push(FsEntry {
                name,
                entry_type,
                size,
                mode,
            });
        }
    }

    Ok(Frame::Json(Message {
        id: msg.id,
        kind: MsgKind::Resp,
        op: "fs.list".to_string(),
        payload: serde_json::to_value(FsListResp { entries })?,
        err: None,
    }))
}

/// Handle fs.stat
pub(crate) async fn handle_fs_stat(msg: Message) -> Result<Frame> {
    let req: FsStat = serde_json::from_value(msg.payload)
        .context("Failed to parse FsStat")?;

    let path = Path::new(&req.path);

    let metadata = fs::metadata(path)
        .with_context(|| format!("Failed to stat: {}", req.path))?;

    let entry_type = if metadata.is_dir() {
        FsEntryType::Dir
    } else if metadata.is_symlink() {
        FsEntryType::Symlink
    } else {
        FsEntryType::File
    };

    let size = if metadata.is_file() {
        Some(metadata.len())
    } else {
        None
    };

    let mode = Some(format!("{:o}", metadata.permissions().mode() & 0o777));

    let mtime = metadata.modified()
        .unwrap_or(SystemTime::UNIX_EPOCH)
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs() as i64;

    let atime = metadata.accessed()
        .unwrap_or(SystemTime::UNIX_EPOCH)
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs() as i64;

    let ctime = metadata.created()
        .unwrap_or(SystemTime::UNIX_EPOCH)
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs() as i64;

    Ok(Frame::Json(Message {
        id: msg.id,
        kind: MsgKind::Resp,
        op: "fs.stat".to_string(),
        payload: serde_json::to_value(FsStatResp {
            entry: FsEntry {
                name: req.path.clone(),
                entry_type,
                size,
                mode,
            },
            mtime,
            atime,
            ctime,
        })?,
        err: None,
    }))
}

/// Handle fs.read
pub(crate) async fn handle_fs_read(msg: Message) -> Result<Frame> {
    let req: FsRead = serde_json::from_value(msg.payload)
        .context("Failed to parse FsRead")?;

    let path = Path::new(&req.path);

    // ⚠️ 大文件分块读（导出到宿主）：offset 时 seek 读取而非全量读后切片
    //（旧实现全量 fs::read 再切片——分块循环会重复读整个文件）
    let offset = req.offset.unwrap_or(0);
    let data = if offset > 0 {
        use std::io::{Read as _, Seek, SeekFrom};
        let mut file = fs::File::open(path)
            .with_context(|| format!("Failed to open file: {}", req.path))?;
        file.seek(SeekFrom::Start(offset))
            .with_context(|| format!("Failed to seek: {}", req.path))?;
        let mut buf = Vec::new();
        let len = req.len.unwrap_or(0) as usize;
        if len > 0 {
            buf.resize(len, 0);
            let n = file
                .read(&mut buf)
                .with_context(|| format!("Failed to read: {}", req.path))?;
            buf.truncate(n);
        } else {
            file.read_to_end(&mut buf)
                .with_context(|| format!("Failed to read: {}", req.path))?;
        }
        buf
    } else {
        let data = fs::read(path)
            .with_context(|| format!("Failed to read file: {}", req.path))?;
        let len = req.len.unwrap_or(data.len() as u64) as usize;
        let end = std::cmp::min(len, data.len());
        data[..end].to_vec()
    };

    let data_b64 = base64::engine::general_purpose::STANDARD.encode(data);

    Ok(Frame::Json(Message {
        id: msg.id,
        kind: MsgKind::Resp,
        op: "fs.read".to_string(),
        payload: serde_json::to_value(FsReadResp { data_b64 })?,
        err: None,
    }))
}

/// Handle fs.write
pub(crate) async fn handle_fs_write(msg: Message) -> Result<Frame> {
    let req: FsWrite = serde_json::from_value(msg.payload)
        .context("Failed to parse FsWrite")?;

    let data = base64::engine::general_purpose::STANDARD.decode(&req.data_b64)
        .context("Failed to decode base64 data")?;
    let bytes = data.len() as u64;

    match req.offset {
        // 分块上传续写（宿主→容器拖入的大文件分块写）
        Some(offset) => {
            use std::io::{Seek, SeekFrom, Write};
            let mut opts = std::fs::OpenOptions::new();
            opts.create(true).write(true);
            // 首块（offset=0）截断重建，续写不截断（分块上传语义）
            if offset == 0 {
                opts.truncate(true);
            }
            let mut file = opts
                .open(&req.path)
                .with_context(|| format!("Failed to open file for write: {}", req.path))?;
            file.seek(SeekFrom::Start(offset))
                .with_context(|| format!("Failed to seek: {}", req.path))?;
            file.write_all(&data)
                .with_context(|| format!("Failed to write chunk: {}", req.path))?;
        }
        None => {
            fs::write(&req.path, &data)
                .with_context(|| format!("Failed to write file: {}", req.path))?;
        }
    }

    Ok(Frame::Json(Message {
        id: msg.id,
        kind: MsgKind::Resp,
        op: "fs.write".to_string(),
        payload: serde_json::to_value(FsWriteResp {
            bytes_written: bytes,
        })?,
        err: None,
    }))
}

/// Handle fs.mkdir（文件夹拖入上传时递归建目录）
pub(crate) async fn handle_fs_mkdir(msg: Message) -> Result<Frame> {
    let req: FsMkdir = serde_json::from_value(msg.payload)
        .context("Failed to parse FsMkdir")?;

    fs::create_dir_all(&req.path)
        .with_context(|| format!("Failed to create directory: {}", req.path))?;

    Ok(Frame::Json(Message {
        id: msg.id,
        kind: MsgKind::Resp,
        op: "fs.mkdir".to_string(),
        payload: serde_json::to_value(FsMkdirResp)?,
        err: None,
    }))
}

/// Handle fs.copy（容器内文件复制：右键复制/粘贴菜单，server 直接 fs::copy）
pub(crate) async fn handle_fs_copy(msg: Message) -> Result<Frame> {
    let req: FsCopy = serde_json::from_value(msg.payload)
        .context("Failed to parse FsCopy")?;

    let bytes = fs::copy(&req.src, &req.dst)
        .with_context(|| format!("Failed to copy {} → {}", req.src, req.dst))?;

    Ok(Frame::Json(Message {
        id: msg.id,
        kind: MsgKind::Resp,
        op: "fs.copy".to_string(),
        payload: serde_json::to_value(FsCopyResp { bytes_copied: bytes })?,
        err: None,
    }))
}
