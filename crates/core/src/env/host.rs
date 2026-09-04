//! 宿主侧 env/挂载生成（容器创建期）。
//!
//! 「意图 → 注入产物」的统一入口：按配置内的 `gui` / `gpu_nvidia` / `gpu_amd`
//! 意图，从宿主实时探测并追加 env/mounts。保证在任何把配置变成运行容器的
//! 路径上（模板展开 `Flavor::build_config` / `ConfTemplate::build_config` /
//! 实例应用 `apply_container_config`）行为一致。
//!
//! - [`inject_passthrough`]：总入口（dispatch gui / gpu）
//! - [`inject_gui_passthrough`]：GUI 透传（规则外置到 `ASSETS_DIR::
//!   gui-passthrough.yaml`，执行逻辑见 [`super::gui`]）
//! - [`inject_gpu_passthrough`]：GPU env 注入（NVIDIA 专属 env；AMD 无 vendor env）
//! - [`detect_amd_gpu_devices`]：AMD GPU 裸设备探测（render 节点 + /dev/kfd）
//!
//! 均幂等（已声明项跳过），对已展开过的配置重复调用安全。

use std::collections::HashSet;
use std::path::Path;

use crate::models::{ContainerConfig, ContainerParams};

/// 透传注入总入口：按配置内的 `gui` / `gpu_nvidia` / `gpu_amd` 意图，调用对应的
/// 共享注入函数（[`inject_gui_passthrough`] / [`inject_gpu_passthrough`]）。
///
/// 供模板展开（`Flavor::build_config` / `ConfTemplate::build_config`）与实例
/// 应用/重建（`apply_container_config`）共用——保证「意图 → 注入产物」在任何把
/// 配置变成运行容器的路径上行为一致。
pub fn inject_passthrough(config: &mut ContainerConfig) {
    if config.params.gui {
        inject_gui_passthrough(&mut config.params, &mut config.env);
    }
    if config.params.gpu_nvidia || config.params.gpu_amd {
        inject_gpu_passthrough(
            &mut config.env,
            config.params.gpu_nvidia,
            config.params.gpu_amd,
        );
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

/// GPU env 注入（共享）：按 vendor 幂等追加对应 env。
///
/// - **NVIDIA**：开 → `NVIDIA_VISIBLE_DEVICES=all` + `NVIDIA_DRIVER_CAPABILITIES=all`
/// - **AMD**：开 → 无 vendor 专属 env（ROCm/VAAPI 容器内自检设备即可；设备节点
///   由 [`detect_amd_gpu_devices`] 在 create 时注入 libpod body，不走 env）
///
/// 本函数只负责 env 部分；GPU 设备节点注入：
/// - NVIDIA → libpod body 拼 `nvidia.com/gpu=all` CDI 引用（nvidia-container-toolkit
///   已在宿主生成 nvidia.yaml，可解析）
/// - AMD → create 前调 [`detect_amd_gpu_devices`] 探测裸设备（AMD 无自动 CDI
///   工具链，`amd.com/gpu=all` 在真实宿主恒 unresolvable → 不采用 CDI）
///
/// `security_opts`（label=disable / apparmor=unconfined）属独立关切，保留显式
/// 声明（见 `ContainerParams::security_opts`），此处不隐式注入。
///
/// 与 [`inject_gui_passthrough`] 同款幂等去重：env key 已存在则跳过（模板作者
/// 显式写的优先，引擎不覆盖）。
pub fn inject_gpu_passthrough(env: &mut Vec<String>, nvidia: bool, amd: bool) {
    if !nvidia {
        return;
    }
    let existing_env_keys: HashSet<String> = env
        .iter()
        .filter_map(|kv| kv.split_once('=').map(|(k, _)| k.to_string()))
        .collect();
    if !existing_env_keys.contains("NVIDIA_VISIBLE_DEVICES") {
        env.push("NVIDIA_VISIBLE_DEVICES=all".to_string());
    }
    if !existing_env_keys.contains("NVIDIA_DRIVER_CAPABILITIES") {
        env.push("NVIDIA_DRIVER_CAPABILITIES=all".to_string());
    }
    // AMD 无 vendor 专属 env（容器内自检 /dev/dri + /dev/kfd 即可）。
    // `_amd` 参数保留以备未来 AMD env 需求（ROCm 容器内自检即可，故暂无）。
    let _ = amd;
}

/// 探测宿主要透传的 AMD GPU 裸设备（create 期调用）。
///
/// 返回 easytidy 设备串（`"host:container"`）列表：
/// - `/dev/kfd`（ROCm compute 入口；宿主无 kfd = 无 amdgpu compute，跳过）
/// - `/dev/dri/renderD*` 中 PCI vendor = `0x1002`（AMD）的渲染节点——容器内
///   VAAPI/Mesa 硬件加速与 ROCm 都走 render 节点，够用且不抢宿主 DRM master
///   （card 节点留给宿主合成器，不透传）。
///
/// **为什么不用 CDI `amd.com/gpu=all`**：NVIDIA 有 nvidia-container-toolkit 自动
/// 生成 nvidia.yaml，AMD 生态无对等工具链，`amd.yaml` 在真实宿主几乎恒缺失 →
/// podman 报 `unresolvable CDI devices amd.com/gpu=all`（2026-09-04 desk_pilot
/// 实测）。故 AMD 一律走 sysfs vendor 匹配的裸设备。
pub fn detect_amd_gpu_devices() -> Vec<String> {
    collect_amd_devices(
        Path::new("/dev/dri"),
        Path::new("/sys/class/drm"),
        Path::new("/dev/kfd"),
    )
}

/// [`detect_amd_gpu_devices`] 的纯函数实现（目录注入以便单测）。
///
/// - `dri_dir`：宿主 DRM 设备节点目录（prod `/dev/dri`）
/// - `sys_drm_dir`：DRM sysfs 类目录（prod `/sys/class/drm`），每个节点
///   `<sys_drm_dir>/<name>/device/vendor` 可读 PCI vendor（如 `0x1002`）
/// - `kfd_path`：ROCm 计算入口（prod `/dev/kfd`）；不存在则跳过
fn collect_amd_devices(dri_dir: &Path, sys_drm_dir: &Path, kfd_path: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if kfd_path.exists() {
        out.push(format!("{}:/dev/kfd", kfd_path.display()));
    }
    let Ok(entries) = std::fs::read_dir(dri_dir) else {
        return out;
    };
    let mut render_nodes: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("renderD"))
        .collect();
    render_nodes.sort();
    for name in render_nodes {
        let vendor_file = sys_drm_dir.join(&name).join("device").join("vendor");
        let Ok(vendor) = std::fs::read_to_string(vendor_file) else {
            continue;
        };
        if vendor.trim() == "0x1002" {
            out.push(format!("/dev/dri/{name}:/dev/dri/{name}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &std::path::Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn test_collect_amd_devices_matches_vendor_1002() {
        let tmp = std::env::temp_dir().join(format!("easytidy-host-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let dri = tmp.join("dev/dri");
        let sys = tmp.join("sys/class/drm");
        let kfd = tmp.join("dev/kfd");

        // /dev/dri 下：renderD128 = NVIDIA(0x10de)、renderD129 = AMD(0x1002)、
        // renderD130 = Intel(0x8086)、外加非 render 的 card 节点（不应被扫到）
        for name in ["renderD128", "renderD129", "renderD130", "card2"] {
            write(&dri.join(name), "");
        }
        for (node, vendor) in [
            ("renderD128", "0x10de"),
            ("renderD129", "0x1002"),
            ("renderD130", "0x8086"),
        ] {
            write(&sys.join(node).join("device/vendor"), vendor);
        }
        write(&kfd, "");

        let got = collect_amd_devices(&dri, &sys, &kfd);
        assert_eq!(
            got,
            vec![
                format!("{}:/dev/kfd", kfd.display()),
                "/dev/dri/renderD129:/dev/dri/renderD129".to_string(),
            ],
            "仅 AMD render 节点 + kfd 入选"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_collect_amd_devices_empty_when_no_amd() {
        let tmp = std::env::temp_dir().join(format!("easytidy-host-test2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let dri = tmp.join("dev/dri");
        let sys = tmp.join("sys/class/drm");
        let kfd = tmp.join("dev/kfd"); // 不存在 = 无 kfd

        write(&dri.join("renderD128"), "");
        write(&sys.join("renderD128").join("device/vendor"), "0x10de");

        // kfd 不存在 → 只可能扫 render 节点；renderD128 非 AMD → 空
        assert!(collect_amd_devices(&dri, &sys, &kfd).is_empty());

        // dri 目录缺失 → 空（不 panic）
        let missing = tmp.join("no-such-dev-dri");
        assert!(collect_amd_devices(&missing, &sys, &kfd).is_empty());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    fn env_keys(env: &[String]) -> Vec<&str> {
        env.iter()
            .filter_map(|kv| kv.split_once('=').map(|(k, _)| k))
            .collect()
    }

    #[test]
    fn test_inject_gpu_nvidia() {
        let mut env = Vec::new();
        inject_gpu_passthrough(&mut env, true, false);
        assert_eq!(env.len(), 2);
        assert!(env.contains(&"NVIDIA_VISIBLE_DEVICES=all".to_string()));
        assert!(env.contains(&"NVIDIA_DRIVER_CAPABILITIES=all".to_string()));
    }

    #[test]
    fn test_inject_gpu_amd_no_env() {
        let mut env = Vec::new();
        inject_gpu_passthrough(&mut env, false, true);
        assert!(env.is_empty(), "AMD 不应注入 NVIDIA_* env（ROCm 容器内自检）");
    }

    #[test]
    fn test_inject_gpu_both_vendors() {
        // 同时开 NVIDIA + AMD → 只有 NVIDIA env（AMD 无 vendor 专属 env）
        let mut env = Vec::new();
        inject_gpu_passthrough(&mut env, true, true);
        assert_eq!(env.len(), 2);
        assert!(env.contains(&"NVIDIA_VISIBLE_DEVICES=all".to_string()));
        assert!(env.contains(&"NVIDIA_DRIVER_CAPABILITIES=all".to_string()));
    }

    #[test]
    fn test_inject_gpu_idempotent() {
        let mut env = vec!["NVIDIA_VISIBLE_DEVICES=all".to_string()];
        inject_gpu_passthrough(&mut env, true, false);
        assert_eq!(env.len(), 2, "已有 key 不重复注入，仅补 CAPABILITIES");
        assert!(env.contains(&"NVIDIA_DRIVER_CAPABILITIES=all".to_string()));
    }

    #[test]
    fn test_inject_gpu_no_vendor() {
        let mut env = Vec::new();
        inject_gpu_passthrough(&mut env, false, false);
        assert!(env.is_empty());
    }

    #[test]
    fn test_inject_passthrough_dispatch() {
        // 仅 gpu_nvidia 开关时：env 应含 NVIDIA_*；mounts 由 gui 决定（gui 关则空）
        let mut cfg = ContainerConfig {
            name: "t".into(),
            params: ContainerParams {
                image: "alpine".into(),
                gui: false,
                gpu_nvidia: true,
                gpu_amd: false,
                ..ContainerParams::default()
            },
            env: vec![],
            silent_boot: false,
            persistent: true,
        };
        inject_passthrough(&mut cfg);
        assert!(cfg.env.iter().any(|e| e == "NVIDIA_VISIBLE_DEVICES=all"));
        assert!(cfg.env.iter().any(|e| e == "NVIDIA_DRIVER_CAPABILITIES=all"));
        assert_eq!(env_keys(&cfg.env).len(), 2);
    }
}