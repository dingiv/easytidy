//! .desktop 文件生成（宿主导出应用图标）。
//!
//! 功能：
//! - 生成容器专用的 .desktop 文件（Exec = `<cli> open --container <name>`，
//!   经 CLI 垫片确保容器运行/等待 server 后再拉起 GUI——桌面快捷方式统一入口）
//! - 安装到 $XDG_DATA_HOME/applications/
//! - 支持自定义图标（使用发行版 logo 或默认）

use std::path::{Path, PathBuf};
use std::fs;
use crate::error::{Error, Result};

/// .desktop 文件模板。
const DESKTOP_ENTRY_TEMPLATE: &str = r#"[Desktop Entry]
Version=1.0
Type=Application
Name={title}
Exec={exec}
Icon={icon}
Terminal=false
Categories=System;ContainerManagement;
X-easytidy-container={name}
"#;

/// 生成 .desktop 文件内容。
///
/// 参数：
/// - name: 容器名
/// - title: 显示标题（默认 "easytidy <name>"）
/// - icon: 图标路径（默认使用发行版 logo 或 easytidy 图标）
/// - cli_path: 宿主 easytidy CLI 绝对路径（Exec/TryExec）
///
/// 返回 .desktop 文件内容字符串。
pub fn generate_desktop_entry(
    name: &str,
    title: Option<&str>,
    icon: Option<&str>,
    cli_path: &str,
) -> String {
    let default_title = format!("easytidy {}", name);
    let title = title.unwrap_or(&default_title);
    let icon = icon.unwrap_or("easytidy-container");
    let exec = format!("{} open --container {}", cli_path, name);

    DESKTOP_ENTRY_TEMPLATE
        .replace("{title}", title)
        .replace("{exec}", &exec)
        .replace("{icon}", icon)
        .replace("{name}", name)
}

/// 安装 .desktop 文件到用户应用目录。
///
/// 参数：
/// - name: 容器名
/// - title: 显示标题
/// - icon: 图标路径
/// - cli_path: 宿主 easytidy CLI 绝对路径
///
/// 返回安装后的 .desktop 文件路径。
pub fn install_desktop_entry(
    name: &str,
    title: Option<&str>,
    icon: Option<&str>,
    cli_path: &str,
) -> Result<PathBuf> {
    let content = generate_desktop_entry(name, title, icon, cli_path);

    // 确定目标路径
    let data_home = dirs::data_local_dir()
        .ok_or_else(|| Error::Config("无法确定 XDG_DATA_HOME".to_string()))?;

    let applications_dir = data_home.join("applications");
    fs::create_dir_all(&applications_dir)
        .map_err(|e| Error::Config(format!("创建 applications 目录失败：{e}")))?;

    let desktop_path = applications_dir.join(format!("easytidy-{}.desktop", name));

    // 写入文件
    fs::write(&desktop_path, content)
        .map_err(|e| Error::Config(format!("写入 .desktop 文件失败：{e}")))?;

    tracing::info!("安装 .desktop 文件：{:?}", desktop_path);

    Ok(desktop_path)
}

/// 卸载 .desktop 文件。
pub fn uninstall_desktop_entry(name: &str) -> Result<()> {
    let data_home = dirs::data_local_dir()
        .ok_or_else(|| Error::Config("无法确定 XDG_DATA_HOME".to_string()))?;

    let desktop_path = data_home
        .join("applications")
        .join(format!("easytidy-{}.desktop", name));

    if desktop_path.exists() {
        fs::remove_file(&desktop_path)
            .map_err(|e| Error::Config(format!("删除 .desktop 文件失败：{e}")))?;

        tracing::info!("卸载 .desktop 文件：{:?}", desktop_path);
    }

    Ok(())
}

// ============================================================================
// passthrough 导出（宿主侧）：把容器内应用导出为宿主 .desktop，
// Exec = <cli> --container <name> run -- <exec>，经 server socket 拉起应用。
// 风格对齐 distrobox 生成的 .desktop（TryExec / GenericName / Keywords /
// Actions=Remove 等——TryExec 缺失时部分桌面环境不显示入口，实测）。
// ============================================================================

/// 宿主 applications 目录（~/.local/share/applications/）
pub fn passthrough_dir() -> Result<PathBuf> {
    let data_home = dirs::data_local_dir()
        .ok_or_else(|| Error::Config("无法确定 XDG_DATA_HOME".to_string()))?;
    Ok(data_home.join("applications"))
}

