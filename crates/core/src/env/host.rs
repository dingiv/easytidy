//! 宿主侧 env/挂载生成（容器创建期）。
//!
//! 「意图 → 注入产物」的统一入口：按配置内的 `gui` / `gpu` 意图，从宿主实时
//! 探测并追加 env/mounts。保证在任何把配置变成运行容器的路径上（模板展开
//! `Flavor::build_config` / `ConfTemplate::build_config` / 实例应用
//! `apply_container_config`）行为一致。
//!
//! - [`inject_passthrough`]：总入口（dispatch gui / gpu）
//! - [`inject_gui_passthrough`]：GUI 透传（规则外置到 `ASSETS_DIR::
//!   gui-passthrough.yaml`，执行逻辑见 [`super::gui`]）
//! - [`inject_gpu_passthrough`]：GPU 透传（按 vendor 注入对应 env）
//! - [`GpuVendor`] / [`parse_gpu_value`]：GPU vendor 解析（nvidia / amd）
//!
//! 均幂等（已声明项跳过），对已展开过的配置重复调用安全。

use std::collections::HashSet;

use crate::models::{ContainerConfig, ContainerParams};

/// GPU vendor（决定 CDI 前缀 + env 注入策略）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuVendor {
    Nvidia,
    Amd,
}

impl GpuVendor {
    /// CDI 设备前缀（如 `nvidia` / `amd`），拼成 `<prefix>.com/gpu=<spec>`。
    pub fn cdi_prefix(&self) -> &'static str {
        match self {
            GpuVendor::Nvidia => "nvidia",
            GpuVendor::Amd => "amd",
        }
    }
}

/// 解析 GPU 透传值为 `(vendor, device_spec)`。
///
/// 值格式：
/// - `nvidia` / `nvidia=all` / `nvidia=0` / `nvidia=device=<uuid>` → `(Nvidia, ...)`
/// - `amd` / `amd=all` / `amd=0` → `(Amd, ...)`
/// - `all` / `0` / `device=<uuid>` / `true` → `(Nvidia, value)`（向后兼容）
pub fn parse_gpu_value(value: &str) -> (GpuVendor, &str) {
    let trimmed = value.trim();
    if trimmed == "true" {
        return (GpuVendor::Nvidia, "all");
    }
    if let Some(rest) = trimmed.strip_prefix("nvidia") {
        let spec = rest.strip_prefix('=').unwrap_or("all");
        (GpuVendor::Nvidia, spec)
    } else if let Some(rest) = trimmed.strip_prefix("amd") {
        let spec = rest.strip_prefix('=').unwrap_or("all");
        (GpuVendor::Amd, spec)
    } else {
        (GpuVendor::Nvidia, trimmed)
    }
}

/// 透传注入总入口：按配置内的 `gui` / `gpu` 意图，调用对应的共享注入函数
/// （[`inject_gui_passthrough`] / [`inject_gpu_passthrough`]）。
///
/// 供模板展开（`Flavor::build_config` / `ConfTemplate::build_config`）与实例
/// 应用/重建（`apply_container_config`）共用——保证「意图 → 注入产物」在任何把
/// 配置变成运行容器的路径上行为一致。
pub fn inject_passthrough(config: &mut ContainerConfig) {
    if config.params.gui {
        inject_gui_passthrough(&mut config.params, &mut config.env);
    }
    if let Some(gpu) = config.params.gpu.clone() {
        if !gpu.trim().is_empty() {
            inject_gpu_passthrough(&mut config.env, &gpu);
        }
    }
}

/// GUI 透传注入（共享）：按 [`super::gui`] 规则，从宿主探测
/// DISPLAY/WAYLAND_DISPLAY/XDG_RUNTIME_DIR + 字体/图标挂载，追加到 env 与
/// params.mounts，并恒开 keep-id。
///
/// 「注入什么」（映射表 / XDG_DATA_DIRS 值 / keep-id）外置到资源文件
/// `ASSETS_DIR::gui-passthrough.yaml`（dev 源码树 / prod 数据目录），本函数只
/// 负责执行——详见 [`super::gui`]。
///
/// **`XAUTHORITY` 不在此处注入**：路径含随机后缀（`mutter-Xwaylandauth.<random>`
/// 或 `xauth_<random>`），由容器内 `easytidy-server` 启动时自动探 `$XDG_RUNTIME_DIR`
/// 下已知模式并覆盖进程 env（见 `crates/server/src/setup.rs` `ensure_xauthority`）。
pub fn inject_gui_passthrough(params: &mut ContainerParams, env: &mut Vec<String>) {
    crate::env::gui::apply(params, env);
}

