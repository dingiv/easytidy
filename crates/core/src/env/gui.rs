//! GUI 直通规则加载 + 注入（内容数据驱动，**两本规则书**）。
//!
//! `gui_x11` / `gui_wayland` 每半取 [`GuiMode`](crate::models::GuiMode) 三态：
//! - `direct` → [`ASSETS_DIR::gui-passthrough.yaml`]（宿主直通，容器 GUI 画到宿主屏）
//! - `bypass` → [`ASSETS_DIR::gui-bypass.yaml`]（容器自有世界，宿主资源只经
//!   `HOST_*` 改名旁路；设计依据 `crates/core/data/xtiger_container.md`）
//! - `none` → 不注入
//!
//! 两模型可混搭（各半独立），可同时激活（此时 shared 冲突项由 bypass 优先，
//! 见 [`apply`]）。代码只负责「执行」：规则文件 → 占位符展开 → 存在性/空值
//! 检查 → 幂等去重注入。
//!
//! **占位符视角（两本规则书的关键差异）**：
//! - direct：`host_path` / `container_path` / env 值统一按**宿主 env** 展开
//!   （直通本来就是「宿主值原样进容器」）。
//! - bypass：`host_path` 按宿主 env；`container_path` 与 env 值按**容器 env 字典**
//!   （本文件 env 段写进 podman Config.Env 的那套）展开，字典优先、宿主兑底。
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

use crate::models::{ContainerParams, GuiMode, MountConfig};

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

/// 加载旁路直通规则（`gui-bypass.yaml`，形状与直通完全对称的三段式）。
///
/// 加载链路与 [`load_rule`] 一致：ASSETS_DIR 读取 → prod 首跑播种 → 失败回退
/// 内嵌默认（源码树文件的编译期副本）。语义差异见模块文档「占位符视角」。
pub fn load_bypass_rule() -> GuiPassthroughRule {
    const EMBEDDED: &str = include_str!("../../../gui/assets/gui-bypass.yaml");

    if let Some(path) = easytidy_shared::loader!().resolve("ASSETS_DIR::gui-bypass.yaml") {
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
                    "gui-bypass 规则解析失败（{}）：{e}，回退内嵌默认",
                    path.display()
                ),
            }
        }
    }
    serde_yaml::from_str::<GuiPassthroughRule>(EMBEDDED)
        .unwrap_or_else(|e| panic!("内嵌 gui-bypass.yaml 解析失败（构建期应可解析）：{e}"))
}

