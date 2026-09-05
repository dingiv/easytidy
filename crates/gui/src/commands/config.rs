//! 配置管理器命令：读取（configfile + inspect）与应用（重建容器）+ YAML 桥 +
//! conf 模板管理（取代 GUI 侧的 flavor TOML 模板）。

use serde::Serialize;
use tracing::warn;

use easytidy_core::conf_template::{ConfTemplate, ConfTemplateInfo};
use easytidy_core::configfile::ConfigFile;
use easytidy_core::env::inject_passthrough;
use easytidy_core::models::{ContainerConfig, MountConfig};
use easytidy_core::podman::Podman;
use easytidy_protocol::{ServerEnv, ServerEnvItem, ServerEnvResp};

use crate::commands::socket::send_json_request;
use crate::state::GuiSession;

// ============================================================================
// 配置管理器命令（M4 前置：mount 管理 + 网络映射管理；改配置 = 重建容器）
// ============================================================================

/// 获取容器配置（configfile 期望配置 + podman inspect 当前生效状态 + 宿主用户）。
///
/// 返回（前端契约）：
/// ```json
/// { "config": { "name","image","entry","entry_args","silent_boot","persistent",
///                "mounts":[{"host_path","container_path","read_only"}],
///                "network":{"mode":"host"|"mapped","ports":[...]},
///                "env":["K=V"], "user_home":true },
///   "effective": { "mounts":[...], "network":{...}, "env":["K=V"],
///                  "user":"0:0"|null, "userns_mode":"keep-id"|null },
///   "host_user": { "name","uid","gid","home" } | null }
/// ```
/// `effective` 为 `null` 表示容器尚未创建（仅 configfile 有记录）或 podman 不可达；
/// `host_user` 为 `null` 表示宿主用户探测失败（容器降级 root 运行，UI 需展示）。
#[tauri::command]
pub async fn get_container_config(name: String) -> Result<serde_json::Value, String> {
    // configfile 期望配置
    let config_file =
        ConfigFile::default_instance().map_err(|e| format!("解析容器配置目录失败：{e}"))?;
    let config = config_file
        .get_container(&name)
        .map_err(|e| format!("读取容器配置失败：{}", e))?
        .ok_or_else(|| format!("容器配置不存在：{}", name))?;

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
    }))
}

/// 应用容器配置（mounts / 网络映射变更 → 快速重建容器，重启后生效）。
///
/// `config` 即 `get_container_config` 返回的 `config` 对象（含 mounts/network）。
/// 走快速重建（普通 commit + 安全流程：保留旧容器、失败自动回滚）。返回新容器 ID。
#[tauri::command]
pub async fn apply_container_config(
    name: String,
    config: serde_json::Value,
) -> Result<String, String> {
    let mut container_config: ContainerConfig =
        serde_json::from_value(config).map_err(|e| format!("解析容器配置失败：{}", e))?;
    container_config.name = name.clone();

    // 按 gui/gpu 意图注入宿主透传（幂等；已展开过的配置重复调用安全）——
    // 让实例配置区里切换「GUI 透传 / GPU 透传」开关后,重建即生效。
    inject_passthrough(&mut container_config);

    // 直接调用 core（不依赖 GuiSession）：快速重建（普通 commit + 安全流程）
    // commit → 保留旧容器 → 同名重建（新配置）→ 确认就绪 → 删旧（失败自动回滚）
    let podman = Podman::connect().await.map_err(|e| e.to_string())?;
    let bins = easytidy_core::ContainerBins::resolve().map_err(|e| e.to_string())?;
    let new_id = podman
        .rebuild_quick(&name, &container_config, &bins)
        .await
        .map_err(|e| e.to_string())?;

    // 容器内准备（fontconfig + 可选 useradd）：失败不阻断重建（落日志）
    if let Err(e) = podman.prepare_container(&name, &container_config.params).await {
        tracing::error!("容器内准备失败（{name}）：{e}");
    }

    // 更新 configfile（与重建后的容器保持一致）
    let config_file =
        ConfigFile::default_instance().map_err(|e| format!("解析容器配置目录失败：{e}"))?;
    config_file
        .register_container(container_config)
        .map_err(|e| format!("更新容器配置失败：{e}"))?;

    Ok(new_id)
}