/// GPU 透传注入（共享）：按 vendor 幂等追加对应 env。
///
/// - **NVIDIA**：`NVIDIA_VISIBLE_DEVICES=<spec>` + `NVIDIA_DRIVER_CAPABILITIES=all`
/// - **AMD**：无 vendor 专属 env（设备节点经 CDI `amd.com/gpu=<spec>` 注入）
///
/// **设备节点由 `params.gpu` 直接驱动**（libpod 端拼 `<vendor>.com/gpu=<spec>`
/// CDI 引用），本函数只负责 env 部分。`security_opts`（label=disable / apparmor=
/// unconfined）属独立关切，保留显式声明（见 `ContainerParams::security_opts`），
/// 此处不隐式注入。
///
/// 与 [`inject_gui_passthrough`] 同款幂等去重：env key 已存在则跳过（模板作者
/// 显式写的优先，引擎不覆盖）。
pub fn inject_gpu_passthrough(env: &mut Vec<String>, value: &str) {
    let (vendor, spec) = parse_gpu_value(value);
    if vendor == GpuVendor::Amd {
        return;
    }
    let existing_env_keys: HashSet<String> = env
        .iter()
        .filter_map(|kv| kv.split_once('=').map(|(k, _)| k.to_string()))
        .collect();
    if !existing_env_keys.contains("NVIDIA_VISIBLE_DEVICES") {
        env.push(format!("NVIDIA_VISIBLE_DEVICES={spec}"));
    }
    if !existing_env_keys.contains("NVIDIA_DRIVER_CAPABILITIES") {
        env.push("NVIDIA_DRIVER_CAPABILITIES=all".to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_gpu_value_nvidia() {
        assert_eq!(parse_gpu_value("nvidia"), (GpuVendor::Nvidia, "all"));
        assert_eq!(parse_gpu_value("nvidia=all"), (GpuVendor::Nvidia, "all"));
        assert_eq!(parse_gpu_value("nvidia=0"), (GpuVendor::Nvidia, "0"));
        assert_eq!(parse_gpu_value("nvidia=device=abc"), (GpuVendor::Nvidia, "device=abc"));
    }

    #[test]
    fn test_parse_gpu_value_amd() {
        assert_eq!(parse_gpu_value("amd"), (GpuVendor::Amd, "all"));
        assert_eq!(parse_gpu_value("amd=all"), (GpuVendor::Amd, "all"));
        assert_eq!(parse_gpu_value("amd=0"), (GpuVendor::Amd, "0"));
    }

    #[test]
    fn test_parse_gpu_value_backward_compat() {
        // 无 vendor 前缀 → 视为 NVIDIA
        assert_eq!(parse_gpu_value("all"), (GpuVendor::Nvidia, "all"));
        assert_eq!(parse_gpu_value("0"), (GpuVendor::Nvidia, "0"));
        assert_eq!(parse_gpu_value("device=abc"), (GpuVendor::Nvidia, "device=abc"));
        assert_eq!(parse_gpu_value("true"), (GpuVendor::Nvidia, "all"));
    }

    #[test]
    fn test_parse_gpu_value_cdi_prefix() {
        assert_eq!(GpuVendor::Nvidia.cdi_prefix(), "nvidia");
        assert_eq!(GpuVendor::Amd.cdi_prefix(), "amd");
    }

    #[test]
    fn test_inject_gpu_nvidia() {
        let mut env = Vec::new();
        inject_gpu_passthrough(&mut env, "nvidia");
        assert_eq!(env.len(), 2);
        assert!(env.contains(&"NVIDIA_VISIBLE_DEVICES=all".to_string()));
        assert!(env.contains(&"NVIDIA_DRIVER_CAPABILITIES=all".to_string()));
    }

    #[test]
    fn test_inject_gpu_nvidia_device() {
        let mut env = Vec::new();
        inject_gpu_passthrough(&mut env, "nvidia=0");
        assert!(env.contains(&"NVIDIA_VISIBLE_DEVICES=0".to_string()));
    }

    #[test]
    fn test_inject_gpu_amd_no_env() {
        let mut env = Vec::new();
        inject_gpu_passthrough(&mut env, "amd");
        assert!(env.is_empty(), "AMD 不应注入 NVIDIA_* env");
    }

    #[test]
    fn test_inject_gpu_backward_compat() {
        // 旧值 "all" → NVIDIA
        let mut env = Vec::new();
        inject_gpu_passthrough(&mut env, "all");
        assert!(env.contains(&"NVIDIA_VISIBLE_DEVICES=all".to_string()));
    }

    #[test]
    fn test_inject_gpu_idempotent() {
        let mut env = vec!["NVIDIA_VISIBLE_DEVICES=all".to_string()];
        inject_gpu_passthrough(&mut env, "nvidia");
        assert_eq!(env.len(), 2, "已有 key 不重复注入，仅补 CAPABILITIES");
        assert!(env.contains(&"NVIDIA_DRIVER_CAPABILITIES=all".to_string()));
    }
}
