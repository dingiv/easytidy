//! 配置管理器命令：读取（configfile + inspect）与应用（重建容器）。

use tracing::warn;

use easytidy_core::configfile::ConfigFile;
use easytidy_core::models::ContainerConfig;
use easytidy_core::podman::Podman;

// ============================================================================
// 配置管理器命令（M4 前置：mount 管理 + 网络映射管理；改配置 = 重建容器）
// ============================================================================

/// 获取容器配置（configfile 期望配置 + podman inspect 当前生效状态 + 宿主用户）。
///
/// 返回（前端契约）：
/// ```json
/// { "config": { "name","image","entry","silent_boot","persistent",
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
    let config_path = ConfigFile::default_path().map_err(|e| format!("解析配置路径失败：{}", e))?;
    let config_file = ConfigFile::with_path(config_path);
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