/// 执行 GUI 注入（两本规则书的统一入口）：按 `gui_x11` / `gui_wayland` 的
/// [`GuiMode`] 分派到 direct（[`load_rule`]）与 bypass（[`load_bypass_rule`]），
/// 两模型可同时激活（混搭：各半独立）。
///
/// - **direct 三段**：按宿主视角展开；bypass 任一半激活时**抑制**其 shared 里
///   落 `$XDG_RUNTIME_DIR` 的 env/挂载（否则盖掉容器私有目录，
///   D-Bus 洗环境的机制 B 复活——见 xtiger 设计稿 §1.3）。
/// - **bypass 三段**：`host_path` 按宿主 env，`container_path` 与 env 值按
///   **容器 env 字典**（本文件 env 段）展开，字典优先、宿主兑底；env 段边展开
///   边入字典（后续段可引用 `${HOST_XDG_RUNTIME_DIR}`）。X11 半的
///   `HOST_XAUTHORITY` = 规则声明的**稳定软链**（默认 `/run/easytidy/xauthority`），
///   由容器内 server 探 [自有 runtime dir, HOST_XDG] 双目录维护 + 轮换重探——
///   cookie 文件名轮换无需 rebuild。
/// - **意图驱动覆盖**（DISPLAY / XAUTHORITY / WAYLAND_DISPLAY）最后单点做一次：
///   direct → 宿主实时值 / 稳定软链路径；bypass → 容器自有值（`:0` / 置空 /
///   `wayland-0` 悬空）；none → 置空（清镜像烘焙旧值）。
///
/// **用户/模板显式声明的 key 不覆盖**（`declared_env_keys` 由
/// [`crate::env::host::inject_passthrough`] 在注入前采集传入）。
pub fn apply(
    params: &mut ContainerParams,
    env: &mut Vec<String>,
    x11: GuiMode,
    wayland: GuiMode,
    declared_env_keys: &std::collections::HashSet<String>,
) {
    let hv = host_vars();
    let direct_active = x11.is_direct() || wayland.is_direct();
    let bypass_active = x11.is_bypass() || wayland.is_bypass();

    let mut existing_mount_targets: std::collections::HashSet<String> = params
        .mounts
        .iter()
        .map(|m| m.container_path.clone())
        .collect();
    let mut existing_env_keys: std::collections::HashSet<String> = env
        .iter()
        .filter_map(|kv| kv.split_once('=').map(|(k, _)| k.to_string()))
        .collect();
    // 容器 env 字典（bypass 视角）：bypass env 段展开后的键值，供后续
    // container_path / env 引用（字典优先、宿主兑底）。
    let mut cenv: HashMap<String, String> = HashMap::new();

    // ── direct 三段：shared（任一半 direct）→ x11 → wayland ──
    if direct_active {
        let rule = load_rule();
        for (enabled, section) in [
            (true, &rule.shared),
            (x11.is_direct(), &rule.x11),
            (wayland.is_direct(), &rule.wayland),
        ] {
            if !enabled {
                continue;
            }
            inject_section(
                section,
                false,
                bypass_active,
                params,
                env,
                &hv,
                &mut cenv,
                &mut existing_mount_targets,
                &mut existing_env_keys,
            );
        }
    }

    // ── bypass 三段：shared（任一半 bypass）→ x11 → wayland ──
    let bypass_rule = bypass_active.then(load_bypass_rule);
    if let Some(rule) = &bypass_rule {
        for (enabled, section) in [
            (true, &rule.shared),
            (x11.is_bypass(), &rule.x11),
            (wayland.is_bypass(), &rule.wayland),
        ] {
            if !enabled {
                continue;
            }
            inject_section(
                section,
                true,
                false,
                params,
                env,
                &hv,
                &mut cenv,
                &mut existing_mount_targets,
                &mut existing_env_keys,
            );
        }
    }

    // ── 意图驱动覆盖（单点；见文档）：开 → 该模型的值，关 → 置空 ──
    // 命名随规则走（当前规则：容器自有 :99、宿主 X 原名 :0——见
    // gui-bypass.yaml）。**删行 = 置空**（P1 兑底：绝不能回退 `:0`——那可能
    // 是宿主 X 的名字，回退会让应用默落宿主屏）。
    let bypass_display = bypass_rule
        .as_ref()
        .and_then(|r| section_env_value(&r.x11, "DISPLAY"))
        .unwrap_or_default();
    let bypass_wayland = bypass_rule
        .as_ref()
        .and_then(|r| section_env_value(&r.wayland, "WAYLAND_DISPLAY"))
        .unwrap_or_else(|| "wayland-0".to_string());
    let display = match x11 {
        GuiMode::Direct => hv.get("DISPLAY").cloned().unwrap_or_default(),
        GuiMode::Bypass => bypass_display,
        GuiMode::None => String::new(),
    };
    set_env(env, "DISPLAY", &display, declared_env_keys);
    // XAUTHORITY / HOST_XAUTHORITY（单点覆盖，规则文件为准，逐模式）：
    // - direct：XAUTHORITY = 稳定软链路径（server 维护 + 30s 重探），HOST_ 置空；
    // - bypass：XAUTHORITY = 规则声明的**容器自有**值（
    //   `${XDG_RUNTIME_DIR}/.Xauthority`，归容器自己的 X——server **不碰**它，
    //   避免覆盖 Xtigervnc 写的 auth 文件）；HOST_XAUTHORITY = 规则声明的稳定
    //   软链（server 探双目录维护 + 轮换重探）；
    // - none：两者置空（清镜像烘焙旧值）。
    let merged = merged_dict(&hv, &cenv);
    let xauth = match x11 {
        GuiMode::Direct => XAUTHORITY_STABLE_PATH.to_string(),
        GuiMode::Bypass => bypass_rule
            .as_ref()
            .and_then(|r| section_env_value(&r.x11, "XAUTHORITY"))
            .map(|v| expand_host_vars(&v, &merged))
            .unwrap_or_default(),
        GuiMode::None => String::new(),
    };
    set_env(env, "XAUTHORITY", &xauth, declared_env_keys);
    let host_xauth = match x11 {
        GuiMode::Bypass => bypass_rule
            .as_ref()
            .and_then(|r| section_env_value(&r.x11, "HOST_XAUTHORITY"))
            .map(|v| expand_host_vars(&v, &merged))
            .filter(|v| !v.is_empty())
            // 规则删行时的兑底：仍是稳定路径（server 维护同一链）
            .unwrap_or_else(|| XAUTHORITY_STABLE_PATH.to_string()),
        GuiMode::Direct | GuiMode::None => String::new(),
    };
    set_env(env, "HOST_XAUTHORITY", &host_xauth, declared_env_keys);
    let wayland_display = match wayland {
        GuiMode::Direct => hv.get("WAYLAND_DISPLAY").cloned().unwrap_or_default(),
        GuiMode::Bypass => bypass_wayland,
        GuiMode::None => String::new(),
    };
    set_env(env, "WAYLAND_DISPLAY", &wayland_display, declared_env_keys);
}

