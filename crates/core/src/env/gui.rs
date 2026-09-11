//! GUI 透传规则加载 + 注入（内容数据驱动）。
//!
//! `gui_x11` / `gui_wayland` 任一开启时引擎按宿主实时环境注入显示 env + 目录挂载 +
//! keep-id（`shared` 段任一半开启即注入，`x11` / `wayland` 段按各自开关）。本模块把
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
//! XAUTHORITY：core 无条件注入稳定间接路径（[`XAUTHORITY_STABLE_PATH`]）
//! （不在 yaml env 段——播种副本会过时，且旧版 yaml 可能残留随机路径行，
//! core 注入时过滤覆盖）。真实 auth 文件含随机后缀（mutter-Xwaylandauth.<random>
//! 等）随会话轮换，由容器内 server 探到后维护该路径的软链（见
//! `crates/server/src/setup.rs` `ensure_xauthority`）。

use std::collections::HashMap;
use std::path::Path;

use crate::models::{ContainerParams, MountConfig};

/// GUI 直通规则（`gui-passthrough.yaml` 反序列化形状）。
///
/// 拆三段：**shared**（任一半开启即注入：keep-id / 字体图标 / XDG_DATA_DIRS /
/// $XDG_RUNTIME_DIR 挂载）+ **x11**（X11 应用半）+ **wayland**（Wayland 应用半）。
/// `deny_unknown_fields`：旧版扁平格式（顶层 env/mounts/keep_id）解析失败 →
/// load_rule 回退内嵌默认（不迁移）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuiPassthroughRule {
    /// 共享基建（gui_x11 或 gui_wayland 任一半开启即注入）
    #[serde(default)]
    pub shared: GuiPassthroughSection,
    /// X11 应用半（gui_x11=true）：DISPLAY + /tmp/.X11-unix（XAUTHORITY 由 core 恒注入）
    #[serde(default)]
    pub x11: GuiPassthroughSection,
    /// Wayland 应用半（gui_wayland=true）：WAYLAND_DISPLAY（socket 在 $XDG_RUNTIME_DIR）
    #[serde(default)]
    pub wayland: GuiPassthroughSection,
}

/// 规则中的一个直通段（env + 挂载 + keep-id 意图）。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct GuiPassthroughSection {
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

/// 容器内 XAUTHORITY 稳定间接路径（core 单一真相源）。
///
/// 真实 auth 文件（`$XDG_RUNTIME_DIR/mutter-Xwaylandauth.<random>` 等）随会话轮换
/// 且路径随机；容器内 server 启动时探测并维护 `{socket_dir}/xauthority` 软链
/// （socket 目录固定 /run/easytidy，见 server 入口）。容器 Config.Env 恒注入本值，
/// 所有入口（PTY / apps.launch / podman exec）统一。
pub const XAUTHORITY_STABLE_PATH: &str = "/run/easytidy/xauthority";

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
/// 按「共享 + X11 半 + Wayland 半」三段幂等注入：`shared` 在任一半开启时注入，
/// `x11`/`wayland` 按各自开关注入。
///
/// **意图驱动覆盖（2026-09-10 双半拆分）**：commit 快照会把容器 env 烘焙进镜像、
/// 重建继承，故 GUI 意图相关 env（DISPLAY / XAUTHORITY / WAYLAND_DISPLAY）必须按
/// 当前意图**覆盖**（非仅追加）：意图开 → 宿主实时值；意图关 → 置空（覆盖镜像里
/// 的旧值）。两半均关时不注入任何段，但仍执行覆盖清空。
/// 供 [`crate::env::host::inject_gui_passthrough`] 调用。
pub fn apply(params: &mut ContainerParams, env: &mut Vec<String>, x11: bool, wayland: bool) {
    let rule = load_rule();
    let vars = host_vars();

    let mut existing_mount_targets: std::collections::HashSet<String> = params
        .mounts
        .iter()
        .map(|m| m.container_path.clone())
        .collect();
    let mut existing_env_keys: std::collections::HashSet<String> = env
        .iter()
        .filter_map(|kv| kv.split_once('=').map(|(k, _)| k.to_string()))
        .collect();

    // 三段按序注入：shared（任一半）→ x11 → wayland
    for (enabled, section) in [
        (x11 || wayland, &rule.shared),
        (x11, &rule.x11),
        (wayland, &rule.wayland),
    ] {
        if !enabled {
            continue;
        }
        // env：展开值；空值跳过；已声明同 key 跳过（XAUTHORITY 由下处 core 恒注入）
        for entry in &section.env {
            let Some((key, value)) = entry.split_once('=') else {
                continue;
            };
            if key == "XAUTHORITY" {
                continue;
            }
            if existing_env_keys.contains(key) {
                continue;
            }
            let expanded = expand_host_vars(value, &vars);
            if expanded.is_empty() {
                continue;
            }
            env.push(format!("{key}={expanded}"));
            existing_env_keys.insert(key.to_string());
        }
        // mounts：两侧展开；空值 / require_exists 缺失 / 已声明同目标 跳过
        for m in &section.mounts {
            let host_path = expand_host_vars(&m.host_path, &vars);
            let container_path = expand_host_vars(&m.container_path, &vars);
            if host_path.is_empty() || container_path.is_empty() {
                continue;
            }
            if m.require_exists && !Path::new(&host_path).exists() {
                continue;
            }
            if existing_mount_targets.contains(&container_path) {
                continue;
            }
            params.mounts.push(MountConfig {
                host_path,
                container_path: container_path.clone(),
                read_only: m.read_only,
            });
            existing_mount_targets.insert(container_path);
        }
        if section.keep_id {
            params.keep_id = true;
        }
    }

    // XAUTHORITY 稳定间接路径：仅 X11 半（core 恒注入 + 幂等去重 + 覆盖）。
    // 真实 auth 文件由容器内 server 启动时探测并维护软链（setup.rs ensure_xauthority）。
    //
    // 意图驱动覆盖：DISPLAY / XAUTHORITY / WAYLAND_DISPLAY 三个 GUI 意图 env 按当前
    // 意图覆盖（开→宿主实时值，关→置空），覆盖 commit 快照烘焙进镜像的旧值。
    // 空串 = 覆盖镜像继承的旧值（podman create 容器 env 按 key 覆盖 image env）。
    let display = if x11 {
        vars.get("DISPLAY").cloned().unwrap_or_default()
    } else {
        String::new()
    };
    set_env(env, "DISPLAY", &display);
    let xauth = if x11 {
        XAUTHORITY_STABLE_PATH.to_string()
    } else {
        String::new()
    };
    set_env(env, "XAUTHORITY", &xauth);
    let wayland_display = if wayland {
        vars.get("WAYLAND_DISPLAY").cloned().unwrap_or_default()
    } else {
        String::new()
    };
    set_env(env, "WAYLAND_DISPLAY", &wayland_display);
}