// ============================================================================
// conf 模板管理（GUI 全面切 YAML 后取代 flavor TOML 模板）
// ============================================================================

/// conf 模板目录（双轨制读写）：dev = `crates/gui/conf`（本 crate manifest + `conf`，
/// 即源码种子目录——GUI 改动即改源文件、提交即发布）；prod = `~/.easytidy/conf`
/// （用户配置目录，首跑播种不覆盖）。解析走本 crate 声明的 `CONF_DIR` namespace
/// （gui/Cargo.toml `[package.metadata.shared]`），`is_dev()` = 运行期 env 含
/// `CARGO_MANIFEST_DIR`（cargo run / cargo test / tauri dev 成立；安装二进制不含 → prod）。
fn conf_dir() -> Result<std::path::PathBuf, String> {
    let dir = easytidy_shared::loader!()
        .resolve("CONF_DIR::")
        .ok_or_else(|| "未配置 CONF_DIR namespace".to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建配置模板目录失败：{e}"))?;
    Ok(dir)
}

/// 从模板文件路径取身份 stem（`chrome.copy.yaml` → `chrome.copy`）。
/// 列表展示的 `id` 即此值——GUI 增删改用它定位文件，与 YAML 内 `config.name`
/// 解耦（后者可能因历史复制不改名而撞车）。
fn template_stem(p: &std::path::Path) -> &str {
    p.file_stem().and_then(|s| s.to_str()).unwrap_or("")
}

/// 简易 IO 句柄：读写 conf 模板文件（YAML 文本存取，解析在本命令层）。
///
/// 目录走 [`conf_dir`]（双轨制）。从 core 迁入（2026-08-26）：conf 域 GUI 独占
/// ——种子在 `crates/gui/conf`、命令在 GUI，core 不再持有该路径。
struct ConfTemplateStore;

impl ConfTemplateStore {
    /// 规范化模板名 → 文件名 stem：`chrome`/`chrome.yaml`/`chrome.yml` → `chrome`。
    ///
    /// 模板身份 = 文件 stem（非 YAML 内 `config.name`）。防止调用方带扩展名
    /// 导致 `chrome.yaml.yaml` 这类错位路径。
    fn stem(name: &str) -> String {
        let n = name.trim();
        let stem = n
            .strip_suffix(".yaml")
            .or_else(|| n.strip_suffix(".yml"))
            .unwrap_or(n);
        stem.to_string()
    }

    /// 读模板文件原文（YAML 字符串）。模板不存在 → Err。
    fn read_yaml(name: &str) -> Result<String, String> {
        Self::read_yaml_in(&conf_dir()?, name)
    }

    /// 在指定目录下读（测试注入临时目录用；逻辑与 [`Self::read_yaml`] 一致）。
    fn read_yaml_in(dir: &std::path::Path, name: &str) -> Result<String, String> {
        let stem = Self::stem(name);
        let path = dir.join(format!("{stem}.yaml"));
        if !path.exists() {
            return Err(format!("模板不存在：{stem}"));
        }
        std::fs::read_to_string(&path).map_err(|e| format!("读取模板 {stem} 失败：{e}"))
    }

    /// 写回模板（原子写；目录由 conf_dir 确保存在）。文件名 = stem（与
    /// 编辑器 nameLocked 一致：`config.name` == stem，二者不分离）。
    fn write_yaml(name: &str, yaml: &str) -> Result<(), String> {
        let dir = conf_dir()?;
        Self::write_yaml_in(&dir, name, yaml)
    }

    fn write_yaml_in(dir: &std::path::Path, name: &str, yaml: &str) -> Result<(), String> {
        let stem = Self::stem(name);
        let target = dir.join(format!("{stem}.yaml"));
        let tmp = target.with_extension("yaml.tmp");
        std::fs::write(&tmp, yaml).map_err(|e| format!("写入模板 {stem} 临时文件失败：{e}"))?;
        std::fs::rename(&tmp, &target).map_err(|e| format!("保存模板 {stem} 失败：{e}"))
    }

    /// 删除模板文件（按 stem 定位，与列表展示的 `id` 一致）。
    fn delete(name: &str) -> Result<(), String> {
        Self::delete_in(&conf_dir()?, name)
    }

    fn delete_in(dir: &std::path::Path, name: &str) -> Result<(), String> {
        let stem = Self::stem(name);
        let path = dir.join(format!("{stem}.yaml"));
        if !path.exists() {
            return Err(format!("模板不存在：{stem}"));
        }
        std::fs::remove_file(&path).map_err(|e| format!("删除模板 {stem} 失败：{e}"))
    }

    /// 复制模板为新的名字（`to` 已存在时报错防覆盖，同 flavor::duplicate）。
    ///
    /// **复制即改写内部 `config.name` = 新 stem**（2026-09-02 修复）：原实现
    /// 逐字节拷贝，新文件的 `config.name` 仍是源名（如 `chrome`）→ 列表出现
    /// 两个同名模板、删除按名撞到源文件、复制品「使用」预填的容器名与源冲突。
    /// 改写后 `config.name` == stem，模板身份与默认容器名始终对齐。
    fn duplicate(from: &str, to: &str) -> Result<(), String> {
        Self::duplicate_in(&conf_dir()?, from, to)
    }

    fn duplicate_in(dir: &std::path::Path, from: &str, to: &str) -> Result<(), String> {
        let from_stem = Self::stem(from);
        let to_stem = Self::stem(to);
        if to_stem.is_empty() || to_stem == from_stem {
            return Err("复制目标名无效".to_string());
        }
        let dst = dir.join(format!("{to_stem}.yaml"));
        if dst.exists() {
            return Err(format!("模板 {to_stem} 已存在，不能覆盖"));
        }
        let src = dir.join(format!("{from_stem}.yaml"));
        let yaml = std::fs::read_to_string(&src)
            .map_err(|e| format!("读取模板 {from_stem} 失败：{e}"))?;
        let mut tpl: ConfTemplate =
            serde_yaml::from_str(&yaml).map_err(|e| format!("解析模板 {from_stem} 失败：{e}"))?;
        tpl.config.name = to_stem.clone();
        let out = serde_yaml::to_string(&tpl)
            .map_err(|e| format!("序列化模板 {to_stem} 失败：{e}"))?;
        Self::write_yaml_in(dir, &to_stem, &out)
    }
}

/// 列出全部 conf 模板（解析为 ConfTemplate + 磁盘路径）。
///
/// 双目录语义：先 ensure_conf_examples()（首跑播种内置示例），再读
/// 「conf 目录」（双轨制：dev = `crates/gui/conf`，prod = `~/.easytidy/conf`）下的
/// `*.yaml` —— 用户新增/修改的模板一并出现（播种不覆盖用户改动）。
///
/// 返回 [`ConfTemplateInfo`]（模板字段 + `path`），GUI 据此展示每个模板的文件位置。
#[tauri::command]
pub fn conf_templates() -> Result<Vec<ConfTemplateInfo>, String> {
    ensure_conf_examples();
    let dir = conf_dir().map_err(|e| e.to_string())?;
    let mut out: Vec<ConfTemplateInfo> = Vec::new();
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
            Ok(t) => out.push(ConfTemplateInfo {
                template: t,
                id: template_stem(&p).to_string(),
                path: p.to_string_lossy().into_owned(),
            }),
            Err(e) => warn!("模板 {:?} 解析失败：{e}", p.file_name()),
        }
    }
    out.sort_by(|a, b| a.template.config.name.cmp(&b.template.config.name));
    Ok(out)
}