/// 向 env/挂载集合注入一个规则段。
///
/// - `bypass_view`：true = container_path/env 值按容器 env 字典展开（字典优先、
///   宿主兑底），env 展开后同步入字典；false = 两侧统一宿主视角（直通旧语义）。
/// - `suppress_runtime_dir`：true = 跳过落 `$XDG_RUNTIME_DIR` 的 env/挂载
///   （bypass 激活时对 direct shared 的抑制，防止盖掉容器私有 runtime dir）。
#[allow(clippy::too_many_arguments)]
fn inject_section(
    section: &GuiPassthroughSection,
    bypass_view: bool,
    suppress_runtime_dir: bool,
    params: &mut ContainerParams,
    env: &mut Vec<String>,
    hv: &HashMap<String, String>,
    cenv: &mut HashMap<String, String>,
    existing_mount_targets: &mut std::collections::HashSet<String>,
    existing_env_keys: &mut std::collections::HashSet<String>,
) {
    // env：展开值；空值跳过；已声明同 key 跳过（XAUTHORITY 由 apply 单点注入）
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
        if suppress_runtime_dir && key == "XDG_RUNTIME_DIR" {
            continue;
        }
        let expanded = if bypass_view {
            let dict = merged_dict(hv, cenv);
            expand_host_vars(value, &dict)
        } else {
            expand_host_vars(value, hv)
        };
        if expanded.is_empty() {
            continue;
        }
        env.push(format!("{key}={expanded}"));
        existing_env_keys.insert(key.to_string());
        if bypass_view {
            cenv.insert(key.to_string(), expanded);
        }
    }
    // mounts：host 侧永远宿主视角；container 侧按模型视角；空值 / require_exists
    // 缺失 / 已声明同目标 跳过
    for m in &section.mounts {
        if suppress_runtime_dir && m.container_path.contains("XDG_RUNTIME_DIR") {
            continue;
        }
        let host_path = expand_host_vars(&m.host_path, hv);
        let container_path = if bypass_view {
            let dict = merged_dict(hv, cenv);
            expand_host_vars(&m.container_path, &dict)
        } else {
            expand_host_vars(&m.container_path, hv)
        };
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

/// 合并展开字典：宿主 env 兑底 + 容器字典优先（bypass 视角）。
fn merged_dict(
    hv: &HashMap<String, String>,
    cenv: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut dict = hv.clone();
    dict.extend(cenv.iter().map(|(k, v)| (k.clone(), v.clone())));
    dict
}

/// 取段内 `KEY=VALUE` 的 VALUE 原串（未展开；供 apply 意图覆盖读取规则声明的
/// 容器自有值，如 bypass 的 `DISPLAY=:0`）。
fn section_env_value(section: &GuiPassthroughSection, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    section
        .env
        .iter()
        .find_map(|e| e.strip_prefix(&prefix).map(str::to_string))
}

/// 覆盖式设置 env（替换同 key 既有项；无则追加）。用于意图驱动覆盖——
/// commit 快照把容器 env 烘焙进镜像、重建继承，故 GUI 意图 env 必须按意图
/// 覆盖（而非仅幂等追加），关时置空以清除镜像里的旧值。
/// **例外**：用户/模板显式声明的 key（`declared_env_keys`）不覆盖、不清空——
/// 用户显式配置优先，引擎注入不越权。
fn set_env(
    env: &mut Vec<String>,
    key: &str,
    value: &str,
    declared_env_keys: &std::collections::HashSet<String>,
) {
    if declared_env_keys.contains(key) {
        return;
    }
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
    // UID/GID：bash shell 变量常未导出（GUI 进程 env 里基本没有）→ 用进程身份兑底
    //（宿主 GUI/CLI 与登录用户同 uid；keep-id 后容器 uid == 此值，
    // 供 bypass 规则的 /run/user/${UID} 展开）。
    if v.get("UID").is_none_or(|s| s.is_empty()) {
        v.insert("UID".to_string(), unsafe { libc::getuid() }.to_string());
    }
    if v.get("GID").is_none_or(|s| s.is_empty()) {
        v.insert("GID".to_string(), unsafe { libc::getgid() }.to_string());
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
        let declared = std::collections::HashSet::new();
        apply(
            &mut params,
            &mut env,
            GuiMode::Direct,
            GuiMode::None,
            &declared,
        );
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

    /// 伪造宿主 env（确定性，不依赖测试机 session 状态）。
    fn fake_host_vars() -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert("HOME".into(), "/home/div".into());
        m.insert("UID".into(), "1000".into());
        m.insert("XDG_RUNTIME_DIR".into(), "/run/user/1000".into());
        m
    }

    /// bypass 规则 + 容器字典展开：★核心旁路（宿主 runtime dir 整目录改名挂入）、
    /// HOST_* env 的字典滚动引用、字体/keep_id 照旧。
    #[test]
    fn bypass_section_dual_view_expansion() {
        let rule = load_bypass_rule();
        let hv = fake_host_vars();
        let mut params = ContainerParams::default();
        let mut env = Vec::new();
        let mut cenv = HashMap::new();
        let mut targets = std::collections::HashSet::new();
        let mut keys = std::collections::HashSet::new();
        inject_section(
            &rule.shared,
            true,
            false,
            &mut params,
            &mut env,
            &hv,
            &mut cenv,
            &mut targets,
            &mut keys,
        );

        // 容器字典：自有 runtime dir（${UID} 宿主展开）+ 宿主改名地址
        assert_eq!(
            cenv.get("XDG_RUNTIME_DIR").map(String::as_str),
            Some("/run/user/1000")
        );
        assert_eq!(
            cenv.get("HOST_XDG_RUNTIME_DIR").map(String::as_str),
            Some("/run/user/999")
        );
        // 字典滚动：后续 env 值可引用前面定义的键（规则文件当前未用到活例
        // ——音频 HOST_* 行被暂注释——用合成段锁机制本身）
        {
            let sect = GuiPassthroughSection {
                env: vec![
                    "A=/run/x".to_string(),
                    "B=${A}/pipewire-0".to_string(),
                    "C=${B}.lock".to_string(),
                ],
                ..Default::default()
            };
            let mut e2 = Vec::new();
            let mut c2 = HashMap::new();
            let mut p2 = ContainerParams::default();
            let mut t2 = std::collections::HashSet::new();
            let mut k2 = std::collections::HashSet::new();
            inject_section(
                &sect, true, false, &mut p2, &mut e2, &hv, &mut c2, &mut t2, &mut k2,
            );
            assert_eq!(c2.get("B").map(String::as_str), Some("/run/x/pipewire-0"));
            assert_eq!(
                c2.get("C").map(String::as_str),
                Some("/run/x/pipewire-0.lock"),
                "env 字典边展开边入典，后续值引用前键"
            );
        }
        // ★核心：host_path 宿主视角、container_path 容器视角（同名占位符两侧不同值）
        assert!(
            params
                .mounts
                .iter()
                .any(|m| m.host_path == "/run/user/1000"
                    && m.container_path == "/run/user/999"
                    && m.read_only),
            "宿主 runtime dir 应整目录只读改名挂入：{:?}",
            params.mounts
        );
        // 字体/keep_id 照旧（非显示通道）
        assert!(
            params
                .mounts
                .iter()
                .any(|m| m.container_path == "/mnt/host/fonts")
        );
        assert!(params.keep_id);

        // x11/wayland 段的结构性契约（挂载含 require_exists，不在无 X 的机器上跑）
        // x11 段结构契约（具体命名随规则走：当前 = 自有 :99 / 宿主 X0 原名）
        assert!(rule.x11.env.iter().any(|e| e.starts_with("DISPLAY=")));
        assert!(rule.x11.env.iter().any(|e| e.starts_with("HOST_DISPLAY=")));
        let xm = rule
            .x11
            .mounts
            .first()
            .expect("x11 段应有单文件 X 旁路挂载");
        assert!(
            xm.host_path.starts_with("/tmp/.X11-unix/X")
                && xm.container_path.starts_with("/tmp/.X11-unix/X"),
            "X 旁路应是 /tmp/.X11-unix/X<id> 单文件：{xm:?}"
        );
        assert!(xm.require_exists, "宿主 X 缺失时应跳过（headless 宿主）：{xm:?}");
        // WAYLAND 行存在即可（值随规则——当前为配空 = 显式不要 Wayland）
        assert!(
            rule.wayland
                .env
                .iter()
                .any(|e| e.starts_with("WAYLAND_DISPLAY=")),
            "wayland 段应有 WAYLAND_DISPLAY 行（值可为空）：{:?}",
            rule.wayland.env
        );
        // 安全契约：bypass 规则不把宿主 runtime dir 挂到 $XDG_RUNTIME_DIR（直通语义）
        assert!(
            !rule.shared.mounts.iter().any(|m| m.container_path
                == "${XDG_RUNTIME_DIR}"
                && m.host_path == "${XDG_RUNTIME_DIR}"),
            "bypass 不应含直通的 runtime dir 同名挂载"
        );
    }

    /// direct shared 的宿主 runtime dir 整目录挂载与 XDG_RUNTIME_DIR env 在
    /// bypass 激活时被抑制（防盖掉容器私有目录 = D-Bus 洗环境的机制 B 复活）；
    /// 非 runtime 项（字体等）照注。
    #[test]
    fn direct_shared_runtime_dir_suppressed_by_bypass() {
        let rule = load_rule();
        let hv = fake_host_vars();

        // 不抑制（纯 direct）→ 宿主 runtime dir 同名挂载 + env 正常注入
        let mut params = ContainerParams::default();
        let mut env = Vec::new();
        let mut cenv = HashMap::new();
        let mut targets = std::collections::HashSet::new();
        let mut keys = std::collections::HashSet::new();
        inject_section(
            &rule.shared,
            false,
            false,
            &mut params,
            &mut env,
            &hv,
            &mut cenv,
            &mut targets,
            &mut keys,
        );
        assert!(
            params
                .mounts
                .iter()
                .any(|m| m.container_path == "/run/user/1000"),
            "纯 direct 应注入宿主 runtime dir：{:?}",
            params.mounts
        );
        assert!(env.iter().any(|e| e.starts_with("XDG_RUNTIME_DIR=/run/user/1000")));

        // 抑制（bypass 激活）→ runtime dir 项消失，其它照注
        let mut params = ContainerParams::default();
        let mut env = Vec::new();
        let mut cenv = HashMap::new();
        let mut targets = std::collections::HashSet::new();
        let mut keys = std::collections::HashSet::new();
        inject_section(
            &rule.shared,
            false,
            true,
            &mut params,
            &mut env,
            &hv,
            &mut cenv,
            &mut targets,
            &mut keys,
        );
        assert!(
            !params
                .mounts
                .iter()
                .any(|m| m.container_path == "/run/user/1000"),
            "bypass 激活时 direct 的宿主 runtime dir 挂载应被抑制：{:?}",
            params.mounts
        );
        assert!(
            !env.iter().any(|e| e.starts_with("XDG_RUNTIME_DIR=")),
            "bypass 激活时 direct 的 XDG_RUNTIME_DIR env 应被抑制：{:?}",
            env
        );
        assert!(
            params
                .mounts
                .iter()
                .any(|m| m.container_path == "/mnt/host/fonts"),
            "非 runtime 项照注"
        );
    }

    /// bypass 意图覆盖（apply 单点）：DISPLAY 随规则（当前 :99——命名是规则
    /// 文件的契约，不是代码常量）、XAUTHORITY = 规则的容器自有值、
    /// HOST_XAUTHORITY 稳定链、WAYLAND 半 none 置空（清镜像烘焙旧值）。
    /// 规则值配空（`WAYLAND_DISPLAY=`）**允许且原样写入**：意图键由 override
    /// 单点写入，空串 = 显式置空（容器 env 收 `KEY=`，清镜像烘焙旧值）。
    /// 与「删行」区分——删行回退 `wayland-0` 悬空名。锁住用户现场配置依赖
    /// 的这条语义（两分支都锁）。
    #[test]
    fn bypass_rule_value_written_verbatim_including_empty() {
        let rule = load_bypass_rule();
        let raw = section_env_value(&rule.wayland, "WAYLAND_DISPLAY");
        let mut params = ContainerParams::default();
        let mut env = Vec::new();
        let declared = std::collections::HashSet::new();
        apply(
            &mut params,
            &mut env,
            GuiMode::None,
            GuiMode::Bypass,
            &declared,
        );
        let got = env
            .iter()
            .find(|e| e.starts_with("WAYLAND_DISPLAY="))
            .map(|e| e.split_once('=').unwrap().1.to_string());
        match raw {
            Some(v) => assert_eq!(
                got.as_deref(),
                Some(v.as_str()),
                "规则行值（含空串）应原样写入容器 env：{env:?}"
            ),
            None => assert_eq!(
                got.as_deref(),
                Some("wayland-0"),
                "规则删行 → 回退悬空 wayland-0：{env:?}"
            ),
        }
    }

    #[test]
    fn apply_bypass_intent_override() {
        let mut params = ContainerParams::default();
        let mut env = Vec::new();
        let declared = std::collections::HashSet::new();
        apply(
            &mut params,
            &mut env,
            GuiMode::Bypass,
            GuiMode::None,
            &declared,
        );
        let get = |k: &str| -> Option<String> {
            env.iter()
                .find(|e| e.starts_with(&format!("{k}=")))
                .map(|e| e.split_once('=').unwrap().1.to_string())
        };
        // DISPLAY / HOST_DISPLAY 随规则文件（当前：自有 :99、宿主 X 原名 :0）
        // ——测试锁「规则值原样进 env」，不锁具体命名；DISPLAY 删行 = 置空（P1
        //：绝不能回退 :0——那可能是宿主 X）。
        let rule = load_bypass_rule();
        match section_env_value(&rule.x11, "DISPLAY") {
            Some(v) => assert_eq!(
                get("DISPLAY").as_deref(),
                Some(v.as_str()),
                "DISPLAY 随规则：{env:?}"
            ),
            None => assert_eq!(
                get("DISPLAY").as_deref(),
                Some(""),
                "DISPLAY 删行 = 置空（P1）：{env:?}"
            ),
        }
        match section_env_value(&rule.x11, "HOST_DISPLAY") {
            Some(v) => assert_eq!(get("HOST_DISPLAY").as_deref(), Some(v.as_str())),
            None => assert!(get("HOST_DISPLAY").is_none(), "无规则行则不注入 HOST_DISPLAY"),
        }
        // 容器自有 X 的 auth 路径：规则值 `${XDG_RUNTIME_DIR}/.Xauthority`
        //（容器字典展开；归容器自己的 X，server 不碰）
        let xa = get("XAUTHORITY").unwrap_or_default();
        assert!(
            xa.starts_with("/run/user/") && xa.ends_with("/.Xauthority"),
            "bypass XAUTHORITY 应为规则声明的容器自有值：{env:?}"
        );
        // HOST_XAUTHORITY = 稳定软链（规则声明，server 维护 + 轮换重探）
        assert_eq!(
            get("HOST_XAUTHORITY").as_deref(),
            Some("/run/easytidy/xauthority")
        );
        assert_eq!(
            get("WAYLAND_DISPLAY").as_deref(),
            Some(""),
            "WAYLAND 半 none → 置空：{env:?}"
        );
        assert_eq!(
            get("HOST_XDG_RUNTIME_DIR").as_deref(),
            Some("/run/user/999")
        );
        assert!(
            get("XDG_RUNTIME_DIR").is_some_and(|v| v.starts_with("/run/user/")),
            "容器自有 runtime dir（${{UID}} 展开）：{env:?}"
        );
        assert!(params.keep_id, "bypass shared 恒开 keep-id");
        // 直通语义的宿主显示值不得出现（除非宿主恰好叫 :0——bypass 的 DISPLAY
        // 就是 :0，故检查 XAUTHORITY 稳定路径不出现）
        assert!(
            !env.iter().any(|e| e.starts_with("XAUTHORITY=/run/easytidy")),
            "bypass 不应注入直通的稳定 XAUTHORITY 路径：{env:?}"
        );
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