/// 覆盖式设置 env（替换同 key 既有项；无则追加）。用于意图驱动覆盖——
/// commit 快照把容器 env 烘焙进镜像、重建继承，故 GUI 意图 env 必须按意图
/// 覆盖（而非仅幂等追加），关时置空以清除镜像里的旧值。
fn set_env(env: &mut Vec<String>, key: &str, value: &str) {
    let prefix = format!("{key}=");
    env.retain(|e| !e.starts_with(&prefix));
    env.push(format!("{key}={value}"));
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

    /// 回归（2026-09-02）：规则里的 `${XDG_RUNTIME_DIR}` 展开后与已声明的
    /// `/run/user/1000` 相同 → 必须跳过（否则注入重复项、后续被 dedup 静默丢）。
    /// （shared 段：X11 半开启即可触发。）
    #[test]
    fn apply_skips_existing_expanded_target() {
        let mut params = ContainerParams::default();
        // 模板已声明 /run/user/1000 → /run/user/1000
        params.mounts.push(MountConfig {
            host_path: "/run/user/1000".into(),
            container_path: "/run/user/1000".into(),
            read_only: false,
        });
        let mut env = Vec::new();
        apply(&mut params, &mut env, true, false);
        let run_count = params
            .mounts
            .iter()
            .filter(|m| m.container_path == "/run/user/1000")
            .count();
        assert_eq!(
            run_count, 1,
            "展开后同 container_path 应跳过（避免注入重复 /run/user/1000）：{:?}",
            params.mounts
        );
        // 其余新增（字体/图标等）仍应注入
        assert!(
            params
                .mounts
                .iter()
                .any(|m| m.container_path == "/mnt/host/fonts"),
            "字体挂载应注入：{:?}",
            params.mounts
        );
    }

    #[test]
    fn expand_known_var() {
        let v = vars();
        assert_eq!(
            expand_host_vars("${HOME}/.local/share/fonts", &v),
            "/home/div/.local/share/fonts"
        );
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

    /// 规则文件本身应可解析（锁定「assets 规则 = 合法三段式 GuiPassthroughRule」契约）。
    #[test]
    fn embedded_rule_parses() {
        let rule = load_rule();
        // shared：任一半开启即注入的基建
        assert!(rule.shared.keep_id, "shared 段应恒开 keep-id");
        assert!(
            rule.shared
                .mounts
                .iter()
                .any(|m| m.host_path == "${XDG_RUNTIME_DIR}"
                    && m.container_path == "${XDG_RUNTIME_DIR}"),
            "shared 应含 XDG_RUNTIME_DIR 挂载（两侧占位符）：{:?}",
            rule.shared.mounts
        );
        assert!(
            rule.shared
                .mounts
                .iter()
                .any(|m| m.host_path == "/usr/share/fonts" && m.require_exists && m.read_only),
            "shared 应含系统字体挂载（require_exists + 只读）：{:?}",
            rule.shared.mounts
        );
        assert!(
            rule.shared
                .env
                .iter()
                .any(|e| e.starts_with("XDG_DATA_DIRS=") && e.contains("/usr/share")),
            "shared 应含 XDG_DATA_DIRS 系统默认：{:?}",
            rule.shared.env
        );
        // x11：X11 socket 恒注入（require_exists=false）；DISPLAY 半段 env
        assert!(
            rule.x11
                .mounts
                .iter()
                .any(|m| m.host_path == "/tmp/.X11-unix" && !m.require_exists),
            "x11 应含 X11 socket 且非 require_exists：{:?}",
            rule.x11.mounts
        );
        assert!(
            rule.x11.env.iter().any(|e| e.starts_with("DISPLAY=")),
            "x11 应含 DISPLAY env：{:?}",
            rule.x11.env
        );
        // wayland：WAYLAND_DISPLAY 半段 env（socket 在 $XDG_RUNTIME_DIR，shared 段挂载）
        assert!(
            rule.wayland
                .env
                .iter()
                .any(|e| e.starts_with("WAYLAND_DISPLAY=")),
            "wayland 应含 WAYLAND_DISPLAY env：{:?}",
            rule.wayland.env
        );
    }
}
