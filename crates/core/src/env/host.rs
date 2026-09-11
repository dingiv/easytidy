//! 宿主侧 env/挂载生成（容器创建期）。
//!
//! 「意图 → 注入产物」的统一入口：按配置内的 `gui_x11` / `gui_wayland` /
//! `gpu_nvidia` / `gpu_amd` 意图，从宿主实时探测并追加 env/mounts。保证在任何
//! 把配置变成运行容器的路径上（模板展开 `Flavor::build_config` /
//! `ConfTemplate::build_config` / 实例应用 `apply_container_config`）行为一致。
//!
//! - [`inject_passthrough`]：总入口（dispatch gui / gpu）
//! - [`inject_gui_passthrough`]：GUI 透传（规则外置到 `ASSETS_DIR::
//!   gui-passthrough.yaml`，执行逻辑见 [`super::gui`]）
//! - [`inject_gpu_passthrough`]：GPU env 注入（NVIDIA 专属 env；AMD 无 vendor env）
//! - [`detect_nvidia_gpu`]：NVIDIA GPU 直通宿主就绪预检（GPU 设备节点 + CDI spec）
//! - [`detect_amd_gpu_devices`]：AMD GPU 裸设备探测（render 节点 + /dev/kfd）
//!
//! 均幂等（已声明项跳过），对已展开过的配置重复调用安全。

use std::collections::HashSet;
use std::path::Path;

use crate::models::{ContainerConfig, ContainerParams};

/// 透传注入总入口：按配置内的 `gui_x11` / `gui_wayland` / `gpu_nvidia` / `gpu_amd`
/// 意图，调用对应的
/// 共享注入函数（[`inject_gui_passthrough`] / [`inject_gpu_passthrough`]）。
///
/// 供模板展开（`Flavor::build_config` / `ConfTemplate::build_config`）与实例
/// 应用/重建（`apply_container_config`）共用——保证「意图 → 注入产物」在任何把
/// 配置变成运行容器的路径上行为一致。
pub fn inject_passthrough(config: &mut ContainerConfig) {
    let (x11, wayland) = (config.params.gui_x11, config.params.gui_wayland);
    // 至少一半开启才调：apply 内部按意图**覆盖** GUI 意图 env（开→宿主值，关→置空），
    // 故关一半（如 x11 关 wayland 开）时仍会覆盖清空镜像里烘焙的旧 X11 值（commit
    // 快照把容器 env 烘进镜像、重建继承）。两半均关 = 非 GUI 容器，不调 apply（避免给
    // 纯 headless 容器写空 env）；此场景下镜像若残留旧 X11 env 不再主动清除（边缘情况）。
    if x11 || wayland {
        inject_gui_passthrough(&mut config.params, &mut config.env, x11, wayland);
    }
    if config.params.gpu_nvidia || config.params.gpu_amd {
        inject_gpu_passthrough(
            &mut config.env,
            config.params.gpu_nvidia,
            config.params.gpu_amd,
        );
    }
}

/// GUI 直通注入（共享）：按 [`super::gui`] 规则，从宿主探测 DISPLAY/WAYLAND/
/// XDG_RUNTIME_DIR + 字体/图标挂载，追加到 env 与 params.mounts，并恒开 keep-id。
/// 按 `x11`/`wayland` 两半各自注入（shared 任一半开启即注入）。
///
/// 「注入什么」（映射表 / XDG_DATA_DIRS 值 / keep-id）外置到资源文件
/// `ASSETS_DIR::gui-passthrough.yaml`（dev 源码树 / prod 数据目录），本函数只
/// 负责执行——详见 [`super::gui`]。
///
/// **`XAUTHORITY` 注入稳定间接路径**（`/run/easytidy/xauthority`，仅 X11 半）：
/// 真实 auth 文件含随机后缀、随登录会话轮换，不能写死；由容器内
/// `easytidy-server` 启动时探 `$XDG_RUNTIME_DIR` 下已知模式并维护软链指向真实文件
/// （周期重探重链，见 crates/server/src/setup.rs）。稳定路径进容器 spec Env，
/// 容器内所有进程（含 `podman exec` / `ets`）生效。
pub fn inject_gui_passthrough(
    params: &mut ContainerParams,
    env: &mut Vec<String>,
    x11: bool,
    wayland: bool,
) {
    crate::env::gui::apply(params, env, x11, wayland);
}

