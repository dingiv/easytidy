//! 存储健康（docs/18 道路二）：rootless native overlay chown 慢诊断 + fuse-overlayfs 一键修复。
//!
//! 背景：rootless + native overlay 场景，首次从 commit 镜像建容器触发全栈校验
//! 扫描（~2s/GB，20GB 镜像 ≈ 44s）。修复 = storage.conf 配
//! `[storage.options.overlay] mount_program` 指向 fuse-overlayfs（属主翻译由
//! FUSE 挂载视图完成，磁盘文件从不改写，零 chown）。
//!
//! 诊断只读（`Podman::engine_info`（含宿主 storage.conf 探测）+ 二进制/设备探测）；
//! 修复只写一个文件（`$XDG_CONFIG_HOME/containers/storage.conf`）：备份 →
//! TOML 合并写 → 重诊断验证 → 可回滚。绝不触碰容器/镜像。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::podman::Podman;

/// 诊断结论
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// overlay + mount_program 已配置（= fuse-overlayfs 生效）→ 快速路径
    Ok,
    /// rootless + native overlay + fuse-overlayfs 已安装 → 建议一键修复
    Recommended,
    /// rootless + native overlay + fuse-overlayfs 未安装（或 /dev/fuse 缺失）
    NeedsInstall,
    /// 不适用（非 rootless / 非 overlay 驱动）
    NotApplicable,
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Verdict::Ok => "ok",
            Verdict::Recommended => "recommended",
            Verdict::NeedsInstall => "needs_install",
            Verdict::NotApplicable => "not_applicable",
        };
        f.write_str(s)
    }
}

/// 诊断报告
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnoseReport {
    pub verdict: Verdict,
    pub rootless: bool,
    pub storage_driver: Option<String>,
    /// storage.conf `[storage.options.overlay] mount_program`（None = native overlay）
    pub mount_program: Option<String>,
    /// fuse-overlayfs 二进制路径（None = 未找到）
    pub fuse_overlayfs: Option<String>,
    /// /dev/fuse 是否存在
    pub dev_fuse: bool,
    /// 人类可读摘要（CLI/GUI 直接展示）
    pub summary: String,
}

/// 结论判定（纯函数；输入全组合可单测）
pub fn verdict_from(
    rootless: bool,
    driver: Option<&str>,
    mount_program: Option<&str>,
    fuse_overlayfs_installed: bool,
) -> Verdict {
    if !rootless || driver != Some("overlay") {
        return Verdict::NotApplicable;
    }
    if mount_program.is_some() {
        Verdict::Ok
    } else if fuse_overlayfs_installed {
        Verdict::Recommended
    } else {
        Verdict::NeedsInstall
    }
}

/// 查找 fuse-overlayfs 二进制（PATH 扫描 + 常见路径兜底）
pub fn find_fuse_overlayfs() -> Option<String> {
    let path_var = std::env::var("PATH").unwrap_or_default();
    for dir in path_var.split(':') {
        if dir.is_empty() {
            continue;
        }
        let p = Path::new(dir).join("fuse-overlayfs");
        if p.is_file() {
            return Some(p.to_string_lossy().into_owned());
        }
    }
    for c in ["/usr/bin/fuse-overlayfs", "/usr/local/bin/fuse-overlayfs"] {
        if Path::new(c).is_file() {
            return Some(c.to_string());
        }
    }
    None
}

fn build_summary(
    verdict: Verdict,
    driver: &Option<String>,
    mount_program: &Option<String>,
    fuse: &Option<String>,
    dev_fuse: bool,
) -> String {
    match verdict {
        Verdict::Ok => format!(
            "fuse-overlayfs 已生效（{}），建容器为快速路径",
            mount_program.as_deref().unwrap_or("未知")
        ),
        Verdict::Recommended => format!(
            "rootless + 原生 overlay：首次从镜像建容器会慢（~2s/GB）；fuse-overlayfs 已安装（{}），可一键切换",
            fuse.as_deref().unwrap_or("未知")
        ),
        Verdict::NeedsInstall => {
            if !dev_fuse {
                "rootless + 原生 overlay：首次从镜像建容器会慢（~2s/GB）；/dev/fuse 缺失且 fuse-overlayfs 未安装".into()
            } else {
                format!(
                    "rootless + 原生 overlay：首次从镜像建容器会慢（~2s/GB）；fuse-overlayfs 未安装（{driver:?} 驱动下需先安装）"
                )
            }
        }
        Verdict::NotApplicable => "不适用（非 rootless 或非 overlay 驱动）".into(),
    }
}