/// 读单个 conf 模板（创建表单预填 / 编辑回显共用）。
#[tauri::command]
pub fn conf_template_get(name: String) -> Result<ConfTemplate, String> {
    let yaml = ConfTemplateStore::read_yaml(&name).map_err(|e| e.to_string())?;
    serde_yaml::from_str(&yaml).map_err(|e| format!("解析模板 {name} 失败：{e}"))
}

/// 模板展开 → 完整 ContainerConfig（创建表单预填）。
///
/// 与 [`conf_template_get`] 的区别:`get` 返回原始 ConfTemplate（含 `setup` 等模板字段）,
/// `expand` 返回可直接提交的 [`ContainerConfig`]——已按宿主实时 env 注入 GUI/GPU 透传
/// (config 内 `gui`/`gpu` 意图开启时)。模板仅作创建期预填，容器创建后与模板解耦
/// （无血缘字段、无同步、无漂移检测）。
///
/// - `name`:模板文件名 stem(如 `chrome`)
/// - `container_name`:执行容器名(覆盖模板内 `config.name` 默认值)
#[tauri::command]
pub fn conf_template_expand(name: String, container_name: String) -> Result<ContainerConfig, String> {
    let yaml = ConfTemplateStore::read_yaml(&name).map_err(|e| e.to_string())?;
    let tpl: ConfTemplate =
        serde_yaml::from_str(&yaml).map_err(|e| format!("解析模板 {name} 失败：{e}"))?;
    Ok(tpl.build_config(&container_name))
}