/// 宿主图标缓存目录（~/.easytidy/icons/，集中管理；不存在则创建）
pub fn passthrough_icon_dir() -> Result<PathBuf> {
    crate::appdata::icons_dir()
}

/// 宿主桌面目录（`xdg-user-dir DESKTOP` 动态获取，兼容中文系统"桌面"；
/// 失败回退 ~/Desktop / ~/桌面 中存在者）。桌面路径可能不存在
/// （GNOME 默认无桌面图标扩展时用户仍可能有目录）。
pub fn desktop_dir() -> Option<PathBuf> {
    // xdg-user-dir 动态获取（Ubuntu/Debian 自带 xdg-utils）
    if let Ok(out) = std::process::Command::new("xdg-user-dir")
        .arg("DESKTOP")
        .output()
    {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !s.is_empty() && Path::new(&s).is_dir() {
                return Some(PathBuf::from(s));
            }
        }
    }
    let home = dirs::home_dir()?;
    [home.join("Desktop"), home.join("桌面")]
        .into_iter()
        .find(|cand| cand.is_dir())
}

/// 标记 GNOME 桌面 .desktop 为已信任（允许双击启动；仅桌面路径需要，
/// 应用菜单无需。非 GNOME 环境 gio 缺失时静默忽略）。
pub fn mark_desktop_trusted(path: &Path) {
    let _ = std::process::Command::new("gio")
        .args(["set", &path.to_string_lossy(), "metadata::trusted", "true"])
        .status();
}

/// passthrough 导出参数（宿主侧生成 .desktop 的全部输入）
pub struct PassthroughSpec {
    pub container: String,
    pub app_name: String,
    pub comment: Option<String>,
    pub categories: Option<String>,
    /// 清理后的容器内执行命令（已去 %U 等占位符）
    pub exec: String,
    /// 宿主图标绝对路径（已搬运到本地；空则不写 Icon 字段）
    pub icon: Option<String>,
    /// 容器内 .desktop 路径（应用标识，revoke/state 用）
    pub desktop_file: String,
    /// 宿主 easytidy CLI 绝对路径（Exec/TryExec）
    pub cli_path: String,
    /// StartupNotify（容器内 .desktop）
    pub startup_notify: bool,
    /// StartupWMClass（容器内 .desktop；Wayland 窗口匹配用）
    pub startup_wm_class: Option<String>,
}

/// 生成 distrobox 风格 .desktop 内容（TryExec/GenericName/Keywords/
/// Actions=Remove——TryExec 缺失时部分桌面环境不显示入口，实测）。
fn generate_passthrough_content(spec: &PassthroughSpec) -> String {
    let comment = spec.comment.as_deref().unwrap_or("easytidy passthrough");
    let categories = spec.categories.as_deref().unwrap_or("Application;Utility;");
    let mut content = format!(
        "[Desktop Entry]\n\
         Name={}\n\
         GenericName=easytidy {} - {}\n\
         Comment={comment}\n\
         Categories={categories}\n",
        spec.app_name, spec.container, spec.app_name,
    );
    // ⚠️ --container 是子命令级参数（`easytidy open --container <n> -- ...`），
    // 放顶层会报 "unexpected argument '--container'"（journalctl 实测）。
    // open = 垫片入口：确保容器运行 + 等 server socket 就绪，再经 PTY 转发命令
    // （阻塞至应用退出，退出码透传——同原 `run` 路径）。
    content.push_str(&format!(
        "Exec={} open --container {} -- {}\n",
        spec.cli_path, spec.container, spec.exec,
    ));
    if let Some(icon) = spec.icon.as_ref() {
        content.push_str(&format!("Icon={icon}\n"));
    }
    if spec.startup_notify {
        content.push_str("StartupNotify=true\n");
    }
    if let Some(wm) = spec.startup_wm_class.as_ref() {
        content.push_str(&format!("StartupWMClass={wm}\n"));
    }
    content.push_str(&format!(
        "Keywords=easytidy;{};\n\
         NoDisplay=false\n\
         Terminal=false\n\
         TryExec={}\n\
         Type=Application\n\
         Actions=Remove;\n\
         \n\
         [Desktop Action Remove]\n\
         Name=Remove {} from system\n\
         Exec={} unexport --container {} --desktop-file {}\n",
        spec.container,
        spec.cli_path,
        spec.app_name,
        spec.cli_path,
        spec.container,
        spec.desktop_file,
    ));
    content.push_str(&format!(
        "X-easytidy-pt=1\n\
         X-easytidy-container={}\n\
         X-easytidy-app={}\n",
        spec.container, spec.desktop_file,
    ));
    content
}