/// 诊断（只读：/info + 宿主探测）
pub async fn diagnose(podman: &Podman) -> Result<DiagnoseReport> {
    let info = podman.engine_info().await?;
    let fuse = find_fuse_overlayfs();
    let dev_fuse = Path::new("/dev/fuse").exists();
    let verdict = verdict_from(
        info.rootless,
        info.storage_driver.as_deref(),
        info.overlay_mount_program.as_deref(),
        fuse.is_some(),
    );
    let summary = build_summary(
        verdict,
        &info.storage_driver,
        &info.overlay_mount_program,
        &fuse,
        dev_fuse,
    );
    Ok(DiagnoseReport {
        verdict,
        rootless: info.rootless,
        storage_driver: info.storage_driver,
        mount_program: info.overlay_mount_program,
        fuse_overlayfs: fuse,
        dev_fuse,
        summary,
    })
}

// ============================================================================
// 修复（apply_fix / restore_backup / latest_backup）
// ============================================================================

/// 并发修复锁（docs/18 §5：同时只允许一个 apply）
static FIX_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 修复结果
#[derive(Debug, Clone, Serialize)]
pub struct ApplyReport {
    /// 原 storage.conf 备份（无原文件则 None）
    pub backup_path: Option<PathBuf>,
    /// 被写入的 storage.conf 路径
    pub config_path: PathBuf,
    /// 写后重诊断确认生效（常驻 daemon 未重启 = false）
    pub verified: bool,
    /// 需要手动重启常驻 podman daemon（socket-activated 形态 = false，无需重启）
    pub note_daemon: bool,
    /// 写入的 fuse-overlayfs 路径
    pub fuse_overlayfs_path: String,
}

/// storage.conf 路径：`$XDG_CONFIG_HOME/containers/storage.conf`
/// （未设 XDG_CONFIG_HOME 时回退 `~/.config/containers/storage.conf`；文件可能不存在）
pub fn storage_conf_path() -> Result<PathBuf> {
    let base = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .map(|h| h.join(".config"))
        .ok_or_else(|| Error::Config("无法确定 home 目录 / XDG_CONFIG_HOME".to_string()))?;
    Ok(base.join("containers").join("storage.conf"))
}

/// TOML 合并写：保留现有内容，仅确保 `[storage] driver="overlay"` 并加
/// `[storage.options.overlay] mount_program`（纯函数，单测入口）。
///
/// 冲突策略：现有 `driver` 非 overlay → **中止报错**，不擅改驱动。
pub fn merge_mount_program(existing: &str, mount_program: &str) -> Result<String> {
    let mut v: toml::Value = if existing.trim().is_empty() {
        toml::Value::Table(toml::map::Map::new())
    } else {
        toml::from_str(existing).map_err(|e| {
            Error::Config(format!("解析现有 storage.conf 失败（内容已损坏？）：{e}"))
        })?
    };
    let table = v
        .as_table_mut()
        .ok_or_else(|| Error::Config("storage.conf 不是 TOML 表".to_string()))?;
    let storage = table
        .entry("storage")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    let st = storage
        .as_table_mut()
        .ok_or_else(|| Error::Config("storage.conf [storage] 不是表".to_string()))?;
    if let Some(d) = st.get("driver").and_then(|d| d.as_str()) {
        if d != "overlay" {
            return Err(Error::Config(format!(
                "storage.conf 已配 driver={d}（非 overlay），中止——请人工处理"
            )));
        }
    }
    st.entry("driver")
        .or_insert_with(|| toml::Value::String("overlay".to_string()));
    let options = st
        .entry("options")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    let opts = options
        .as_table_mut()
        .ok_or_else(|| Error::Config("storage.conf [storage.options] 不是表".to_string()))?;
    let overlay = opts
        .entry("overlay")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    let ov = overlay.as_table_mut().ok_or_else(|| {
        Error::Config("storage.conf [storage.options.overlay] 不是表".to_string())
    })?;
    ov.insert(
        "mount_program".to_string(),
        toml::Value::String(mount_program.to_string()),
    );
    toml::to_string_pretty(&v).map_err(|e| Error::Config(format!("TOML 序列化失败：{e}")))
}

/// `systemctl --user is-active podman.socket` = active（socket-activated 形态：
/// 新连接自动生效，无需重启）
fn podman_socket_activated() -> bool {
    std::process::Command::new("systemctl")
        .args(["--user", "is-active", "podman.socket"])
        .output()
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "active")
        .unwrap_or(false)
}

