//! 配置服务：config.get / config.set（数据源 = 持久层 /home/easytidy/config.json）。

use anyhow::{Context, Result};
use serde_json::json;
use easytidy_protocol::{CfgGetResp, CfgSet, CfgSetResp, Frame, Message, MsgKind};
use tracing::info;

use crate::storage;

/// 默认配置（无持久化文件时的兜底；get/set 共用同一形状）。
fn default_config() -> serde_json::Value {
    json!({
        "version": 1,
        "mounts": [],
        "network": {},
        "passthrough": []
    })
}

pub(crate) async fn handle_config_get(msg: Message) -> Result<Frame> {
    let config = match storage::read_config()? {
        Some(content) => serde_json::from_str(&content)
            .with_context(|| format!("解析配置失败：{}", storage::config_path().display()))?,
        None => default_config(),
    };

    Ok(Frame::Json(Message {
        id: msg.id,
        kind: MsgKind::Resp,
        op: "config.get".to_string(),
        payload: serde_json::to_value(CfgGetResp { config })?,
        err: None,
    }))
}

pub(crate) async fn handle_config_set(msg: Message) -> Result<Frame> {
    let req: CfgSet = serde_json::from_value(msg.payload.clone())
        .context("解析 CfgSet 失败")?;

    // 读-合并-写：键级覆盖（原版 TODO 未实现，只回写旧内容）
    let mut config = match storage::read_config()? {
        Some(content) => serde_json::from_str::<serde_json::Value>(&content)
            .with_context(|| format!("解析配置失败：{}", storage::config_path().display()))?,
        None => default_config(),
    };
    if let Some(obj) = config.as_object_mut() {
        obj.insert(req.key.clone(), req.value);
    }
    storage::write_config(&serde_json::to_string_pretty(&config)?)?;
    info!("配置已更新：{}[{}]", storage::config_path().display(), req.key);

    Ok(Frame::Json(Message {
        id: msg.id,
        kind: MsgKind::Resp,
        op: "config.set".to_string(),
        payload: serde_json::to_value(CfgSetResp { success: true })?,
        err: None,
    }))
}