/// GUI + GPU 透传预览：给定配置（含 `gui`/`gpu` 意图与当前 mounts/env），返回
/// 展开/重建时引擎会**新增**的环境变量与挂载（隐式注入的透明化展示）。
///
/// 前端「容器」配置区「GUI 透传」/「GPU 透传」开启时调用（模板编辑器与实例配置
/// 管理器共用）：把返回项以只读行展示在挂载/环境变量面板，让用户看见引擎将
/// 隐式注入什么：
/// - gui：X11/Wayland socket、$XDG_RUNTIME_DIR、字体图标挂载 +
///   DISPLAY/WAYLAND/XDG_RUNTIME_DIR/XDG_DATA_DIRS env
/// - gpu：NVIDIA_VISIBLE_DEVICES/NVIDIA_DRIVER_CAPABILITIES env（设备节点经
///   `nvidia.com/gpu=<值>` CDI 引用，由 `params.gpu` 驱动）
///
/// 返回的是**增量**：配置里已声明的同 destination 挂载 / 同 key 环境变量不重复出现
/// （与两个 inject 函数的幂等去重一致——显式写的优先，引擎不再覆盖）。
/// 宿主耦合值（DISPLAY 等）实时探测，与 `conf_template_expand` 展开结果完全一致。
#[derive(Debug, Clone, Serialize)]
pub struct PassthroughPreview {
    /// 展开时注入的环境变量（"KEY=VALUE"；宿主实时探测值）
    pub env: Vec<String>,
    /// 展开时注入的挂载（仅 gui 产生）
    pub mounts: Vec<MountConfig>,
}

#[tauri::command]
pub fn passthrough_preview(config: ContainerConfig) -> Result<PassthroughPreview, String> {
    let mut container = config;
    // 展开前的去重键（挂载按 container_path、env 按 key——与 inject 函数一致）
    let before_mount_targets: std::collections::HashSet<String> = container
        .params
        .mounts
        .iter()
        .map(|m| m.container_path.clone())
        .collect();
    let before_env_keys: std::collections::HashSet<String> = container
        .env
        .iter()
        .filter_map(|kv| kv.split_once('=').map(|(k, _)| k.to_string()))
        .collect();

    // gui/gpu 意图在 config 基座内（与实例 apply 路径共用 inject_passthrough）
    inject_passthrough(&mut container);

    let mounts = container
        .params
        .mounts
        .iter()
        .filter(|m| !before_mount_targets.contains(&m.container_path))
        .cloned()
        .collect();
    let env = container
        .env
        .iter()
        .filter(|kv| match kv.split_once('=') {
            Some((k, _)) => !before_env_keys.contains(k),
            None => true,
        })
        .cloned()
        .collect();
    Ok(PassthroughPreview { env, mounts })
}