/// GPU env 注入（共享）：按 vendor 幂等追加对应 env。
///
/// - **NVIDIA**：开 → `NVIDIA_DRIVER_CAPABILITIES=all`（**不注** `NVIDIA_VISIBLE_DEVICES`——
///   GPU 设备经 CDI `nvidia.com/gpu=all` 注入，该变量是 legacy runtime 侧提示，与
///   nvidia hook 追加的 `NVIDIA_VISIBLE_DEVICES=void` 在容器 spec env 里成对冲突，
///   2026-09-10 实测去掉后 GPU 仍经 CDI 设备节点正常工作）
/// - **AMD**：开 → 无 vendor 专属 env（ROCm/VAAPI 容器内自检设备即可；设备节点
///   由 [`detect_amd_gpu_devices`] 在 create 时注入 libpod body，不走 env）
///
/// 本函数只负责 env 部分；GPU 设备节点注入：
/// - NVIDIA → libpod body 拼 `nvidia.com/gpu=all` CDI 引用（create 前经
///   [`detect_nvidia_gpu`] 预检：GPU 设备节点 + 宿主 nvidia-container-toolkit 的
///   CDI spec 就绪，否则报可读错误）
/// - AMD → create 前调 [`detect_amd_gpu_devices`] 探测裸设备（AMD 无自动 CDI
///   工具链，`amd.com/gpu=all` 在真实宿主恒 unresolvable → 不采用 CDI）
///
/// `extra_opts`（label=disable / apparmor=unconfined）属独立关切，保留显式
/// 声明（见 `ContainerParams::extra_opts`），此处不隐式注入。
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
    // 不注 `NVIDIA_VISIBLE_DEVICES`：GPU 设备经 CDI `nvidia.com/gpu=all` 注入，
    // 容器内访问走 /dev/nvidia* 设备节点，不依赖该 legacy runtime 侧 env（与
    // nvidia hook 追加的 `NVIDIA_VISIBLE_DEVICES=void` 冲突，2026-09-10 实测去掉后
    // GPU 仍正常）。
    if !existing_env_keys.contains("NVIDIA_DRIVER_CAPABILITIES") {
        env.push("NVIDIA_DRIVER_CAPABILITIES=all".to_string());
    }
    // AMD 无 vendor 专属 env（容器内自检 /dev/dri + /dev/kfd 即可）。
    // `_amd` 参数保留以备未来 AMD env 需求（ROCm 容器内自检即可，故暂无）。
    let _ = amd;
}

/// NVIDIA GPU 直通宿主就绪（create 期预检，与 AMD 探测同款）。
///
/// easytidy 用 CDI 引用 `nvidia.com/gpu=all` 注入 NVIDIA GPU 设备节点（非 legacy
/// env 机制），宿主需满足两件事：
/// 1. NVIDIA 驱动工作、GPU 设备节点存在（`/dev/nvidia0` 等）；
/// 2. nvidia-container-toolkit 已生成 CDI spec（`/etc/cdi/nvidia.yaml` 或
///    `/var/run/cdi/nvidia.yaml`），否则 podman 报 `unresolvable CDI devices
///    nvidia.com/gpu=all` 玄学报错。
///
/// create/rebuild 路径调用：`gpu_nvidia` 开但未就绪 → 直接报可读错误（避免静默
/// 无 GPU / podman 玄学报错）。easytidy 只预检提示、不代跑安装（root 操作由用户
/// 执行，同 AMD 的 kfd 预检哲学）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NvidiaGpuReadiness {
    /// 未探测到 GPU 设备节点（无 NVIDIA GPU 硬件，或驱动未装/未加载）
    NoGpu,
    /// GPU 在但 nvidia-container-toolkit CDI spec 缺失（未装 toolkit，或未跑
    /// `nvidia-ctk cdi generate`）
    MissingCdiSpec,
    /// 就绪（GPU + CDI spec）
    Ready,
}

