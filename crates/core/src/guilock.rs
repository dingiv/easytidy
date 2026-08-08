//! GUI 进程锁（防多实例）。
//!
//! - 主 GUI（中心化模式）：`$XDG_RUNTIME_DIR/easytidy/gui.lock`
//! - 每个容器的 per-container GUI：`$XDG_RUNTIME_DIR/easytidy/gui-<name>.lock`
//!
//! XDG_RUNTIME_DIR 即 `/run/user/<uid>`（系统保证归当前用户、0700、登录清空
//! 语义）——直接写 `/run` 无权限，`/run/user/<uid>` 是规范位置，且与
//! server socket 同目录（`easytidy/`）。
//! 机制：flock 排他锁，进程持有 fd——正常退出/崩溃均自动释放，无陈旧锁问题。

use std::fs::{File, OpenOptions};
use std::path::PathBuf;

use crate::error::{Error, Result};

/// 运行目录（$XDG_RUNTIME_DIR/easytidy；不存在则创建）
fn runtime_dir() -> Result<PathBuf> {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").map_err(|_| Error::NoXdgRuntime)?;
    let dir = PathBuf::from(runtime_dir).join("easytidy");
    std::fs::create_dir_all(&dir)
        .map_err(|e| Error::Config(format!("创建运行目录失败：{e}")))?;
    Ok(dir)
}

/// 进程锁句柄：持有期间锁生效，drop（进程退出）自动释放
pub struct LockGuard {
    _file: File,
}

/// 获取 GUI 进程锁。
///
/// - `container: None` → 主 GUI（中心化模式），单实例
/// - `container: Some(name)` → 该容器的 per-container GUI，每容器单实例
///
/// 已有实例持有锁时返回 Err（调用方提示后退出）。
pub fn acquire(container: Option<&str>) -> Result<LockGuard> {
    let dir = runtime_dir()?;
    let name = match container {
        None => "gui.lock".to_string(),
        Some(c) => format!("gui-{c}.lock"),
    };
    let path = dir.join(name);
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| Error::Lock(format!("打开进程锁失败（{}）：{e}", path.display())))?;
    // flock 非阻塞尝试：失败 = 已有实例持有
    file.try_lock()
        .map_err(|_| Error::Lock(format!("已有实例在运行（{}）", path.display())))?;
    Ok(LockGuard { _file: file })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 锁互斥 + 释放后可重新获取 + 不同容器互不冲突。
    /// （临时 XDG_RUNTIME_DIR；同模式参照 lib.rs 既有测试）
    #[test]
    fn test_lock_exclusive_and_scoped() {
        let original = std::env::var("XDG_RUNTIME_DIR").ok();
        let temp = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_RUNTIME_DIR", temp.path());

        // 主 GUI 锁：第二次获取失败（flock 同文件两个 OFD 互斥）
        let l1 = acquire(None).expect("首次获取主锁应成功");
        assert!(acquire(None).is_err(), "重复获取主锁应失败");
        drop(l1);
        // 释放后可重新获取（进程退出自动释放语义）
        let _l2 = acquire(None).expect("释放后重新获取应成功");

        // 每容器实例锁：同容器互斥，不同容器互不冲突
        let _c1 = acquire(Some("chrome")).expect("获取 chrome 锁应成功");
        assert!(acquire(Some("chrome")).is_err(), "同容器重复获取应失败");
        let _c2 = acquire(Some("firefox")).expect("不同容器互不冲突");

        if let Some(val) = original {
            std::env::set_var("XDG_RUNTIME_DIR", val);
        }
    }
}
