//! conf YAML 模板（模板体系新载体）。
//!
//! 数据域拆分后,模板的管理由 flavor(TOML) 收敛到 conf(YAML):
//! - **conf 模板** = 容器关键参数(意图)+ 可选 `setup` 安装命令(本轮只存不执行)
//! - 存放:双轨制目录(dev = `crates/gui/conf` 源码目录,prod = `~/.easytidy/conf`,
//!   经 GUI crate 的 `CONF_DIR` namespace 解析)——目录解析 + 文件 IO 的
//!   `ConfTemplateStore` 迁到 GUI 命令层(conf 域 GUI 独占:种子在 gui/conf、
//!   命令在 gui,core 不持有该路径)
//!
//! 与 [`Flavor`](crate::flavor::Flavor) 的关系:
//! - flavor 偏 CLI(创建时执行 setup 安装、带 gui 展开逻辑),GUI 模板 tab 不再使用
//! - 本模块只定义 [`ConfTemplate`] 类型与 `build_config`(展开语义,注入宿主
//!   GUI 透传);YAML ⇄ 类型转换与目录 IO 都在 GUI 命令层完成。

use serde::{Deserialize, Serialize};

use crate::env::inject_passthrough;
use crate::models::ContainerConfig;

/// conf YAML 模板：容器关键参数 + 可选安装命令(setup)。
///
/// `#[serde(flatten)]` 平铺 [`ContainerConfig`] 字段——YAML 形状与实例容器
/// 配置一致(旧 conf/*.yaml 文件直接兼容,setup 默认空)。
///
/// GUI / GPU 透传意图(`gui` / `gpu`)在 [`ContainerConfig`] 共享基座内
/// (见 `crate::models::ContainerParams`)——模板与实例共用同一份意图字段,
/// 故模板与实例的「容器」配置区都能编辑这两个开关。
///
/// 设计意图:YAML 存静态意图(镜像/entry/挂载/网络/用户映射 + gui/gpu + 可选 setup),
/// 宿主耦合的运行时数据(DISPLAY/WAYLAND/XAUTHORITY/XDG_RUNTIME_DIR)留到
/// [`ConfTemplate::build_config`] 实时探测注入——避免模板硬编 session 特有值。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfTemplate {
    /// 容器关键参数(flatten 平铺;name 即执行容器名,与 data 目录按名绑定;
    /// 含 `gui` / `gpu` 透传意图)
    #[serde(flatten)]
    pub config: ContainerConfig,
    /// 创建后按序执行的安装命令(本轮只存不执行;执行链路与 data 启动脚本
    /// 一起在下一步接入)
    #[serde(default)]
    pub setup: Vec<String>,
}

/// 模板列表项：[`ConfTemplate`] + 磁盘身份。
///
/// `#[serde(flatten)]` 平铺模板字段（与 `ConfTemplate` JSON 形状一致）,另加
/// `id` / `path`——仅运行时填充,供 GUI 展示与**按文件名定位**;`ConfTemplate`
/// 本身保持纯 YAML 形状（身份/路径不入文件,避免 `conf_save_template`
/// 序列化时污染）。
///
/// **身份语义（2026-09-02 修复）**：模板的文件 stem（`id`）才是稳定身份——
/// 不是 YAML 内 `config.name`（= 默认容器名，复制不重写时会撞名）。
/// GUI 增删改用 `id`，杜绝「删 copy 删到原文件」。
#[derive(Debug, Clone, Serialize)]
pub struct ConfTemplateInfo {
    #[serde(flatten)]
    pub template: ConfTemplate,
    /// 模板身份 = 文件名 stem（`chrome.yaml` → `chrome`；含 `.copy` 等后缀）。
    /// 增删改/展开一律用它定位文件（`<conf_dir>/<id>.yaml`）。
    pub id: String,
    /// 模板在磁盘上的绝对路径（`<conf_dir>/<id>.yaml`）
    pub path: String,
}

impl ConfTemplate {
    /// 模板展开为可创建的 [`ContainerConfig`]。
    ///
    /// 模板仅作**创建期预填**：展开为实例快照，容器创建后与模板彻底解耦
    /// （无血缘字段、无同步、无漂移检测）。
    ///
    /// - `name`：执行容器名（覆盖模板内 `config.name`）
    /// - 按 config 内的 gui/gpu 意图注入宿主透传（与实例 apply/重建路径共用）
    pub fn build_config(&self, name: &str) -> ContainerConfig {
        let mut config = self.config.clone();
        config.name = name.to_string();
        inject_passthrough(&mut config);
        config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ContainerParams;

    #[test]
    fn test_conf_template_serde_default_setup() {
        // serde 反序列化旧文件(无 setup 字段)时 setup 默认空——通过
        // JSON 平铺形状验证（与 YAML 同走 serde 数据模型）。
        // ContainerConfig 的 silent_boot/persistent 无 serde default，需显式给。
        let json = r#"{
            "name":"chrome","image":"docker.io/library/ubuntu:24.04",
            "entry":"google-chrome-stable","entry_args":[],
            "mounts":[],"network":{"mode":"host","ports":[]},"keep_id":true,
            "env":[],"silent_boot":false,"persistent":true
        }"#;
        let t: ConfTemplate = serde_json::from_str(json).unwrap();
        assert!(t.setup.is_empty());
        assert_eq!(t.config.name, "chrome");
        assert_eq!(t.config.params.image, "docker.io/library/ubuntu:24.04");
    }

