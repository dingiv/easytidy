//! GUI 透传规则加载 + 注入（内容数据驱动）。
//!
//! `gui: true` 时引擎按宿主实时环境注入显示 env + 目录挂载 + keep-id。本模块把
//! 「注入什么」从 Rust 代码迁到资源文件 [`ASSETS_DIR::gui-passthrough.yaml`]：
//! - **dev** = `crates/gui/assets/gui-passthrough.yaml`（源码树，随源码提交）
//! - **prod** = `~/.easytidy/assets/gui-passthrough.yaml`（数据目录，首跑从内嵌
//!   默认播种、可被用户定制）
//!
//! 代码只负责「执行」：经 FileLoader 读取规则 → 展开宿主侧占位符 → 存在性/
//! 空值检查 → 幂等去重注入。占位符取值来自宿主 env（session 耦合的
//! DISPLAY / WAYLAND_DISPLAY / XDG_RUNTIME_DIR 运行时探测，避免硬编 session 值）。
//!
//! 占位符（host_path / container_path / env 值统一按宿主 env 展开，非容器侧上下
//! 文——GUI 透传把宿主显示环境桥进容器，两侧都是宿主值）：
//! `${HOME}` `${USER}` `${UID}` `${GID}` `${DISPLAY}` `${WAYLAND_DISPLAY}`
//! `${XDG_RUNTIME_DIR}`。展开后 host_path 为空（session 变量未设）→ 挂载跳过；
//! env 值为空 → env 跳过。
//!
//! XAUTHORITY 不在此处：路径含随机后缀（mutter-Xwaylandauth.<random> 等），由容器
//! 内 easytidy-server 启动时自动探测 `$XDG_RUNTIME_DIR` 下已知模式并覆盖进程 env
//! （见 `crates/server/src/setup.rs` `ensure_xauthority`）。

use std::collections::HashMap;
use std::path::Path;

use crate::models::{ContainerParams, MountConfig};

/// GUI 透传规则（`gui-passthrough.yaml` 反序列化形状）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GuiPassthroughRule {
    /// 注入的 env（`KEY=VALUE`，VALUE 可含占位符；空值跳过）
    #[serde(default)]
    pub env: Vec<String>,
    /// 注入的挂载映射（host_path / container_path 可含占位符）
    #[serde(default)]
    pub mounts: Vec<GuiPassthroughMount>,
    /// 是否强制开启 keep-id（GUI 应用需以宿主用户身份读写宿主挂载目录）
    #[serde(default)]
    pub keep_id: bool,
}

/// 规则中的单条挂载映射。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GuiPassthroughMount {
    pub host_path: String,
    pub container_path: String,
    #[serde(default)]
    pub read_only: bool,
    /// 宿主路径须存在才注入（false = 恒注入，如 X11 socket；true = 可选资源，
    /// 如字体/图标目录缺失时跳过）。展开后 host_path 为空（session 变量未设）
    /// 一律跳过，与本字段无关。
    #[serde(default)]
    pub require_exists: bool,
}

/// 加载 GUI 透传规则。
///
/// 优先经 FileLoader 的 `ASSETS_DIR` namespace 读取（dev 源码树 / prod 数据目录）；
/// prod 首跑若数据目录无此文件则从内嵌默认播种（用户可后续定制）。任何读取/解析
/// 失败都回退内嵌默认（构建期恒可解析），不阻断容器创建。
pub fn load_rule() -> GuiPassthroughRule {
    // 内嵌默认 = 源码树文件的编译期副本（与 dev 读取同一真相源）。
    const EMBEDDED: &str = include_str!("../../../gui/assets/gui-passthrough.yaml");

    if let Some(path) = easytidy_shared::loader!().resolve("ASSETS_DIR::gui-passthrough.yaml") {
        // prod 首跑播种：数据目录无文件时从内嵌默认写入（dev 指向已存在的源码树文件）。
        if !path.exists() {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&path, EMBEDDED);
        }
        if let Ok(text) = std::fs::read_to_string(&path) {
            match serde_yaml::from_str::<GuiPassthroughRule>(&text) {
                Ok(rule) => return rule,
                Err(e) => tracing::warn!(
                    "gui-passthrough 规则解析失败（{}）：{e}，回退内嵌默认",
                    path.display()
                ),
            }
        }
    }
    serde_yaml::from_str::<GuiPassthroughRule>(EMBEDDED)
        .unwrap_or_else(|e| panic!("内嵌 gui-passthrough.yaml 解析失败（构建期应可解析）：{e}"))
}

