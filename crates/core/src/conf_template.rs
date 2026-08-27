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

use crate::flavor::inject_gui_passthrough;
use crate::models::ContainerConfig;

/// conf YAML 模板：容器关键参数 + 可选安装命令(setup) + GUI 透传开关。
///
/// `#[serde(flatten)]` 平铺 [`ContainerConfig`] 字段——YAML 形状与实例容器
/// 配置一致(旧 conf/*.yaml 文件直接兼容,setup/gui 默认空/false)。
///
/// 设计意图:YAML 存静态意图(镜像/entry/挂载/网络/用户映射 + 可选 setup),
/// 宿主耦合的运行时数据(DISPLAY/WAYLAND/XAUTHORITY/XDG_RUNTIME_DIR)留到
/// [`ConfTemplate::build_config`] 实时探测注入——避免模板硬编 session 特有值。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfTemplate {
    /// 容器关键参数(flatten 平铺;name 即执行容器名,与 data 目录按名绑定)
    #[serde(flatten)]
    pub config: ContainerConfig,
    /// GUI 透传(展开时注入宿主 DISPLAY/WAYLAND_DISPLAY/XAUTHORITY/XDG_RUNTIME_DIR
    /// + /tmp/.X11-unix 与 $XDG_RUNTIME_DIR 挂载 + 字体图标只读挂载 + keep_id=true)
    #[serde(default)]
    pub gui: bool,
    /// 创建后按序执行的安装命令(本轮只存不执行;执行链路与 data 启动脚本
    /// 一起在下一步接入)
    #[serde(default)]
    pub setup: Vec<String>,
}

impl ConfTemplate {
    /// 模板展开为可创建的 [`ContainerConfig`]。
    ///
    /// 与 [`crate::flavor::Flavor::build_config`] 对齐语义,区别是这里走
    /// 共享的 [`inject_gui_passthrough`](crate::flavor::inject_gui_passthrough)
    /// 注入逻辑(`gui: true` 时追加宿主 env/mounts)。
    ///
    /// - `name`:执行容器名(覆盖模板内 `config.name`)
    /// - `flavor` 字段盖章为模板名(`self.config.flavor` 已是 None;若用户保存
    ///   时填了 flavor 仍以填的为准——但模板作者一般不会在 YAML 里写 flavor)
    pub fn build_config(&self, name: &str) -> ContainerConfig {
        let mut config = self.config.clone();
        config.name = name.to_string();
        if self.gui {
            inject_gui_passthrough(&mut config.params, &mut config.env);
        }
        // 血缘盖章 = 模板名(若模板作者没填则使用文件 stem 命名约定;
        // 调用方负责在 conf_template_expand 同步盖章)
        if config.flavor.is_none() {
            // 模板本身 YAML 里写的 flavor 优先,无则填"模板即自己"的标记 —
            // 由调用方在 expand 时按文件 stem 写入(便于 config_sync_from_template
            // 按 conf YAML 重展开重建)
        }
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
        assert_eq!(t.config.flavor, None);
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
            gui: false,
            setup: Vec::new(),
        };
        let v = serde_json::to_value(&t).unwrap();
        let obj = v.as_object().unwrap();
        assert_eq!(obj.get("name").unwrap(), "dev");
        assert_eq!(obj.get("image").unwrap(), "docker.io/library/ubuntu:24.04");
        assert!(obj.contains_key("setup"));
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
        assert!(!t_old.gui, "旧文件缺 gui 字段应默认 false");

        let json_new = r#"{
            "name":"chrome","image":"docker.io/library/ubuntu:24.04",
            "entry":"google-chrome-stable","entry_args":[],
            "mounts":[],"network":{"mode":"host","ports":[]},"keep_id":true,
            "env":[],"silent_boot":false,"persistent":true,
            "gui":true
        }"#;
        let t_new: ConfTemplate = serde_json::from_str(json_new).unwrap();
        assert!(t_new.gui, "新文件含 gui:true 应生效");
    }
}