    #[test]
    fn test_conf_template_flatten_shape() {
        // 序列化形状平铺：name/image 等 ContainerConfig 字段在最外层
        // （setup 空时可由 skip 或显式空数组；此处验证不嵌套出 config.*）。
        let t = ConfTemplate {
            config: ContainerConfig {
                name: "dev".to_string(),
                params: ContainerParams {
                    image: "docker.io/library/ubuntu:24.04".to_string(),
                    ..ContainerParams::default()
                },
                ..ContainerConfig::default()
            },
            setup: Vec::new(),
        };
        let v = serde_json::to_value(&t).unwrap();
        let obj = v.as_object().unwrap();
        assert_eq!(obj.get("name").unwrap(), "dev");
        assert_eq!(obj.get("image").unwrap(), "docker.io/library/ubuntu:24.04");
        assert!(obj.contains_key("setup"));
        // gui 在共享基座内,经 flatten 平铺到最外层(缺省 false)
        assert_eq!(obj.get("gui").unwrap(), false);
        // 不嵌套 config 键
        assert!(!obj.contains_key("config"));
    }

    #[test]
    fn test_conf_template_gui_field_roundtrip() {
        // gui 字段序列化/反序列化:旧文件无 gui 字段 → 默认 false;新文件含 gui
        // 字段 → 按设定值
        let json_old = r#"{
            "name":"chrome","image":"docker.io/library/ubuntu:24.04",
            "entry":"google-chrome-stable","entry_args":[],
            "mounts":[],"network":{"mode":"host","ports":[]},"keep_id":true,
            "env":[],"silent_boot":false,"persistent":true
        }"#;
        let t_old: ConfTemplate = serde_json::from_str(json_old).unwrap();
        assert!(!t_old.config.params.gui, "旧文件缺 gui 字段应默认 false");

        let json_new = r#"{
            "name":"chrome","image":"docker.io/library/ubuntu:24.04",
            "entry":"google-chrome-stable","entry_args":[],
            "mounts":[],"network":{"mode":"host","ports":[]},"keep_id":true,
            "env":[],"silent_boot":false,"persistent":true,
            "gui":true
        }"#;
        let t_new: ConfTemplate = serde_json::from_str(json_new).unwrap();
        assert!(t_new.config.params.gui, "新文件含 gui:true 应生效");
    }

    #[test]
    fn test_conf_template_gpu_expand() {
        // gpu_nvidia: true → params.gpu_nvidia + 注入 NVIDIA_* env
        let json = r#"{
            "name":"chrome","image":"docker.io/library/ubuntu:24.04",
            "entry":"google-chrome-stable","entry_args":[],
            "mounts":[],"network":{"mode":"host","ports":[]},"keep_id":true,
            "env":[],"silent_boot":false,"persistent":true,
            "gpu_nvidia":true
        }"#;
        let t: ConfTemplate = serde_json::from_str(json).unwrap();
        assert!(t.config.params.gpu_nvidia);
        let cfg = t.build_config("c1");
        assert!(cfg.params.gpu_nvidia);
        assert!(cfg.env.iter().any(|e| e == "NVIDIA_VISIBLE_DEVICES=all"));
        assert!(cfg.env.iter().any(|e| e == "NVIDIA_DRIVER_CAPABILITIES=all"));
        assert!(cfg.params.security_opts.is_empty(), "gpu 不应隐式加 security_opts");

        // gpu_amd: true → params.gpu_amd，无 NVIDIA_* env
        let json_a = r#"{
            "name":"c4","image":"alpine","entry_args":[],
            "mounts":[],"network":{"mode":"host","ports":[]},
            "env":[],"silent_boot":false,"persistent":true,
            "gpu_amd":true
        }"#;
        let ta: ConfTemplate = serde_json::from_str(json_a).unwrap();
        let cfga = ta.build_config("c4");
        assert!(cfga.params.gpu_amd);
        assert!(
            !cfga.env.iter().any(|e| e.starts_with("NVIDIA_")),
            "AMD 不应注入 NVIDIA_* env"
        );

        // 双开 → NVIDIA env + 两个 vendor 标志
        let json_both = r#"{
            "name":"c5","image":"alpine","entry_args":[],
            "mounts":[],"network":{"mode":"host","ports":[]},
            "env":[],"silent_boot":false,"persistent":true,
            "gpu_nvidia":true,"gpu_amd":true
        }"#;
        let tb: ConfTemplate = serde_json::from_str(json_both).unwrap();
        let cfgb = tb.build_config("c5");
        assert!(cfgb.params.gpu_nvidia);
        assert!(cfgb.params.gpu_amd);
        assert!(cfgb.env.iter().any(|e| e == "NVIDIA_VISIBLE_DEVICES=all"));

        // 未设 gpu_* → 不透传
        let json2 = r#"{
            "name":"c2","image":"alpine","entry_args":[],
            "mounts":[],"network":{"mode":"host","ports":[]},
            "env":[],"silent_boot":false,"persistent":true
        }"#;
        let t2: ConfTemplate = serde_json::from_str(json2).unwrap();
        assert!(!t2.config.params.gpu_nvidia);
        assert!(!t2.config.params.gpu_amd);
        let cfg2 = t2.build_config("c2");
        assert!(!cfg2.params.gpu_nvidia);
        assert!(!cfg2.params.gpu_amd);
        assert!(
            !cfg2.env.iter().any(|e| e.starts_with("NVIDIA_")),
            "无 gpu 时不应注入 NVIDIA env"
        );
    }
}