/// easytidy 品牌图标（256×256 PNG，内嵌；与 GUI 应用图标/水印同源——
/// crates/gui/icons/easytidy256x256.png 的拷贝，core 无法跨 crate 引用）。
const EASYTIDY_ICON_PNG: &[u8] = include_bytes!("../assets/easytidy.png");

/// 确保 easytidy 品牌图标存在（~/.easytidy/icons/easytidy-gui.png）
/// 返回图标绝对路径；写入失败返回 None（不影响快捷方式导出）。
///
/// 历史遗留：旧版本写 SVG 线条图标（easytidy-gui.svg，v0.1 设计）——
/// 已导出的 .desktop 若仍指向它则图标失效/显示旧设计，顺手清理。
pub fn ensure_gui_icon() -> Option<String> {
    let dir = crate::appdata::icons_dir().ok()?;
    let path = dir.join("easytidy-gui.png");
    if !path.exists() && std::fs::write(&path, EASYTIDY_ICON_PNG).is_err() {
        return None;
    }
    // 清理旧版 SVG（同目录同名 .svg；新导出一律指向 PNG）
    let legacy_svg = dir.join("easytidy-gui.svg");
    if legacy_svg.exists() {
        let _ = std::fs::remove_file(&legacy_svg);
    }
    Some(path.to_string_lossy().into_owned())
}

/// GUI 入口 .desktop 内容（纯函数，可单测）。
///
/// Exec = `<cli> open --container <name>`——经 CLI 垫片确保容器运行、
/// 等待 server socket 就绪，再按配置 silent_boot 决定是否拉起 Worker GUI
/// （直接冷启动 380MB GUI 是点击慢的根源，垫片先行秒级保活）。
pub fn generate_gui_entry_content(container: &str, cli_path: &str) -> String {
    let mut content = format!(
        "[Desktop Entry]\n\
         Name=easytidy {container}\n\
         GenericName=Container GUI for {container}\n\
         Comment=Open {container} management interface\n\
         Categories=System;Utility;ContainerManagement;\n\
         Exec={cli_path} open --container {container}\n",
    );
    if let Some(icon) = ensure_gui_icon() {
        content.push_str(&format!("Icon={icon}\n"));
    }
    content.push_str(&format!(
        "Keywords=easytidy;{container};\n\
         NoDisplay=false\n\
         Terminal=false\n\
         TryExec={cli_path}\n\
         Type=Application\n\
         X-easytidy-gui=1\n\
         X-easytidy-container={container}\n",
    ));
    content
}

/// 导出本容器的 GUI 管理界面桌面快捷方式（distrobox "进入容器"入口的
/// easytidy 变体：Exec 经 CLI 垫片拉起 Worker 管理窗口）。
///
/// - 应用菜单：~/.local/share/applications/easytidy-gui-<container>.desktop
/// - 桌面图标：桌面路径同名文件 + chmod +x + `gio metadata::trusted`
///   （GNOME 双击必需；`desktop_icon=true` 且桌面目录存在时）
/// - `cli_path`：宿主 easytidy CLI 绝对路径（Exec/TryExec；垫片入口）
///
/// 返回应用菜单路径。
pub fn write_gui_entry(container: &str, cli_path: &str, desktop_icon: bool) -> Result<PathBuf> {
    let dir = passthrough_dir()?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| Error::Config(format!("创建 applications 目录失败：{e}")))?;

    let content = generate_gui_entry_content(container, cli_path);

    let file_name = format!("easytidy-gui-{container}.desktop");
    let menu_path = dir.join(&file_name);
    write_desktop_file(&menu_path, &content)?;
    tracing::info!("GUI 入口导出（菜单）：{menu_path:?}");

    if desktop_icon {
        if let Some(dd) = desktop_dir() {
            let desktop_path = dd.join(&file_name);
            if write_desktop_file(&desktop_path, &content).is_ok() {
                mark_desktop_trusted(&desktop_path);
                tracing::info!("GUI 入口导出（桌面）：{desktop_path:?}");
            } else {
                tracing::warn!("桌面图标写入失败（跳过）：{desktop_path:?}");
            }
        }
    }

    Ok(menu_path)
}