/// 探测宿主 NVIDIA GPU 直通就绪（create 期调用）。
pub fn detect_nvidia_gpu() -> NvidiaGpuReadiness {
    detect_nvidia_gpu_at(
        Path::new("/dev"),
        &[
            Path::new("/etc/cdi/nvidia.yaml"),
            Path::new("/var/run/cdi/nvidia.yaml"),
        ],
    )
}

/// [`detect_nvidia_gpu`] 的纯函数实现（目录注入以便单测）。
///
/// - `dev_dir`：宿主设备节点目录（prod `/dev`）；探测 `nvidia<数字>` 节点（≥1 个）
/// - `cdi_candidates`：CDI spec 候选路径（任一存在即就绪）
fn detect_nvidia_gpu_at(dev_dir: &Path, cdi_candidates: &[&Path]) -> NvidiaGpuReadiness {
    // 1. GPU 设备节点：/dev/nvidia<N>（N 为纯数字，nvidia-smi 可见的每块 GPU 一个）
    let gpu_present = std::fs::read_dir(dev_dir)
        .map(|entries| {
            entries.filter_map(|e| e.ok()).any(|e| {
                e.file_name()
                    .to_string_lossy()
                    .strip_prefix("nvidia")
                    .map(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false);
    if !gpu_present {
        return NvidiaGpuReadiness::NoGpu;
    }
    // 2. CDI spec：任一候选路径存在
    if cdi_candidates.iter().any(|p| p.exists()) {
        NvidiaGpuReadiness::Ready
    } else {
        NvidiaGpuReadiness::MissingCdiSpec
    }
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

/// ROCm 计算入口 `/dev/kfd` 的可访问性（create 期预检，见 docs/17）。
///
/// rootless keep-id 剥除宿主 render 补充组，`/dev/kfd`（0660 root:render）无
/// 当前用户 ACL 时，容器进程被 DAC 拒（EACCES）——**设备透传是好的，但 ROCm
/// 计算（rocminfo/rocm-smi/HIP）不可用**。有效解法为宿主侧放行：
/// `sudo chmod 666 /dev/kfd`（docs/17 §3.1，多轮实测最有效）——宿主侧
/// 配置由用户执行，easytidy 只预检提示、不代跑 root 命令。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KfdAccess {
    /// /dev/kfd 不存在（宿主无 amdgpu compute）
    Absent,
    /// 当前用户可读写打开（0666 / owner / group / ACL 放行）
    Open,
    /// 存在但当前用户打不开（DAC 拒）——宿主权限位（如 0o660）
    Blocked { mode: u32 },
}

/// 以「尝试读写打开」为 ground truth 判定 [`KfdAccess`]（与 docs/17 §5 的容器内
/// 验证同法：宿主当前用户打不开 ⇔ keep-id 容器内进程同样被 DAC 拒）。
pub fn kfd_access() -> KfdAccess {
    kfd_access_at(Path::new("/dev/kfd"))
}

/// [`kfd_access`] 的纯函数实现（路径注入以便单测）。
fn kfd_access_at(path: &Path) -> KfdAccess {
    use std::os::unix::fs::PermissionsExt;
    let meta = match path.metadata() {
        Ok(m) => m,
        Err(_) => return KfdAccess::Absent,
    };
    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
    {
        Ok(_) => KfdAccess::Open,
        Err(_) => KfdAccess::Blocked {
            mode: meta.permissions().mode(),
        },
    }
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

    #[test]
    fn test_kfd_access_absent_open_blocked() {
        let tmp = std::env::temp_dir().join(format!("easytidy-kfd-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        // Absent：路径不存在
        assert_eq!(kfd_access_at(&tmp.join("no-such")), KfdAccess::Absent);

        // Open：普通文件（当前用户 owner rw）
        let open_file = tmp.join("kfd-open");
        std::fs::write(&open_file, "").unwrap();
        assert_eq!(kfd_access_at(&open_file), KfdAccess::Open);

        // Blocked：0000 模式——root 有 CAP_DAC_OVERRIDE 会照常打开，root 下跳过
        if unsafe { libc::getuid() } != 0 {
            let blocked_file = tmp.join("kfd-blocked");
            std::fs::write(&blocked_file, "").unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&blocked_file, std::fs::Permissions::from_mode(0o000))
                .unwrap();
            match kfd_access_at(&blocked_file) {
                KfdAccess::Blocked { mode } => {
                    assert_eq!(mode & 0o777, 0o000, "0000 模式（非 root）应打不开")
                }
                other => panic!("expect Blocked, got {other:?}"),
            }
        }

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
        // 只注 NVIDIA_DRIVER_CAPABILITIES（不注 NVIDIA_VISIBLE_DEVICES——GPU 走 CDI，
        // 该 legacy env 与 nvidia hook 的 void 冲突，2026-09-10 去掉）
        assert_eq!(env.len(), 1);
        assert!(env.contains(&"NVIDIA_DRIVER_CAPABILITIES=all".to_string()));
        assert!(!env.contains(&"NVIDIA_VISIBLE_DEVICES=all".to_string()));
    }

    #[test]
    fn test_inject_gpu_amd_no_env() {
        let mut env = Vec::new();
        inject_gpu_passthrough(&mut env, false, true);
        assert!(
            env.is_empty(),
            "AMD 不应注入 NVIDIA_* env（ROCm 容器内自检）"
        );
    }

    #[test]
    fn test_inject_gpu_both_vendors() {
        // 同时开 NVIDIA + AMD → 只有 NVIDIA env（AMD 无 vendor 专属 env）
        let mut env = Vec::new();
        inject_gpu_passthrough(&mut env, true, true);
        assert_eq!(env.len(), 1);
        assert!(env.contains(&"NVIDIA_DRIVER_CAPABILITIES=all".to_string()));
    }

    #[test]
    fn test_inject_gpu_idempotent() {
        let mut env = vec!["NVIDIA_DRIVER_CAPABILITIES=all".to_string()];
        inject_gpu_passthrough(&mut env, true, false);
        assert_eq!(env.len(), 1, "已有 key 不重复注入");
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
        // 仅 gpu_nvidia 开关时：env 应含 NVIDIA_*；mounts 由 gui 双开关决定（均关则空）
        let mut cfg = ContainerConfig {
            name: "t".into(),
            params: ContainerParams {
                image: "alpine".into(),
                gui_x11: false,
                gui_wayland: false,
                gpu_nvidia: true,
                gpu_amd: false,
                ..ContainerParams::default()
            },
            env: vec![],
            silent_boot: false,
            persistent: true,
            icon: None,
        };
        inject_passthrough(&mut cfg);
        // 只注 NVIDIA_DRIVER_CAPABILITIES（NVIDIA_VISIBLE_DEVICES 不注——GPU 走 CDI）
        assert!(cfg
            .env
            .iter()
            .any(|e| e == "NVIDIA_DRIVER_CAPABILITIES=all"));
        assert!(!cfg
            .env
            .iter()
            .any(|e| e.starts_with("NVIDIA_VISIBLE_DEVICES=")));
        assert_eq!(env_keys(&cfg.env).len(), 1);
    }

    /// NVIDIA GPU 直通预检三态（目录注入单测）。
    #[test]
    fn test_detect_nvidia_gpu_readiness() {
        let tmp = tempfile::tempdir().unwrap();
        let dev = tmp.path().join("dev");
        std::fs::create_dir_all(&dev).unwrap();
        let cdi_dir = tmp.path().join("etc").join("cdi");
        std::fs::create_dir_all(&cdi_dir).unwrap();
        let cdi_spec = cdi_dir.join("nvidia.yaml");
        let cdi_candidates: &[&std::path::Path] = &[cdi_spec.as_path()];

        // 无 GPU 设备节点 → NoGpu（即使 CDI spec 在）
        std::fs::write(&cdi_spec, b"kind: cdi").unwrap();
        assert_eq!(
            detect_nvidia_gpu_at(&dev, cdi_candidates),
            NvidiaGpuReadiness::NoGpu
        );

        // GPU 在但无 CDI spec → MissingCdiSpec
        std::fs::write(dev.join("nvidia0"), b"").unwrap();
        std::fs::remove_file(&cdi_spec).unwrap();
        assert_eq!(
            detect_nvidia_gpu_at(&dev, cdi_candidates),
            NvidiaGpuReadiness::MissingCdiSpec
        );

        // GPU + CDI spec → Ready
        std::fs::write(&cdi_spec, b"kind: cdi").unwrap();
        assert_eq!(
            detect_nvidia_gpu_at(&dev, cdi_candidates),
            NvidiaGpuReadiness::Ready
        );

        // 非数字后缀（nvidia-uvm / nvidiactl）不算 GPU 节点
        let dev2 = tmp.path().join("dev2");
        std::fs::create_dir_all(&dev2).unwrap();
        std::fs::write(dev2.join("nvidia-uvm"), b"").unwrap();
        std::fs::write(dev2.join("nvidiactl"), b"").unwrap();
        assert_eq!(
            detect_nvidia_gpu_at(&dev2, cdi_candidates),
            NvidiaGpuReadiness::NoGpu,
            "nvidia-uvm/nvidiactl 不应被当作 GPU 设备节点"
        );
    }

    #[test]
    fn test_inject_gui_split_dispatch() {
        // 双开关拆分 + 意图覆盖：均关 → 无 GUI env；x11 → XAUTHORITY 稳定路径 +
        // WAYLAND_DISPLAY 置空；wayland → DISPLAY/XAUTHORITY 置空（覆盖镜像烘焙旧值）。
        fn build(x11: bool, wayland: bool) -> ContainerConfig {
            ContainerConfig {
                name: "t".into(),
                params: ContainerParams {
                    image: "alpine".into(),
                    keep_id: false,
                    gui_x11: x11,
                    gui_wayland: wayland,
                    ..ContainerParams::default()
                },
                env: vec![],
                silent_boot: false,
                persistent: true,
                icon: None,
            }
        }
        /// 取 env 中某 key 的值（无 → None）。
        fn env_value<'a>(env: &'a [String], key: &str) -> Option<&'a str> {
            env.iter()
                .find(|e| e.starts_with(&format!("{key}=")))
                .map(|e| e.split_once('=').unwrap().1)
        }

        // 均关：非 GUI 容器不调 apply → 无 GUI env，不动 keep_id
        let mut cfg = build(false, false);
        inject_passthrough(&mut cfg);
        assert!(
            cfg.env.is_empty(),
            "gui 双开关均关不应注入 GUI env：{:?}",
            cfg.env
        );
        assert!(!cfg.params.keep_id, "均关时不应由 shared 段开 keep-id");

        // x11 半：XAUTHORITY 稳定路径 + shared XDG_DATA_DIRS；WAYLAND_DISPLAY 置空
        let mut cfg = build(true, false);
        inject_passthrough(&mut cfg);
        assert_eq!(
            env_value(&cfg.env, "XAUTHORITY"),
            Some("/run/easytidy/xauthority"),
            "x11 半应注入 XAUTHORITY 稳定路径：{:?}",
            cfg.env
        );
        assert!(
            cfg.env.iter().any(|e| e.starts_with("XDG_DATA_DIRS=")),
            "shared 段应注入 XDG_DATA_DIRS：{:?}",
            cfg.env
        );
        assert_eq!(
            env_value(&cfg.env, "WAYLAND_DISPLAY"),
            Some(""),
            "x11-only 应把 WAYLAND_DISPLAY 置空（覆盖镜像旧值）：{:?}",
            cfg.env
        );
        assert!(cfg.params.keep_id, "shared 段应恒开 keep-id");

        // wayland 半：DISPLAY / XAUTHORITY 置空（Wayland 不走 X11 auth）
        let mut cfg = build(false, true);
        inject_passthrough(&mut cfg);
        assert_eq!(
            env_value(&cfg.env, "XAUTHORITY"),
            Some(""),
            "wayland 半应把 XAUTHORITY 置空：{:?}",
            cfg.env
        );
        assert_eq!(
            env_value(&cfg.env, "DISPLAY"),
            Some(""),
            "wayland 半应把 DISPLAY 置空：{:?}",
            cfg.env
        );
        assert!(
            cfg.env.iter().any(|e| e.starts_with("XDG_DATA_DIRS=")),
            "shared 段应注入 XDG_DATA_DIRS：{:?}",
            cfg.env
        );
    }
}
