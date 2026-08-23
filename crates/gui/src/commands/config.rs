//! 配置管理器命令：读取（configfile + inspect）与应用（重建容器）+ YAML 桥 +
//! conf 模板管理（取代 GUI 侧的 flavor TOML 模板）。

use serde::Serialize;
use tracing::warn;

use easytidy_core::conf_template::{ConfTemplate, ConfTemplateStore};
use easytidy_core::configfile::ConfigFile;
use easytidy_core::models::ContainerConfig;
use easytidy_core::podman::Podman;

// ============================================================================
// 配置管理器命令（M4 前置：mount 管理 + 网络映射管理；改配置 = 重建容器）
// ============================================================================

/// 获取容器配置（configfile 期望配置 + podman inspect 当前生效状态 + 宿主用户
/// + 模板血缘状态）。
///
/// 返回（前端契约）：
/// ```json
/// { "config": { "name","image","entry","entry_args","silent_boot","persistent",
///                "mounts":[{"host_path","container_path","read_only"}],
///                "network":{"mode":"host"|"mapped","ports":[...]},
///                "env":["K=V"], "user_home":true, "flavor":"chrome"|null },
///   "effective": { "mounts":[...], "network":{...}, "env":["K=V"],
///                  "user":"0:0"|null, "userns_mode":"keep-id"|null },
///   "host_user": { "name","uid","gid","home" } | null,
///   "flavor_status": { "flavor":"chrome","exists":true,"drifted":false } | null }
/// ```
/// `effective` 为 `null` 表示容器尚未创建（仅 configfile 有记录）或 podman 不可达；
/// `host_user` 为 `null` 表示宿主用户探测失败（容器降级 root 运行，UI 需展示）；
/// `flavor_status` 为 `null` 表示自由创建（无血缘，不参与模板同步）。
#[tauri::command]
pub async fn get_container_config(name: String) -> Result<serde_json::Value, String> {
    // configfile 期望配置
    let config_path = ConfigFile::default_path().map_err(|e| format!("解析配置路径失败：{}", e))?;
    let config_file = ConfigFile::with_path(config_path);
    let config = config_file
        .get_container(&name)
        .map_err(|e| format!("读取容器配置失败：{}", e))?
        .ok_or_else(|| format!("容器配置不存在：{}", name))?;

    // 模板血缘状态（漂移检测：实例基座 vs 来源 conf 模板当前声明）。
    // 模板数据源：conf/<name>.yaml（GUI 全面切 YAML 后）。
    let flavor_status = conf_template_lineage_status(&config);

    // podman inspect 当前生效状态（容器未创建 / 连接失败时为 null）
    let effective = match Podman::connect().await {
        Ok(p) => match p.inspect_config(&name).await {
            Ok(view) => serde_json::to_value(view).map_err(|e| e.to_string())?,
            Err(e) => {
                warn!("获取容器 {} 生效配置失败：{}", name, e);
                serde_json::Value::Null
            }
        },
        Err(e) => {
            warn!("连接 podman 失败：{}", e);
            serde_json::Value::Null
        }
    };

    Ok(serde_json::json!({
        "config": serde_json::to_value(config).map_err(|e| e.to_string())?,
        "effective": effective,
        // 宿主用户（uid 映射语义对照表数据源；null = 探测失败，容器降级 root）
        "host_user": serde_json::to_value(easytidy_core::userenv::host_user()).map_err(|e| e.to_string())?,
        "flavor_status": flavor_status,
    }))
}