/// 升级迁移：把旧格式 Exec 的 easytidy .desktop 重写为 CLI 垫片格式。
///
/// 背景：旧容器入口 `Exec=<gui> --container <n>`（冷启动重型 GUI）、
/// 旧 passthrough `Exec=<cli> run --container <n> -- <cmd>` 均不享受垫片
/// 收益（保活 + 等 socket 就绪 + 容器缺失友好报错）。旧格式升级后仍可
/// 工作（run 与 GUI 直连都没删），迁移是补齐而非防坏——在 GUI 启动时
/// 调用（存量文件只在显式导出时生成，不会自己更新）。
///
/// 幂等：Exec 已含 ` open --container ` 的跳过。按标记/文件名识别三类：
/// - `X-easytidy-gui=1`（容器入口）→ `Exec=<cli> open --container <n>`
/// - `X-easytidy-pt=1`（passthrough，容器名取 `X-easytidy-container=`，
///   应用命令 = 旧 Exec 中 ` -- ` 之后的全部）→ 仅 run→open 替换
/// - 遗留 `easytidy-<n>.desktop`（无标记）→ `Exec=<cli> open --container <n>`
///
/// 扫 applications 目录 + 桌面副本两处（委托 [`migrate_dir`]）。
pub fn migrate_shortcuts(cli_path: &str) -> usize {
    let mut total = 0;
    if let Ok(d) = passthrough_dir() {
        total += migrate_dir(&d, cli_path);
    }
    if let Some(d) = desktop_dir() {
        total += migrate_dir(&d, cli_path);
    }
    total
}

/// 迁移单个目录中的 easytidy .desktop（逐文件尽力而为，失败 warn 不中断）。
/// 返回重写的文件数。
pub fn migrate_dir(dir: &Path, cli_path: &str) -> usize {
    let mut rewritten = 0;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with("easytidy-") || !name.ends_with(".desktop") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        // None = 无需迁移（已是新格式 / 无容器标记 / 无 Exec 行）
        let Some(new_content) = migrate_one_content(&content, cli_path) else {
            continue;
        };
        if let Err(e) = write_desktop_file(&path, &new_content) {
            tracing::warn!("迁移 .desktop 失败（{name}）：{e}");
        } else {
            tracing::info!("迁移 .desktop 到新垫片格式：{name}");
            rewritten += 1;
        }
    }
    rewritten
}

/// 单文件迁移（纯函数）：返回新内容；`None` = 无需迁移。
///
/// 只重写**首个**（主）Exec 行与 TryExec 行（`open --container` 为幂等信号），
/// 其余行原样保留——passthrough 的 `[Desktop Action Remove]` 里也有一行
/// `Exec=... unexport ...`（CLI 命令，语义不变），不能被误替换。
fn migrate_one_content(content: &str, cli_path: &str) -> Option<String> {
    // 幂等：Exec 已是垫片格式
    if content.lines().any(|l| l.starts_with("Exec=") && l.contains(" open --container ")) {
        return None;
    }
    let container = content
        .lines()
        .find_map(|l| l.strip_prefix("X-easytidy-container="))
        .unwrap_or_default();
    // 容器名缺失（不应发生）时不迁移
    if container.is_empty() {
        return None;
    }
    let exec_line = content.lines().find(|l| l.starts_with("Exec="))?;
    let new_exec_line = if content.contains("X-easytidy-pt=1") {
        // passthrough：保留应用命令尾巴（旧 Exec 中首个 ` -- ` 之后的全部）
        let rest = exec_line.trim_end().split_once(" -- ").map(|(_, r)| r).unwrap_or_default();
        if rest.is_empty() {
            format!("Exec={cli_path} open --container {container}")
        } else {
            format!("Exec={cli_path} open --container {container} -- {rest}")
        }
    } else {
        // 容器入口（X-easytidy-gui=1）与遗留（easytidy-<n>.desktop）：
        // 无应用命令，整行替换
        format!("Exec={cli_path} open --container {container}")
    };

    let mut out = Vec::with_capacity(content.lines().count());
    let mut replaced_exec = false;
    for line in content.lines() {
        if !replaced_exec && line.starts_with("Exec=") {
            out.push(new_exec_line.clone());
            replaced_exec = true;
        } else if line.starts_with("TryExec=") {
            // TryExec 指向可执行文件本身：旧容器入口指 GUI 二进制（已失效语义），
            // 统一指向 CLI
            out.push(format!("TryExec={cli_path}"));
        } else {
            out.push(line.to_string());
        }
    }
    Some(out.join("\n") + "\n")
}