/// 一键修复（诊断前置检查 → 备份 → TOML 合并写 → 重诊断验证 → daemon 提示）。
///
/// 仅 `verdict == Recommended`（rootless + overlay + 二进制可用 + /dev/fuse）时执行；
/// 其余结论返回明确错误（已生效 / 需安装 / 不适用）。
pub async fn apply_fix(podman: &Podman) -> Result<ApplyReport> {
    let _guard = FIX_LOCK.lock().await;

    let report = diagnose(podman).await?;
    match report.verdict {
        Verdict::Recommended => {}
        Verdict::Ok => {
            return Err(Error::Config(format!(
                "已生效，无需修复：{}",
                report.summary
            )));
        }
        Verdict::NeedsInstall => {
            return Err(Error::Config(format!(
                "fuse-overlayfs 未安装（或 /dev/fuse 缺失），无法修复：先安装（如 `sudo apt install fuse-overlayfs`）后再试。{}",
                report.summary
            )));
        }
        Verdict::NotApplicable => {
            return Err(Error::Config(format!("不适用：{}", report.summary)));
        }
    }
    let fuse = report.fuse_overlayfs.clone().ok_or_else(|| {
        Error::Config("fuse-overlayfs 路径缺失（Recommended 结论不应发生）".to_string())
    })?;

    let config_path = storage_conf_path()?;
    if let Some(dir) = config_path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| Error::Config(format!("创建配置目录失败（{}）：{e}", dir.display())))?;
    }

    // 备份（原文件存在才备份）
    let existing = std::fs::read_to_string(&config_path).unwrap_or_default();
    let backup_path = if !existing.trim().is_empty() {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let backup = config_path
            .file_name()
            .map(|f| config_path.with_file_name(format!("{}.bak-{ts}", f.to_string_lossy())));
        let backup = backup.ok_or_else(|| Error::Config("备份路径推导失败".to_string()))?;
        std::fs::copy(&config_path, &backup)
            .map_err(|e| Error::Config(format!("备份 storage.conf 失败：{e}")))?;
        tracing::info!("storage.conf 已备份：{}", backup.display());
        Some(backup)
    } else {
        None
    };

    // 合并写
    let new_content = merge_mount_program(&existing, &fuse)?;
    std::fs::write(&config_path, &new_content)
        .map_err(|e| Error::Config(format!("写入 storage.conf 失败：{e}")))?;
    tracing::info!(
        "storage.conf 已写入 mount_program={fuse}：{}",
        config_path.display()
    );

    // 验证：重诊断（socket-activated 下新连接即新 daemon，立即可见；
    // 常驻 daemon 下旧进程仍报旧配置 → verified=false + note_daemon）
    let recheck = diagnose(podman).await?;
    let verified =
        recheck.verdict == Verdict::Ok && recheck.mount_program.as_deref() == Some(fuse.as_str());

    Ok(ApplyReport {
        backup_path,
        config_path,
        verified,
        note_daemon: !podman_socket_activated(),
        fuse_overlayfs_path: fuse,
    })
}

/// 最近一份备份（`storage.conf.bak-<epoch>`，取最大时间戳）
pub fn latest_backup() -> Option<PathBuf> {
    let config = storage_conf_path().ok()?;
    let dir = config.parent()?;
    let prefix = format!("{}.bak-", config.file_name()?.to_str()?);
    let mut best: Option<(u128, PathBuf)> = None;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return None;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if let Some(ts) = name.strip_prefix(&prefix) {
            if let Ok(t) = ts.parse::<u128>() {
                if best.as_ref().is_none_or(|(b, _)| t > *b) {
                    best = Some((t, e.path()));
                }
            }
        }
    }
    best.map(|(_, p)| p)
}