/// 应用容器配置（mounts / 网络映射变更 → 重建容器，重启后生效）。
///
/// `config` 即 `get_container_config` 返回的 `config` 对象（含 mounts/network）。
/// 返回新容器 ID。
#[tauri::command]
pub async fn apply_container_config(
    name: String,
    config: serde_json::Value,
) -> Result<String, String> {
    let mut container_config: ContainerConfig =
        serde_json::from_value(config).map_err(|e| format!("解析容器配置失败：{}", e))?;
    container_config.name = name.clone();

    // 直接调用 core（不依赖 GuiSession）：commit → 删旧 → 同名重建（新配置）→ 启动
    let podman = Podman::connect().await.map_err(|e| e.to_string())?;
    let new_id = podman
        .rebuild(&name, &container_config)
        .await
        .map_err(|e| e.to_string())?;

    // 更新 configfile（与重建后的容器保持一致）
    let config_path = ConfigFile::default_path().map_err(|e| format!("解析配置路径失败：{e}"))?;
    let config_file = ConfigFile::with_path(config_path);
    config_file
        .register_container(container_config)
        .map_err(|e| format!("更新容器配置失败：{e}"))?;

    Ok(new_id)
}

/// 从来源模板重新同步容器配置（ConfigManager「从模板同步」/ FlavorsPanel
/// 批量同步的底层动作）：从 conf 模板重新加载 → 保留实例侧字段（silent_boot/
/// persistent，setup 不参与同步，安装已发生）→ 重建容器 → 更新注册。
///
/// 返回同步提示（模板名 + 重建结果）。
#[tauri::command]
pub async fn config_sync_from_template(name: String) -> Result<String, String> {
    let config_path = ConfigFile::default_path().map_err(|e| format!("解析配置路径失败：{e}"))?;
    let config_file = ConfigFile::with_path(config_path);

    let current = config_file
        .get_container(&name)
        .map_err(|e| format!("读取容器配置失败：{e}"))?
        .ok_or_else(|| format!("容器配置不存在：{name}"))?;
    let template_name = current
        .flavor
        .clone()
        .ok_or_else(|| format!("容器 {name} 无血缘（非模板创建），不参与模板同步"))?;

    // 模板源: conf 目录 YAML(GUI 全面切 YAML 后)。`conf_template_expand` 走
    // 共享 inject_gui_passthrough:`gui: true` 时按当前宿主 env 实时注入
    // DISPLAY/WAYLAND/XAUTHORITY/XDG_RUNTIME_DIR——模板不硬编 session 特有值。
    let mut next: ContainerConfig = conf_template_expand(template_name.clone(), name.clone())
        .map_err(|e| format!("从模板 {template_name} 展开失败：{e}"))?;
    next.silent_boot = current.silent_boot;
    next.persistent = current.persistent;
    // flavor 已由 conf_template_expand 盖为 template_name,无需重复设

    let podman = Podman::connect().await.map_err(|e| format!("连接 podman 失败：{e}"))?;
    podman
        .rebuild(&name, &next)
        .await
        .map_err(|e| format!("重建容器失败：{e}"))?;
    config_file
        .register_container(next)
        .map_err(|e| format!("更新容器配置失败：{e}"))?;

    Ok(format!(
        "已从模板 {} 重新同步（容器已重建，实例自启/常驻设置保留）",
        template_name
    ))
}

/// 模板血缘状态（drift 检测）：读 conf 模板比对实例 params。
///
/// 与 `flavor::lineage_status` 等价语义：返回 `{ flavor, exists, drifted }`
/// 结构（保留 `flavor` 字段名以匹配前端 `FlavorStatus` 类型）；不存在时
/// exists=false 不再报漂移（删除模板保留血缘信息）。
fn conf_template_lineage_status(config: &ContainerConfig) -> Option<serde_json::Value> {
    let template_name = config.flavor.clone()?;
    let yaml = match ConfTemplateStore::read_yaml(&template_name) {
        Ok(y) => y,
        Err(_) => {
            return Some(serde_json::json!({
                "flavor": template_name, "exists": false, "drifted": false
            }));
        }
    };
    let template: ConfTemplate = match serde_yaml::from_str(&yaml) {
        Ok(t) => t,
        Err(e) => {
            warn!("解析模板 {template_name} 失败（drift 检测跳过）：{e}");
            return Some(serde_json::json!({
                "flavor": template_name, "exists": false, "drifted": false
            }));
        }
    };
    // 与 flavor 一致:仅比对 params(镜像/entry/挂载/网络/用户映射);
    // env 是宿主展开期快照,天然随会话变化,不参与漂移判定。
    let drifted = template.config.params != config.params;
    Some(serde_json::json!({
        "flavor": template_name, "exists": true, "drifted": drifted
    }))
}