/// 容器内 .desktop basename → 宿主文件名（sanitize，防路径穿越）
fn passthrough_file_name(container: &str, desktop_file: &str) -> String {
    let base = desktop_file
        .rsplit('/')
        .next()
        .unwrap_or("app")
        .trim_end_matches(".desktop");
    let safe: String = base
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    format!("easytidy-pt-{container}-{safe}.desktop")
}

/// 写入 .desktop 文件（+x 权限）。
fn write_desktop_file(path: &Path, content: &str) -> Result<()> {
    std::fs::write(path, content)
        .map_err(|e| Error::Config(format!("写入 .desktop 失败（{}）：{e}", path.display())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
    }
    Ok(())
}

/// 导出 passthrough（应用菜单 + 桌面图标）。
///
/// - 应用菜单：~/.local/share/applications/easytidy-pt-*.desktop（无需信任标记）
/// - 桌面图标：桌面路径同名文件 + chmod +x + `gio metadata::trusted`（GNOME
///   双击启动必需；`desktop_icon=true` 且桌面目录存在时）
///
/// 返回应用菜单路径。
pub fn write_passthrough(spec: &PassthroughSpec, desktop_icon: bool) -> Result<PathBuf> {
    let dir = passthrough_dir()?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| Error::Config(format!("创建 applications 目录失败：{e}")))?;

    let file_name = passthrough_file_name(&spec.container, &spec.desktop_file);
    let content = generate_passthrough_content(spec);
    let menu_path = dir.join(&file_name);
    write_desktop_file(&menu_path, &content)?;
    tracing::info!("passthrough 导出（菜单）：{menu_path:?}");

    // 桌面图标（可选；无桌面目录则跳过）
    if desktop_icon {
        if let Some(dd) = desktop_dir() {
            let desktop_path = dd.join(&file_name);
            if write_desktop_file(&desktop_path, &content).is_ok() {
                mark_desktop_trusted(&desktop_path);
                tracing::info!("passthrough 导出（桌面）：{desktop_path:?}");
            } else {
                tracing::warn!("桌面图标写入失败（跳过）：{desktop_path:?}");
            }
        }
    }

    Ok(menu_path)
}

/// 解析导出 .desktop 的字段值（按容器过滤）
fn parse_pt_value(content: &str, container: &str, key: &str) -> Option<String> {
    let in_container = content
        .lines()
        .find_map(|l| l.strip_prefix("X-easytidy-container="))
        .unwrap_or_default();
    if in_container != container {
        return None;
    }
    content
        .lines()
        .find_map(|l| l.strip_prefix(key))
        .map(|s| s.to_string())
}

/// 已导出的 passthrough 条目（含 .desktop 全文，供 GUI 展示）
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ExportedPassthrough {
    /// 容器内 .desktop 路径（X-easytidy-app 标记）
    pub desktop_file: String,
    /// .desktop 文件全文（read_to_string，非 UTF-8 以 lossy 兜底）
    pub content: String,
    /// 宿主文件路径
    pub path: PathBuf,
}

/// 枚举指定容器已导出的应用（返回容器内 .desktop 路径列表）
pub fn list_passthrough(container: &str) -> Result<Vec<String>> {
    Ok(list_passthrough_detailed(container)?
        .into_iter()
        .map(|e| e.desktop_file)
        .collect())
}

/// 枚举指定容器已导出的应用（含 .desktop 全文，GUI 展示用）
pub fn list_passthrough_detailed(container: &str) -> Result<Vec<ExportedPassthrough>> {
    let dir = passthrough_dir()?;
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(out);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with("easytidy-pt-") || !name.ends_with(".desktop") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        if !content.contains("X-easytidy-pt=1") {
            continue;
        }
        if let Some(app) = parse_pt_value(&content, container, "X-easytidy-app=") {
            if !app.is_empty() {
                out.push(ExportedPassthrough {
                    desktop_file: app,
                    content,
                    path,
                });
            }
        }
    }
    Ok(out)
}