/// 查询容器内 server 运行时注入的环境变量（`server.env`）。
///
/// 与 [`passthrough_preview`] 相对：preview 是**宿主侧**按 gui/gpu 开关可预知的
/// create-time 注入；本命令是**容器内 server** 启动时探测/修正的 session 耦合值
/// （XAUTHORITY 自动探测、XDG_DATA_DIRS 系统默认修正）——配置里定义不了、宿主
/// 侧也无法预知最终值。配置管理器据此展示「easytidy 注入」只读 env 行。
///
/// 非单容器模式 / 容器未运行 → 返回空列表（前端据此不显示此类行）。
#[tauri::command]
pub async fn server_injected_env(
    session: tauri::State<'_, Option<GuiSession>>,
) -> Result<Vec<ServerEnvItem>, String> {
    let sess = session
        .inner()
        .as_ref()
        .ok_or_else(|| "当前模式不是单容器模式".to_string())?;

    let resp = send_json_request(
        sess,
        "server.env".to_string(),
        serde_json::to_value(ServerEnv).map_err(|e| e.to_string())?,
    )
    .await
    .map_err(|e| e.to_string())?;

    let body: ServerEnvResp = serde_json::from_value(resp.payload)
        .map_err(|e| format!("解析 server.env 响应失败：{e}"))?;
    Ok(body.env)
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

/// 内置示例模板 —— `crates/gui/assets/*.eg.yaml`，`include_str!` 编译期打进
/// 二进制（免 tauri bundle resources 配置与安装路径问题）。**仅作播种来源**：
/// 首跑写入运行时 conf 目录（dev = `crates/gui/conf`，prod = `~/.easytidy/conf`），
/// 播种时**去掉 `.eg` 标志**（`chrome.eg.yaml` → `chrome.yaml`），此后运行时
/// 目录为权威（用户可改/增，播种不覆盖）。
struct ConfSeed {
    /// 播种后的文件名 stem（= 模板身份；不含 `.eg`）
    name: &'static str,
    yaml: &'static str,
}

const CONF_SEEDS: [ConfSeed; 2] = [
    ConfSeed {
        name: "full",
        yaml: include_str!("../../assets/full.eg.yaml"),
    },
    // 经典 Chrome 容器模板示例（展开自内置 flavor chrome）
    ConfSeed {
        name: "chrome",
        yaml: include_str!("../../assets/chrome.eg.yaml"),
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
    let Ok(dir) = conf_dir() else {
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
        if let Ok(dir) = conf_dir() {
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

/// 挂载宿主路径输入建议:用户输入路径前缀 → 列出该路径下匹配的文件/目录条目。
///
/// 解析策略:沿输入路径向上找到第一个存在的祖先目录作为 `base`,用最后一
/// 段作为 `partial` 前缀过滤 `base` 下条目(目录优先,字母序)。
/// - `/home/user/data` 存在 → 列 `/home/user/data` 全部条目
/// - `/home/user/data/x` (`data` 不存在) → 列 `/home/user` 下 `data*` 条目
/// - 输入完全无效 → fallback 到 `$HOME` 全列
///
/// 用于挂载行宿主路径 AutoComplete 实时建议——`AutoComplete.onSearch` 每键入
/// 触发一次,内核 `read_dir` + 内存过滤,host 端操作,百级条目亚毫秒。
#[derive(Debug, Clone, serde::Serialize)]
pub struct HostEntry {
    /// 条目名(仅最后一段,无父路径)
    pub name: String,
    /// 完整路径(直接可用作 MountConfig.host_path)
    pub full_path: String,
    /// 是否目录(true → 路径补 `/` 提示;false → 文件)
    pub is_dir: bool,
}

#[tauri::command]
pub async fn list_host_path_suggestions(prefix: String) -> Result<Vec<HostEntry>, String> {
    tokio::task::spawn_blocking(move || {
        let trimmed = prefix.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }
        let path = std::path::Path::new(trimmed);

        // 找最长现存祖先目录 + 末段前缀
        let (base_dir, partial): (std::path::PathBuf, String) = if path.is_dir() {
            (path.to_path_buf(), String::new())
        } else {
            // 向上找第一个现存祖先
            let mut cursor = path.to_path_buf();
            let mut partial = String::new();
            let mut found_dir: Option<std::path::PathBuf> = None;
            loop {
                if let Some(parent) = cursor.parent() {
                    if parent.is_dir() {
                        partial = cursor
                            .file_name()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        found_dir = Some(parent.to_path_buf());
                        break;
                    }
                    cursor = parent.to_path_buf();
                } else {
                    break;
                }
            }
            match found_dir {
                Some(d) => (d, partial),
                // 全部祖先都不存在:fallback 到 $HOME 全列(用户总能浏览自己 home)
                None => (
                    std::env::var("HOME")
                        .map(std::path::PathBuf::from)
                        .unwrap_or_else(|_| std::path::PathBuf::from("/")),
                    String::new(),
                ),
            }
        };

        let mut entries = Vec::new();
        let read = std::fs::read_dir(&base_dir).map_err(|e| {
            format!("读取目录 {} 失败：{}", base_dir.display(), e)
        })?;
        for e in read.flatten() {
            // 跳过隐藏文件(避免 . / .. / .git 等噪声)
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            if !partial.is_empty() && !name.to_lowercase().starts_with(&partial.to_lowercase()) {
                continue;
            }
            let full = e.path();
            entries.push(HostEntry {
                name: name.clone(),
                full_path: full.to_string_lossy().into_owned(),
                is_dir: full.is_dir(),
            });
        }
        // 排序:目录优先,然后字母序(大小写不敏感)
        entries.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        // 截断到合理上限,避免前段渲染过万项(AutoComplete virtual 也会 cap)
        entries.truncate(200);
        Ok(entries)
    })
    .await
    .map_err(|e| format!("列目录建议失败：{e}"))?
}

/// 「选择挂载宿主机目录」:rfd 目录选择对话框,默认起点 = `$HOME`
/// (或调用方传入的 `initial`——若该路径存在且是目录则采用)。
///
/// 取消时返回 `Ok(String::new())`(前端据此识别"未选择")——错误(底层失败)
/// 走 `Err`。返回值直接作为 MountConfig.host_path 字段。
#[tauri::command]
pub async fn mount_pick_host_dir(initial: Option<String>) -> Result<String, String> {
    let initial_dir = initial
        .filter(|s| !s.trim().is_empty())
        .or_else(|| std::env::var("HOME").ok())
        .unwrap_or_else(|| "/".to_string());
    let picked = tokio::task::spawn_blocking(move || {
        let mut dialog = rfd::FileDialog::new();
        // rfd::FileDialog::set_directory 需要路径存在且是目录;否则报错
        // (我们不希望脏 host_path 字段引发 host-side 错误,直接回退到 $HOME)。
        let p = std::path::Path::new(&initial_dir);
        if p.is_dir() {
            dialog = dialog.set_directory(p);
        } else if let Ok(home) = std::env::var("HOME") {
            let h = std::path::Path::new(&home);
            if h.is_dir() {
                dialog = dialog.set_directory(h);
            }
        }
        dialog
            .pick_folder()
            .map(|p| p.to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| format!("打开目录选择对话框失败：{e}"))?;
    Ok(picked.unwrap_or_default())
}

/// 宿主常用用户资源目录（下载/文档/桌面/图片/音乐/视频）。
///
/// 经 `xdg-user-dir` 探测真实路径（兼容本地化命名）。GUI 挂载面板「快捷添加」
/// 据此渲染每目录一个按钮：点击 → `onAdd({ host_path, container_path:
/// "${HOME}/<folder>", read_only: false })`（容器侧 `${HOME}` 运行时展开为容器
/// 用户 home，复用 core 路径变量能力）。
#[tauri::command]
pub async fn list_user_resource_dirs() -> Result<Vec<easytidy_core::desktop::XdgUserDir>, String> {
    tokio::task::spawn_blocking(easytidy_core::desktop::host_user_resource_dirs)
        .await
        .map_err(|e| format!("探测宿主用户资源目录失败：{e}"))
}

/// 内置示例模板列表（「示例模板」下拉数据源）。
///
/// 双目录语义：先播种内置示例到「conf 目录」（双轨制：dev = `crates/gui/conf`，
/// prod = `~/.easytidy/conf`），再列出该目录下所有 *.yaml——用户新增/修改的
/// 模板一并出现（播种不覆盖用户改动）。
#[tauri::command]
pub fn conf_examples() -> Result<Vec<ExampleConf>, String> {
    ensure_conf_examples();
    let dir = conf_dir().map_err(|e| e.to_string())?;
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

    /// 透传预览的增量语义（确定性，不依赖宿主 DISPLAY 等）：
    /// - 已声明的挂载（同 container_path）→ 引擎幂等去重，预览不再重复
    /// - 未声明的 → 引擎注入（gui 开时 /tmp/.X11-unix 恒注入），预览应含
    /// - gpu_nvidia → NVIDIA_* env 增量（宿主无关）；gpu_amd → 无 env 注入
    #[test]
    fn test_passthrough_preview_delta() {
        // gui 开 + 已声明 /tmp/.X11-unix → 不应出现在注入增量
        let declared: ContainerConfig = serde_json::from_value(serde_json::json!({
            "name": "t", "image": "alpine", "gui": true,
            "mounts": [{"host_path":"/tmp/.X11-unix","container_path":"/tmp/.X11-unix","read_only":false}],
            "network": {"mode":"host","ports":[]},
            "silent_boot": false, "persistent": true
        }))
        .unwrap();
        let p = passthrough_preview(declared).unwrap();
        assert!(
            !p.mounts.iter().any(|m| m.container_path == "/tmp/.X11-unix"),
            "已声明的 /tmp/.X11-unix 不应重复出现在注入增量：{:?}",
            p.mounts
        );

        // gui 开 + 未声明 → 引擎恒注入 /tmp/.X11-unix，预览应含
        let bare: ContainerConfig = serde_json::from_value(serde_json::json!({
            "name": "t2", "image": "alpine", "gui": true,
            "mounts": [], "network": {"mode":"host","ports":[]},
            "silent_boot": false, "persistent": true
        }))
        .unwrap();
        let p2 = passthrough_preview(bare.clone()).unwrap();
        assert!(
            p2.mounts.iter().any(|m| m.container_path == "/tmp/.X11-unix"),
            "未声明时应注入 /tmp/.X11-unix：{:?}",
            p2.mounts
        );

        // gpu_nvidia=true → NVIDIA_* env 增量（确定性，不依赖宿主）
        let gpu_cfg: ContainerConfig = serde_json::from_value(serde_json::json!({
            "name": "t3", "image": "alpine", "gpu_nvidia": true,
            "mounts": [], "network": {"mode":"host","ports":[]},
            "silent_boot": false, "persistent": true
        }))
        .unwrap();
        let p3 = passthrough_preview(gpu_cfg).unwrap();
        assert!(p3.env.iter().any(|e| e == "NVIDIA_VISIBLE_DEVICES=all"));
        assert!(p3.env.iter().any(|e| e == "NVIDIA_DRIVER_CAPABILITIES=all"));

        // gui + gpu_nvidia 同开 → 两类注入都在（mounts 来自 gui，NVIDIA env 来自 gpu_nvidia）
        let both: ContainerConfig = serde_json::from_value(serde_json::json!({
            "name": "t4", "image": "alpine", "gui": true, "gpu_nvidia": true,
            "mounts": [], "network": {"mode":"host","ports":[]},
            "silent_boot": false, "persistent": true
        }))
        .unwrap();
        let p4 = passthrough_preview(both).unwrap();
        assert!(p4.mounts.iter().any(|m| m.container_path == "/tmp/.X11-unix"));
        assert!(p4.env.iter().any(|e| e == "NVIDIA_VISIBLE_DEVICES=all"));
    }

    #[test]
    fn test_conf_dir_dual_track_dev() {
        // 测试运行期 CARGO_MANIFEST_DIR 在 env → is_dev() true → dev 根：
        // conf 目录 = 本 crate manifest/conf = crates/gui/conf（工作区源码目录）。
        // 编译期（实测 tauri dev）也成立；安装二进制无该 env → prod ~/.easytidy/conf。
        let dir = conf_dir().unwrap();
        assert!(
            dir.ends_with("crates/gui/conf"),
            "dev 下 conf 目录应指向工作区 crates/gui/conf，得到 {}",
            dir.display()
        );
    }

    #[test]
    fn test_conf_seeds_parse() {
        // 全部内置种子都解析成功 + 结构性不变量（数量随 CONF_SEEDS 变化）
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

    /// 模板名 stem 规范化（增删改用 stem 定位文件，杜绝 `chrome.yaml.yaml`）。
    #[test]
    fn test_conf_template_stem_normalizes_extension() {
        assert_eq!(ConfTemplateStore::stem("chrome"), "chrome");
        assert_eq!(ConfTemplateStore::stem("chrome.yaml"), "chrome");
        assert_eq!(ConfTemplateStore::stem("chrome.yml"), "chrome");
        assert_eq!(ConfTemplateStore::stem(" chrome-copy.yaml "), "chrome-copy");
        assert_eq!(ConfTemplateStore::stem("chrome-copy"), "chrome-copy");
        assert_eq!(ConfTemplateStore::stem("a.b.yaml"), "a.b");
    }

    /// 回归（2026-09-02）：删「带 copy 的模板」不得误删源模板。
    ///
    /// 旧实现按 YAML 内 `config.name` 定位文件——复制不改名时 `chrome.yaml` 与
    /// `chrome-copy.yaml` 内部都是 `name: chrome`，删 copy 会删到源文件。
    /// 修复：身份 = 文件 stem，且复制改写内部 `config.name` = 新 stem。
    #[test]
    fn test_delete_by_stem_does_not_hit_source() {
        let temp = TempDir::new().unwrap();
        // 模拟历史缺陷产物：两个文件内部 name 都是 chrome（复制逐字节拷贝）
        let chrome_yaml =
            "name: chrome\nimage: alpine\nsilent_boot: true\npersistent: true\nmounts: []\nnetwork: { mode: host, ports: [] }\n";
        std::fs::write(temp.path().join("chrome.yaml"), chrome_yaml).unwrap();
        std::fs::write(
            temp.path().join("chrome-copy.yaml"),
            "name: chrome\nimage: alpine\nsilent_boot: true\npersistent: true\nmounts: []\nnetwork: { mode: host, ports: [] }\n",
        )
        .unwrap();

        // 删「带 copy 的」→ 只删 chrome-copy.yaml，源文件保留
        ConfTemplateStore::delete_in(temp.path(), "chrome-copy").unwrap();
        assert!(!temp.path().join("chrome-copy.yaml").exists(), "copy 应被删除");
        assert!(
            temp.path().join("chrome.yaml").exists(),
            "源模板 chrome.yaml 不应被误删"
        );

        // 扩展名调用同样定位正确
        ConfTemplateStore::delete_in(temp.path(), "chrome.yaml").unwrap();
        assert!(!temp.path().join("chrome.yaml").exists());
    }

    /// 复制后内部 `config.name` 必须改写为新 stem——否则复制品「使用」预填的
    /// 容器名与源冲突，且再次删除会撞名。
    #[test]
    fn test_duplicate_rewrites_internal_name_to_stem() {
        let temp = TempDir::new().unwrap();
        std::fs::write(
            temp.path().join("chrome.yaml"),
            "name: chrome\nimage: alpine\nsilent_boot: true\npersistent: true\nmounts: []\nnetwork: { mode: host, ports: [] }\n",
        )
        .unwrap();

        ConfTemplateStore::duplicate_in(temp.path(), "chrome", "chrome-copy").unwrap();
        let copy_yaml = std::fs::read_to_string(temp.path().join("chrome-copy.yaml")).unwrap();
        assert!(
            copy_yaml.contains("name: chrome-copy"),
            "复制品内部 config.name 应为新 stem，实际：{copy_yaml}"
        );
        // 复制品可被独立删除（按 stem），不会误伤源
        ConfTemplateStore::delete_in(temp.path(), "chrome-copy").unwrap();
        assert!(temp.path().join("chrome.yaml").exists(), "源模板应保留");
    }
}