// ============================================================================
// conf 模板管理（GUI 全面切 YAML 后取代 flavor TOML 模板）
// ============================================================================

/// 列出全部 conf 模板（解析为 ConfTemplate）。
///
/// 双目录语义：先 ensure_conf_examples()（首跑播种内置示例），再读
/// `~/.easytidy/conf/*.yaml` —— 用户新增/修改的模板一并出现（播种不覆盖）。
#[tauri::command]
pub fn conf_templates() -> Result<Vec<ConfTemplate>, String> {
    ensure_conf_examples();
    let dir = easytidy_core::appdata::conf_dir().map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    let entries = std::fs::read_dir(&dir)
        .map_err(|e| format!("读取模板目录失败：{e}"))?;
    for entry in entries.flatten() {
        let p = entry.path();
        let is_yaml = p
            .extension()
            .map(|e| e == "yaml" || e == "yml")
            .unwrap_or(false);
        if !is_yaml {
            continue;
        }
        let Ok(yaml) = std::fs::read_to_string(&p) else { continue };
        match serde_yaml::from_str::<ConfTemplate>(&yaml) {
            Ok(t) => out.push(t),
            Err(e) => warn!("模板 {:?} 解析失败：{e}", p.file_name()),
        }
    }
    out.sort_by(|a, b| a.config.name.cmp(&b.config.name));
    Ok(out)
}

/// 读单个 conf 模板（创建表单预填 / 编辑回显共用）。
#[tauri::command]
pub fn conf_template_get(name: String) -> Result<ConfTemplate, String> {
    let yaml = ConfTemplateStore::read_yaml(&name).map_err(|e| e.to_string())?;
    serde_yaml::from_str(&yaml).map_err(|e| format!("解析模板 {name} 失败：{e}"))
}

/// 模板展开 → 完整 ContainerConfig（创建表单预填 / 从模板同步共用）。
///
/// 与 [`conf_template_get`] 的区别:`get` 返回原始 ConfTemplate（含 `setup`/`gui` 标记）,
/// `expand` 返回可直接提交的 [`ContainerConfig`]——已按宿主实时 env 注入 GUI 透传
/// (`gui: true` 时),并盖 `flavor` 字段为模板名(血缘追溯)。
///
/// - `name`:模板文件名 stem(如 `chrome`)
/// - `container_name`:执行容器名(覆盖模板内 `config.name` 默认值)
#[tauri::command]
pub fn conf_template_expand(name: String, container_name: String) -> Result<ContainerConfig, String> {
    let yaml = ConfTemplateStore::read_yaml(&name).map_err(|e| e.to_string())?;
    let tpl: ConfTemplate =
        serde_yaml::from_str(&yaml).map_err(|e| format!("解析模板 {name} 失败：{e}"))?;
    let mut config = tpl.build_config(&container_name);
    // 血缘盖章 = 模板名(模板作者未在 YAML 显式写 flavor 时)
    if config.flavor.is_none() {
        config.flavor = Some(name);
    }
    Ok(config)
}

/// 写回 conf 模板（编辑 / 复制保存）。
#[tauri::command]
pub fn conf_save_template(template: ConfTemplate) -> Result<(), String> {
    let name = template.config.name.trim();
    if name.is_empty() {
        return Err("模板名不能为空".to_string());
    }
    let yaml = serde_yaml::to_string(&template).map_err(|e| format!("序列化 YAML 失败：{e}"))?;
    ConfTemplateStore::write_yaml(name, &yaml).map_err(|e| e.to_string())
}

/// 删除 conf 模板（`name` 含 `.yaml` 扩展或裸 stem 都可;统一按 stem 处理）。
#[tauri::command]
pub fn conf_rm_template(name: String) -> Result<(), String> {
    ConfTemplateStore::delete(name.trim()).map_err(|e| e.to_string())
}