/// 撤销某容器的 passthrough 导出（删除对应 .desktop；返回删除的路径）
pub fn remove_passthrough(container: &str, desktop_file: &str) -> Result<PathBuf> {
    let dir = passthrough_dir()?;
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Err(Error::Config("applications 目录不存在".to_string()));
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with("easytidy-pt-") || !name.ends_with(".desktop") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        if !content.contains("X-easytidy-pt=1") {
            continue;
        }
        if parse_pt_value(&content, container, "X-easytidy-container=").is_none() {
            continue;
        }
        if content.lines().any(|l| l == format!("X-easytidy-app={desktop_file}")) {
            std::fs::remove_file(&path)
                .map_err(|e| Error::Config(format!("删除 .desktop 失败：{e}")))?;
            // 同步删除桌面副本（若存在）
            if let Some(dd) = desktop_dir() {
                if let Some(file_name) = path.file_name() {
                    let desktop_copy = dd.join(file_name);
                    if desktop_copy.exists() {
                        let _ = std::fs::remove_file(&desktop_copy);
                    }
                }
            }
            tracing::info!("passthrough 撤销：{desktop_file} → {path:?}");
            return Ok(path);
        }
    }
    Err(Error::Config(format!("未找到已导出的应用：{desktop_file}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_generate_desktop_entry() {
        let content = generate_desktop_entry(
            "test-container",
            Some("Test Container"),
            Some("test-icon"),
            "/usr/bin/easytidy",
        );

        assert!(content.contains("Name=Test Container"));
        // 垫片格式：CLI 子命令级 --container（放顶层会 clap 报错，存量 bug 已修）
        assert!(content.contains("Exec=/usr/bin/easytidy open --container test-container"));
        assert!(content.contains("Icon=test-icon"));
        assert!(content.contains("X-easytidy-container=test-container"));
    }

    #[test]
    fn test_generate_desktop_entry_defaults() {
        let content = generate_desktop_entry("test", None, None, "/usr/bin/easytidy");

        assert!(content.contains("Name=easytidy test"));
        assert!(content.contains("Icon=easytidy-container"));
    }

    #[test]
    fn test_install_desktop_entry() {
        let temp_dir = TempDir::new().unwrap();

        // 模拟 XDG_DATA_HOME
        let target = temp_dir.path().join("applications/easytidy-test.desktop");

        // 手动创建目录并写入
        let content = generate_desktop_entry("test", None, None, "/usr/bin/easytidy");
        fs::create_dir_all(temp_dir.path().join("applications")).unwrap();
        fs::write(&target, content).unwrap();

        assert!(target.exists());
        let loaded = fs::read_to_string(&target).unwrap();
        assert!(loaded.contains("open --container test"));
    }

    #[test]
    fn test_generate_gui_entry_content_open_format() {
        let content = generate_gui_entry_content("chrome", "/usr/bin/easytidy");

        assert!(content.contains("Exec=/usr/bin/easytidy open --container chrome"));
        assert!(content.contains("TryExec=/usr/bin/easytidy"));
        // 迁移识别标记
        assert!(content.contains("X-easytidy-gui=1"));
        assert!(content.contains("X-easytidy-container=chrome"));
        // Exec 不再直连 GUI 二进制（X-easytidy-gui 标记含该子串，须按行判定）
        let exec_line = content.lines().find(|l| l.starts_with("Exec=")).unwrap();
        assert!(!exec_line.contains("easytidy-gui"));
    }

    #[test]
    fn test_generate_passthrough_content_open_exec() {
        let spec = PassthroughSpec {
            container: "chrome".to_string(),
            app_name: "Google Chrome".to_string(),
            comment: None,
            categories: None,
            exec: "google-chrome --incognito".to_string(),
            icon: None,
            desktop_file: "/usr/share/applications/google-chrome.desktop".to_string(),
            cli_path: "/usr/bin/easytidy".to_string(),
            startup_notify: false,
            startup_wm_class: None,
        };
        let content = generate_passthrough_content(&spec);

        // 垫片入口：open + 应用命令尾巴保留
        assert!(content.contains("Exec=/usr/bin/easytidy open --container chrome -- google-chrome --incognito"));
        // Remove action 的 unexport 行不变
        assert!(content.contains("Exec=/usr/bin/easytidy unexport --container chrome --desktop-file /usr/share/applications/google-chrome.desktop"));
        assert!(content.contains("X-easytidy-pt=1"));
    }

    #[test]
    fn test_migrate_one_content_gui_entry() {
        let old = "[Desktop Entry]\nName=easytidy chrome\nExec=/path/gui --container chrome\nTryExec=/path/gui\nX-easytidy-gui=1\nX-easytidy-container=chrome\n";
        let new = migrate_one_content(old, "/usr/bin/easytidy").unwrap();
        assert!(new.contains("Exec=/usr/bin/easytidy open --container chrome"));
        assert!(new.contains("TryExec=/usr/bin/easytidy"));
        // 幂等：新内容不再迁移
        assert!(migrate_one_content(&new, "/usr/bin/easytidy").is_none());
    }

    #[test]
    fn test_migrate_one_content_passthrough_keeps_remove_action() {
        let old = "[Desktop Entry]\nName=App\nExec=/usr/bin/easytidy run --container c -- google-chrome\nTryExec=/usr/bin/easytidy\nX-easytidy-pt=1\nX-easytidy-container=c\nX-easytidy-app=app.desktop\n[Desktop Action Remove]\nName=Remove\nExec=/usr/bin/easytidy unexport --container c --desktop-file app.desktop\n";
        let new = migrate_one_content(old, "/usr/bin/easytidy").unwrap();
        // 主 Exec 改 open + 命令尾巴保留
        assert!(new.contains("Exec=/usr/bin/easytidy open --container c -- google-chrome"));
        // Remove action 的 unexport 行不被误替换
        assert!(new.contains("Exec=/usr/bin/easytidy unexport --container c --desktop-file app.desktop"));
        // 恰好一行 open Exec
        assert_eq!(new.lines().filter(|l| l.contains("open --container")).count(), 1);
    }

    #[test]
    fn test_migrate_one_content_missing_container_noop() {
        // 无 X-easytidy-container 标记 → 不迁移
        assert!(migrate_one_content(
            "[Desktop Entry]\nExec=whatever\n",
            "/usr/bin/easytidy"
        )
        .is_none());
    }

    #[test]
    fn test_migrate_dir_idempotent() {
        let tmp = TempDir::new().unwrap();
        let apps = tmp.path().join("applications");
        fs::create_dir_all(&apps).unwrap();

        // 旧格式容器入口
        let gui_file = apps.join("easytidy-gui-chrome.desktop");
        fs::write(
            &gui_file,
            "[Desktop Entry]\nExec=/path/gui --container chrome\nTryExec=/path/gui\nX-easytidy-gui=1\nX-easytidy-container=chrome\n",
        )
        .unwrap();
        // 旧格式 passthrough
        let pt_file = apps.join("easytidy-pt-chrome-google-chrome.desktop");
        fs::write(
            &pt_file,
            "[Desktop Entry]\nExec=/usr/bin/easytidy run --container chrome -- google-chrome\nX-easytidy-pt=1\nX-easytidy-container=chrome\n",
        )
        .unwrap();
        // 无关文件应被跳过
        let other = apps.join("nautilus.desktop");
        fs::write(&other, "[Desktop Entry]\nExec=nautilus\n").unwrap();

        let n1 = migrate_dir(&apps, "/usr/bin/easytidy");
        assert_eq!(n1, 2, "首次应重写 2 个 easytidy 文件");
        let gui = fs::read_to_string(&gui_file).unwrap();
        assert!(gui.contains("Exec=/usr/bin/easytidy open --container chrome"));
        let pt = fs::read_to_string(&pt_file).unwrap();
        assert!(pt.contains("open --container chrome -- google-chrome"));
        // 无关文件未被改动
        assert_eq!(fs::read_to_string(&other).unwrap(), "[Desktop Entry]\nExec=nautilus\n");

        // 幂等：二次调用零重写
        let n2 = migrate_dir(&apps, "/usr/bin/easytidy");
        assert_eq!(n2, 0, "已是新格式时不应再重写");
    }
}