/// 执行 GUI 透传注入：按规则展开占位符 + 幂等去重，追加到 params.mounts / env。
///
/// 幂等：模板已声明的同 container_path 挂载 / 同 key env 跳过（以模板作者声明
/// 为准）。供 [`crate::env::host::inject_gui_passthrough`] 调用。
pub fn apply(params: &mut ContainerParams, env: &mut Vec<String>) {
    let rule = load_rule();
    let vars = host_vars();

    let existing_mount_targets: std::collections::HashSet<String> = params
        .mounts
        .iter()
        .map(|m| m.container_path.clone())
        .collect();
    let existing_env_keys: std::collections::HashSet<String> = env
        .iter()
        .filter_map(|kv| kv.split_once('=').map(|(k, _)| k.to_string()))
        .collect();

    // env：展开值；空值跳过（session 变量未设）；已声明同 key 跳过
    for entry in &rule.env {
        let Some((key, value)) = entry.split_once('=') else {
            continue;
        };
        if existing_env_keys.contains(key) {
            continue;
        }
        let expanded = expand_host_vars(value, &vars);
        if expanded.is_empty() {
            continue;
        }
        env.push(format!("{key}={expanded}"));
    }

    // mounts：两侧展开；host 路径为空（session 变量未设）跳过；require_exists 且
    // 宿主路径缺失跳过；已声明同 container_path 跳过
    for m in &rule.mounts {
        if existing_mount_targets.contains(&m.container_path) {
            continue;
        }
        let host_path = expand_host_vars(&m.host_path, &vars);
        let container_path = expand_host_vars(&m.container_path, &vars);
        if host_path.is_empty() || container_path.is_empty() {
            continue;
        }
        if m.require_exists && !Path::new(&host_path).exists() {
            continue;
        }
        params.mounts.push(MountConfig {
            host_path,
            container_path,
            read_only: m.read_only,
        });
    }

    if rule.keep_id {
        params.keep_id = true;
    }
}

/// 宿主侧占位符取值（session 耦合值从宿主 env 实时探测；未设 → 空串）。
fn host_vars() -> HashMap<String, String> {
    let mut v = HashMap::with_capacity(7);
    for key in [
        "HOME",
        "USER",
        "UID",
        "GID",
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "XDG_RUNTIME_DIR",
    ] {
        v.insert(key.to_string(), std::env::var(key).unwrap_or_default());
    }
    v
}

/// 展开路径/值串中的 `${VAR}`（按宿主侧 [`host_vars`] 取值）。
///
/// 无变量 → 原样；未知变量 / 未设 → 空串（调用方据「展开后空值跳过」处理，不
/// fail-fast——session 变量本就可能未设）；`$` 后非 `{` → 字面量；`${` 无闭合
/// `}` → 剩余整体作字面量。
fn expand_host_vars(s: &str, vars: &HashMap<String, String>) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            match s[i + 2..].find('}') {
                Some(rel_close) => {
                    let var = &s[i + 2..i + 2 + rel_close];
                    out.push_str(vars.get(var).map(String::as_str).unwrap_or(""));
                    i = i + 2 + rel_close + 1;
                }
                None => {
                    out.push_str(&s[i..]);
                    break;
                }
            }
        } else {
            let ch = s[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> HashMap<String, String> {
        let mut v = HashMap::new();
        v.insert("HOME".into(), "/home/div".into());
        v.insert("USER".into(), "div".into());
        v.insert("XDG_RUNTIME_DIR".into(), "/run/user/1000".into());
        v.insert("DISPLAY".into(), String::new()); // 未设
        v
    }

    #[test]
    fn expand_known_var() {
        let v = vars();
        assert_eq!(expand_host_vars("${HOME}/.local/share/fonts", &v), "/home/div/.local/share/fonts");
        assert_eq!(expand_host_vars("${XDG_RUNTIME_DIR}", &v), "/run/user/1000");
    }

    #[test]
    fn expand_unset_var_is_empty() {
        let v = vars();
        assert_eq!(expand_host_vars("${DISPLAY}", &v), "");
        assert_eq!(expand_host_vars("${HOME}/${DISPLAY}", &v), "/home/div/");
    }

    #[test]
    fn expand_no_var_passthrough() {
        let v = vars();
        assert_eq!(expand_host_vars("/tmp/.X11-unix", &v), "/tmp/.X11-unix");
        assert_eq!(
            expand_host_vars("/mnt/host:/usr/share", &v),
            "/mnt/host:/usr/share"
        );
    }

    #[test]
    fn expand_no_brace_and_unclosed_are_literal() {
        let v = vars();
        assert_eq!(expand_host_vars("$HOME/x", &v), "$HOME/x");
        assert_eq!(expand_host_vars("${HOME/x", &v), "${HOME/x");
    }

    /// 规则文件本身应可解析（锁定「assets 规则 = 合法 GuiPassthroughRule」契约）。
    #[test]
    fn embedded_rule_parses() {
        let rule = load_rule();
        assert!(rule.keep_id, "gui 透传应恒开 keep-id");
        // X11 socket 恒注入（require_exists=false）
        assert!(
            rule.mounts.iter().any(|m| m.host_path == "/tmp/.X11-unix" && !m.require_exists),
            "应含 X11 socket 且非 require_exists：{:?}",
            rule.mounts
        );
        // 字体为 require_exists
        assert!(
            rule.mounts.iter().any(|m| m.host_path == "/usr/share/fonts" && m.require_exists && m.read_only),
            "应含系统字体挂载（require_exists + 只读）：{:?}",
            rule.mounts
        );
        // XDG_DATA_DIRS 静态值
        assert!(
            rule.env.iter().any(|e| e.starts_with("XDG_DATA_DIRS=") && e.contains("/usr/share")),
            "应含 XDG_DATA_DIRS 系统默认：{:?}",
            rule.env
        );
        // XDG_RUNTIME_DIR 挂载两侧皆占位符
        assert!(
            rule.mounts.iter().any(|m| m.host_path == "${XDG_RUNTIME_DIR}" && m.container_path == "${XDG_RUNTIME_DIR}"),
            "应含 XDG_RUNTIME_DIR 挂载（两侧占位符）：{:?}",
            rule.mounts
        );
    }
}