/// 回滚：把备份拷回 storage.conf。返回被写入的原路径。
pub fn restore_backup(backup: &Path) -> Result<PathBuf> {
    let config = storage_conf_path()?;
    std::fs::copy(backup, &config)
        .map_err(|e| Error::Config(format!("恢复备份失败（{}）：{e}", backup.display())))?;
    tracing::info!("storage.conf 已回滚自 {}", backup.display());
    Ok(config)
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// verdict 全组合判定表
    #[test]
    fn test_verdict_table() {
        // 非 rootless → 一律不适用
        assert_eq!(
            verdict_from(false, Some("overlay"), None, true),
            Verdict::NotApplicable
        );
        assert_eq!(
            verdict_from(false, Some("overlay"), Some("/x"), true),
            Verdict::NotApplicable
        );
        // rootless 但非 overlay → 不适用
        assert_eq!(
            verdict_from(true, Some("vfs"), None, true),
            Verdict::NotApplicable
        );
        assert_eq!(verdict_from(true, None, None, true), Verdict::NotApplicable);
        // rootless + overlay + 已配 mount_program → Ok（无论二进制是否在）
        assert_eq!(
            verdict_from(
                true,
                Some("overlay"),
                Some("/usr/bin/fuse-overlayfs"),
                false
            ),
            Verdict::Ok
        );
        // rootless + overlay + native + 二进制已装 → Recommended
        assert_eq!(
            verdict_from(true, Some("overlay"), None, true),
            Verdict::Recommended
        );
        // rootless + overlay + native + 二进制未装 → NeedsInstall
        assert_eq!(
            verdict_from(true, Some("overlay"), None, false),
            Verdict::NeedsInstall
        );
    }

    #[test]
    fn test_merge_empty() {
        let out = merge_mount_program("", "/usr/bin/fuse-overlayfs").unwrap();
        let v: toml::Value = toml::from_str(&out).unwrap();
        assert_eq!(
            v.get("storage")
                .and_then(|s| s.get("driver"))
                .and_then(|d| d.as_str()),
            Some("overlay")
        );
        assert_eq!(
            v.get("storage")
                .and_then(|s| s.get("options"))
                .and_then(|o| o.get("overlay"))
                .and_then(|o| o.get("mount_program"))
                .and_then(|m| m.as_str()),
            Some("/usr/bin/fuse-overlayfs")
        );
    }

    #[test]
    fn test_merge_preserves_existing() {
        let existing = r#"
[storage]
  runroot = "/run/user/1000/libpod"
  graphroot = "/home/u/.local/share/containers/storage"

[storage.options]
  size = "10%G"

[engine]
  events_logger = "file"
"#;
        let out = merge_mount_program(existing, "/usr/bin/fuse-overlayfs").unwrap();
        let v: toml::Value = toml::from_str(&out).unwrap();
        // 原有内容保留
        assert_eq!(
            v.get("storage")
                .and_then(|s| s.get("runroot"))
                .and_then(|d| d.as_str()),
            Some("/run/user/1000/libpod")
        );
        assert_eq!(
            v.get("storage")
                .and_then(|s| s.get("options"))
                .and_then(|o| o.get("size"))
                .and_then(|d| d.as_str()),
            Some("10%G")
        );
        assert_eq!(
            v.get("engine")
                .and_then(|e| e.get("events_logger"))
                .and_then(|d| d.as_str()),
            Some("file")
        );
        // 新增 mount_program
        assert_eq!(
            v.get("storage")
                .and_then(|s| s.get("options"))
                .and_then(|o| o.get("overlay"))
                .and_then(|o| o.get("mount_program"))
                .and_then(|m| m.as_str()),
            Some("/usr/bin/fuse-overlayfs")
        );
    }

    #[test]
    fn test_merge_idempotent_value() {
        // 已有相同 mount_program → 合并后值不变（幂等）
        let existing = "[storage]\n  driver = \"overlay\"\n\n[storage.options.overlay]\n  mount_program = \"/usr/bin/fuse-overlayfs\"\n";
        let out = merge_mount_program(existing, "/usr/bin/fuse-overlayfs").unwrap();
        let v: toml::Value = toml::from_str(&out).unwrap();
        assert_eq!(
            v.get("storage")
                .and_then(|s| s.get("options"))
                .and_then(|o| o.get("overlay"))
                .and_then(|o| o.get("mount_program"))
                .and_then(|m| m.as_str()),
            Some("/usr/bin/fuse-overlayfs")
        );
    }

    #[test]
    fn test_merge_driver_conflict_aborts() {
        let existing = "[storage]\n  driver = \"vfs\"\n";
        let e = merge_mount_program(existing, "/usr/bin/fuse-overlayfs").unwrap_err();
        assert!(e.to_string().contains("vfs"), "应报冲突：{e}");
    }

    #[test]
    fn test_merge_invalid_toml_aborts() {
        let e = merge_mount_program("not [ valid toml", "/x").unwrap_err();
        assert!(e.to_string().contains("解析"), "应报解析失败：{e}");
    }
}
