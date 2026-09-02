//! env —— 运行时环境适配的统一模块（宿主侧生成 + 容器侧探测）。
//!
//! 把散在各处的「env 探测 + 配置生成」收敛到这里，作单一事实源：
//! - [`host`]：宿主侧（容器创建期）——透传注入入口（`gui` / `gpu` 意图 → env/mounts）
//! - [`gui`]：GUI 透传规则（`ASSETS_DIR::gui-passthrough.yaml` 资源驱动）
//! - [`incontainer`]：容器侧——身份（[`resolve_identity`]，server 与 ctool 共用
//!   单一事实源）+ 容器运行时 env 探测（XAUTHORITY / XDG_DATA_DIRS / uid）+
//!   容器内准备（passwd/group/home/fontconfig）
//!
//! 两个进程、两个生命周期阶段，不混：
//! - 宿主进程（GUI/CLI 创建容器）→ [`host`] / [`gui`]
//! - 容器内进程（server / ctool 启动）→ [`incontainer`]
//!
//! 支撑工具保持独立：`userenv`（宿主用户探测）、`pathvars`（路径变量展开）——
//! 非 env 生成主体，按需被 host / incontainer / podman 引用。

pub mod gui;
pub mod host;
pub mod incontainer;

pub use gui::{GuiPassthroughMount, GuiPassthroughRule};
pub use host::{
    inject_gpu_passthrough, inject_gui_passthrough, inject_passthrough, parse_gpu_value,
    GpuVendor,
};
pub use incontainer::{
    fixup_xdg_data_dirs_value, prepare_in_container, probe_xauthority, resolve_identity,
    self_uid_gid, Identity,
};
