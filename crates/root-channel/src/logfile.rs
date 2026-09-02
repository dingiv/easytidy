//! root-channel 容器内文件日志。
//!
//! 所有模式（daemon / bootstrap / client）都写一个共享日志文件
//! `/run/easytidy/root-channel.log`，供宿主侧 `podman exec` 读取排查。
//!
//! 为什么需要文件日志：daemon 由 bootstrap 用 `setsid -f` 拉起，其 stdio
//! 被重定向到 /dev/null（`Stdio::null()`）——**daemon 的 stderr 完全不可见**。
//! 若 daemon 启动即崩（openpty / bind socket 失败），宿主侧只有"没反应"，
//! 无任何线索。文件日志让每次 exec 的进程都能追加可查诊断记录。
//!
//! ## 并发安全
//! bootstrap/client 每次 `podman exec` 都是新进程，可能同时写同一文件。
//! 用 `OpenOptions::append(true)`（O_APPEND）+ 每次写打开即关，保证单条
//! 日志原子追加、无跨进程 buffering 串扰。

use tracing_subscriber::fmt::MakeWriter;

/// 日志目录（容器内 tmpfs，容器存活期内有效；重启即清——诊断当次会话足够）。
pub const LOG_DIR: &str = "/run/easytidy";
/// 共享日志文件（所有模式追加写入）。
pub const LOG_FILE: &str = "/run/easytidy/root-channel.log";

/// 确保日志目录存在（失败静默——文件日志是 best-effort，不因路径问题崩进程）。
pub fn ensure_dir() {
    let _ = std::fs::create_dir_all(LOG_DIR);
}

/// 单行追加写入器（每次写打开-追加-关，跨进程安全）。
struct FileWriter;

impl std::io::Write for FileWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(LOG_FILE)?;
        f.write_all(buf)?;
        f.flush()?;
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// 一次写入同时落到共享文件与 stderr（可选）。
///
/// `to_stderr` 控制是否写 stderr：client 模式 stderr 会被 podman exec 捕获并
/// 桥进终端（残留日志污染）→ 关；daemon/bootstrap 保留 stderr 供 GUI 错误收集。
/// 文件始终写（daemon stderr=null）。
pub struct MultiWriter {
    file: FileWriter,
    stderr: Option<std::io::Stderr>,
}

impl std::io::Write for MultiWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let _ = self.file.write(buf);
        if let Some(s) = self.stderr.as_mut() {
            let _ = s.write(buf);
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        let _ = self.file.flush();
        if let Some(s) = self.stderr.as_mut() {
            let _ = s.flush();
        }
        Ok(())
    }
}

/// 同时写「stderr（可选）+ 共享日志文件」的 MakeWriter（具体类型，非 opaque，
/// 使 `for<'a> MakeWriter<'a>` 边界成立）。
pub struct MultiMakeWriter {
    to_stderr: bool,
}

impl<'a> MakeWriter<'a> for MultiMakeWriter {
    type Writer = MultiWriter;
    fn make_writer(&'a self) -> Self::Writer {
        MultiWriter {
            file: FileWriter,
            stderr: if self.to_stderr {
                Some(std::io::stderr())
            } else {
                None
            },
        }
    }
}

/// 返回「stderr（可选）+ 共享日志文件」的 writer。
pub fn layered_writer(to_stderr: bool) -> MultiMakeWriter {
    MultiMakeWriter { to_stderr }
}
