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
//! - [`inject_gpu_passthrough`]：GPU 透传（NVIDIA_* env）
//!
//! 均幂等（已声明项跳过），对已展开过的配置重复调用安全。

use std::collections::HashSet;

use crate::models::{ContainerConfig, ContainerParams};

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

/// GPU 透传注入（共享）：给定 GPU 值（`"all"` / 设备名 / `"device=<uuid>"`），
/// 幂等地追加 `NVIDIA_VISIBLE_DEVICES` / `NVIDIA_DRIVER_CAPABILITIES` env。
///
/// **设备节点由 `params.gpu` 直接驱动**（libpod 端拼 `nvidia.com/gpu=<值>` CDI
/// 引用），本函数只负责 env 部分。`security_opts`（label=disable / apparmor=
/// unconfined）属独立关切，保留显式声明（见 `ContainerParams::security_opts`），
/// 此处不隐式注入。
///
/// 与 [`inject_gui_passthrough`] 同款幂等去重：env key 已存在则跳过（模板作者
/// 显式写的优先，引擎不覆盖）。`"true"` 归一化为 `"all"`。
pub fn inject_gpu_passthrough(env: &mut Vec<String>, value: &str) {
    let norm = if value == "true" { "all" } else { value };
    let existing_env_keys: HashSet<String> = env
        .iter()
        .filter_map(|kv| kv.split_once('=').map(|(k, _)| k.to_string()))
        .collect();
    if !existing_env_keys.contains("NVIDIA_VISIBLE_DEVICES") {
        env.push(format!("NVIDIA_VISIBLE_DEVICES={norm}"));
    }
    if !existing_env_keys.contains("NVIDIA_DRIVER_CAPABILITIES") {
        env.push("NVIDIA_DRIVER_CAPABILITIES=all".to_string());
    }
}