/// 复制 conf 模板为新名（目标已存在时报错防覆盖，GUI 负责生成不冲突名）。
#[tauri::command]
pub fn conf_duplicate_template(from: String, to: String) -> Result<(), String> {
    ConfTemplateStore::duplicate(from.trim(), to.trim()).map_err(|e| e.to_string())
}

// ============================================================================
// 配置编辑器 YAML 桥（conf/*.yaml 模板 ⇄ 表单）
// ============================================================================

/// 「加载 YAML」对话框返回契约（前端表单预填）。
#[derive(Debug, Clone, Serialize)]
pub struct LoadConfResp {
    /// 选中的文件路径
    pub path: String,
    /// 解析后的容器配置（与 ContainerConfig 序列化形状一致，前端直接预填）
    pub config: serde_json::Value,
}

/// 内置示例模板条目（「示例模板」下拉/加载）。
#[derive(Debug, Clone, Serialize)]
pub struct ExampleConf {
    /// 模板名（文件 stem，如 dev / full / media / 用户自建名）
    pub name: String,
    /// YAML 原文（选中后经 `conf_parse` 解析预填）
    pub yaml: String,
}

/// 内置示例模板 —— `crates/gui/conf/*.yaml`，`include_str!` 编译期打进
/// 二进制（免 tauri bundle resources 配置与安装路径问题）。仅作播种来源：
/// 首跑写入 `~/.easytidy/conf/`，此后运行时目录为权威（用户可改/增）。
struct ConfSeed {
    name: &'static str,
    yaml: &'static str,
}

const CONF_SEEDS: [ConfSeed; 4] = [
    ConfSeed {
        name: "dev",
        yaml: include_str!("../../conf/container.dev.yaml"),
    },
    ConfSeed {
        name: "full",
        yaml: include_str!("../../conf/container.full.yaml"),
    },
    ConfSeed {
        name: "media",
        yaml: include_str!("../../conf/container.media.yaml"),
    },
    // 经典 Chrome 容器模板（展开自内置 flavor chrome）
    ConfSeed {
        name: "chrome",
        yaml: include_str!("../../conf/container.chrome.yaml"),
    },
];

/// 播种内置示例到指定目录（已存在不覆盖——用户改过的不碰，同
/// `flavor::ensure_presets` 语义）。
fn seed_conf_examples_to(dir: &std::path::Path) {
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    for seed in CONF_SEEDS.iter() {
        let path = dir.join(format!("{}.yaml", seed.name));
        if !path.exists() && std::fs::write(&path, seed.yaml).is_ok() {
            tracing::info!("内置示例配置已写入：{path:?}");
        }
    }
}

/// 播种到运行时目录（`~/.easytidy/conf`）——读列表命令惰性调用
/// （同 `flavor_list` 调 `ensure_presets` 的模式）。
fn ensure_conf_examples() {
    let Ok(dir) = easytidy_core::appdata::conf_dir() else {
        return;
    };
    seed_conf_examples_to(&dir);
}

/// 列出目录下所有 *.yaml 模板（含用户自建/修改的）。
fn conf_examples_from_dir(dir: &std::path::Path) -> Vec<ExampleConf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            let is_yaml = p
                .extension()
                .map(|e| e == "yaml" || e == "yml")
                .unwrap_or(false);
            if !is_yaml {
                continue;
            }
            let Some(name) = p.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
                continue;
            };
            let Ok(yaml) = std::fs::read_to_string(&p) else {
                continue;
            };
            out.push(ExampleConf { name, yaml });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// 解析 YAML 字符串为容器配置（示例模板 / 通用导入共用）。
///
/// 与 `env_new`/`apply_container_config` 走同一 `ContainerConfig` serde 模型
/// （`#[serde(flatten)]` 平铺形状，见 core `models.rs`）——YAML 字段是单一事实源的
/// 序列化视图，前端只过 JSON。
#[tauri::command]
pub fn conf_parse(text: String) -> Result<serde_json::Value, String> {
    let config: ContainerConfig = serde_yaml::from_str(&text)
        .map_err(|e| format!("配置格式解析失败：{e}"))?;
    serde_json::to_value(config).map_err(|e| format!("序列化配置失败：{e}"))
}

/// 「加载 YAML」：rfd 打开对话框 → 读盘 → 解析 → 返回表单预填数据。
///
/// 对话框默认定位到运行时模板目录（`~/.easytidy/conf`，双目录语义的 UI 落点），
/// 用户可自由跳转到其他位置（含源码侧 `crates/gui/conf/`）。
#[tauri::command]
pub async fn conf_load_dialog() -> Result<LoadConfResp, String> {
    // 打开对话框（阻塞；spawn_blocking 避免卡 async runtime）
    let picked = tokio::task::spawn_blocking(|| {
        let mut dialog = rfd::FileDialog::new().add_filter("YAML 配置", &["yaml", "yml"]);
        if let Ok(dir) = easytidy_core::appdata::conf_dir() {
            dialog = dialog.set_directory(dir);
        }
        dialog
            .pick_file()
            .map(|p| p.to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| format!("打开文件对话框失败：{e}"))?
    .ok_or_else(|| "已取消".to_string())?;

    let text = std::fs::read_to_string(&picked)
        .map_err(|e| format!("读取文件失败（{picked}）：{e}"))?;
    let config = conf_parse(text)?;
    Ok(LoadConfResp {
        path: picked,
        config,
    })
}

/// 「导出 YAML」:表单配置 → YAML → rfd 保存对话框 → 落盘。
/// 返回实际保存路径。
#[tauri::command]
pub async fn conf_save_dialog(config: ContainerConfig) -> Result<String, String> {
    let yaml = serde_yaml::to_string(&config).map_err(|e| format!("序列化 YAML 失败：{e}"))?;

    let file_name = if config.name.trim().is_empty() {
        "container.yaml".to_string()
    } else {
        format!("{}.yaml", config.name.trim())
    };
    let dest = tokio::task::spawn_blocking(move || {
        rfd::FileDialog::new()
            .set_file_name(&file_name)
            .add_filter("YAML 配置", &["yaml", "yml"])
            .save_file()
            .map(|p| p.to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| format!("打开保存对话框失败：{e}"))?
    .ok_or_else(|| "已取消".to_string())?;

    if let Some(parent) = std::path::Path::new(&dest).parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("创建目标目录失败（{parent:?}）：{e}"))?;
    }
    std::fs::write(&dest, yaml).map_err(|e| format!("写入文件失败（{dest}）：{e}"))?;
    tracing::info!("配置导出成功：{dest}");
    Ok(dest)
}

/// 内置示例模板列表（「示例模板」下拉数据源）。
///
/// 双目录语义：先播种内置示例到 `~/.easytidy/conf`（首跑），再列出该目录下
/// 所有 *.yaml——用户新增/修改的模板一并出现（播种不覆盖用户改动）。
#[tauri::command]
pub fn conf_examples() -> Result<Vec<ExampleConf>, String> {
    ensure_conf_examples();
    let dir = easytidy_core::appdata::conf_dir().map_err(|e| e.to_string())?;
    Ok(conf_examples_from_dir(&dir))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// 解析内置示例模板（锁定「conf/*.yaml 能被加载成功」契约）。
    fn parse_seed(seed: &ConfSeed) -> ContainerConfig {
        serde_yaml::from_str(seed.yaml)
            .unwrap_or_else(|e| panic!("内置示例 {} 应可解析为 ContainerConfig：{e}", seed.name))
    }

    #[test]
    fn test_conf_seeds_parse() {
        // 三个内置模板都解析成功 + 结构性不变量
        for seed in CONF_SEEDS.iter() {
            let config = parse_seed(seed);
            assert!(!config.name.trim().is_empty(), "{} 应含 name", seed.name);
            assert!(!config.params.image.trim().is_empty(), "{} 应含 image", seed.name);
            for m in &config.params.mounts {
                assert!(!m.host_path.trim().is_empty(), "{} 挂载缺 host_path", seed.name);
                assert!(!m.container_path.trim().is_empty(), "{} 挂载缺 container_path", seed.name);
            }
            for env in &config.env {
                assert!(env.contains('='), "{} 的 env 项应是 KEY=VALUE（得到「{env}」）", seed.name);
            }
        }
    }

    #[test]
    fn test_conf_parse_roundtrip() {
        // YAML → ContainerConfig → JSON（env_new 契约形状）→ 表单；
        // 且 ContainerConfig → YAML 可再解析回同一配置（导出→加载一致）
        for seed in CONF_SEEDS.iter() {
            let config = parse_seed(seed);
            let json = serde_json::to_value(&config).unwrap();

            let yaml = serde_yaml::to_string(&config).unwrap();
            let reloaded: ContainerConfig = serde_yaml::from_str(&yaml).unwrap();
            let reloaded_json = serde_json::to_value(&reloaded).unwrap();
            assert_eq!(
                json, reloaded_json,
                "{} 导出→回读应与原配置一致",
                seed.name
            );
        }
    }

    #[test]
    fn test_conf_parse_errors_readable() {
        // 缺必填字段（name/image 无 serde 默认值）→ 可读错误而非 panic
        let err = conf_parse("image: alpine:latest".to_string()).unwrap_err();
        assert!(err.contains("解析失败"), "缺 name 应报可读错误：{err}");

        // 非法 YAML → 可读错误
        let err = conf_parse("name: [unclosed".to_string()).unwrap_err();
        assert!(err.contains("解析失败"), "非法 YAML 应报可读错误：{err}");
    }

    #[test]
    fn test_conf_seeds_serviceable() {
        // env_new 的前置校验（name/image 非空）对三个内置模板都应通过
        let configs: Vec<ContainerConfig> = CONF_SEEDS.iter().map(parse_seed).collect();
        for config in &configs {
            assert!(!config.name.trim().is_empty());
            assert!(!config.params.image.trim().is_empty());
        }
    }

    #[test]
    fn test_seed_never_overwrites_user_edits() {
        // 播种幂等：已存在 = 用户改过，不覆盖（同 flavor::ensure_presets）
        let temp = TempDir::new().unwrap();
        seed_conf_examples_to(temp.path());
        assert_eq!(conf_examples_from_dir(temp.path()).len(), CONF_SEEDS.len());

        // 篡改一份 → 再播种 → 不被覆盖
        let dev_path = temp.path().join("dev.yaml");
        let custom = "name: my-dev\nimage: alpine\nsilent_boot: true\npersistent: false\n";
        std::fs::write(&dev_path, custom).unwrap();
        seed_conf_examples_to(temp.path());
        assert_eq!(
            std::fs::read_to_string(&dev_path).unwrap(),
            custom,
            "用户已存在的模板不应被内置示例覆盖"
        );
    }

    #[test]
    fn test_conf_examples_from_dir_lists_custom() {
        // 运行时目录含用户自建模板 → 一并列出且按名排序
        let temp = TempDir::new().unwrap();
        seed_conf_examples_to(temp.path());
        std::fs::write(
            temp.path().join("my-project.yaml"),
            "name: my-project\nimage: docker.io/library/archlinux:latest\nmounts: []\n",
        )
        .unwrap();
        std::fs::write(temp.path().join("notes.txt"), "not a yaml").unwrap();

        let list = conf_examples_from_dir(temp.path());
        let names: Vec<_> = list.iter().map(|e| e.name.clone()).collect();
        assert!(
            names.contains(&"my-project".to_string()),
            "用户自建模板应在列表中：{names:?}"
        );
        assert!(!names.contains(&"notes".to_string()), ".txt 不应混入：{names:?}");
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "列表应按名排序");
    }
}
