// 容器配置管理器（zustand 驱动 + 统一编辑器）。
//
// 状态在 stores/configStore：saved/effective/edit/hostUser/flavorStatus +
// **dirty 标志位**（编辑动作显式置位,不再深比较推断——比较法对引擎注入
// env/mounts/userns 回显的过滤有漏网,未修改也误报"配置已修改",
// 2026-08-08 实测）。保存即"保存并重启容器",成功后重载,不存在
// "已保存未生效"状态。
//
// 表单体是统一编辑器 ContainerConfigEditor（与主 GUI 创建共用同一套
// ContainerConfig 编辑 UI）；本组件只承担：加载/校验/保存重启/模板同步。

import { useEffect } from 'react';
import {
  App as AntApp,
  Alert,
  Button,
  Popconfirm,
  Space,
  Spin,
  Typography,
} from 'antd';
import {
  ForkOutlined,
  ReloadOutlined,
  SaveOutlined,
} from '@ant-design/icons';
import { validateEnv } from './config/utils';
import { ContainerConfigEditor } from './config/ContainerConfigEditor';
import { useConfigStore } from '../stores/configStore';
import './ConfigManager.css';

interface ConfigManagerProps {
  containerName: string;
}

function ConfigManagerInner({ containerName }: ConfigManagerProps) {
  const { message } = AntApp.useApp();
  const {
    effective, hostUser, edit, loading, applying, syncing, error, dirty,
    flavorStatus, load, update, apply, syncFromFlavor, clearError,
  } = useConfigStore();

  useEffect(() => {
    load(containerName);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [containerName]);

  // ---------- 从模板同步（血缘） ----------

  const handleSync = async () => {
    const note = await syncFromFlavor(containerName);
    if (note) message.success(note);
  };

  // ---------- 保存并重启 ----------

  const handleApply = async () => {
    if (!edit) return;
    // 提交前校验（编辑操作已校验,此处防陈旧状态）
    for (const m of edit.mounts) {
      if (!m.host_path.trim() || !m.container_path.trim()) {
        message.error('存在路径为空的挂载项');
        return;
      }
    }
    for (const p of edit.network.ports) {
      if (!Number.isInteger(p.host_port) || p.host_port < 1 || p.host_port > 65535) {
        message.error(`端口映射 ${p.host_port}:${p.container_port} 的宿主端口无效`);
        return;
      }
      if (!Number.isInteger(p.container_port) || p.container_port < 1 || p.container_port > 65535) {
        message.error(`端口映射 ${p.host_port}:${p.container_port} 的容器端口无效`);
        return;
      }
    }
    const envKeys = new Set<string>();
    for (const kv of edit.env) {
      const i = kv.indexOf('=');
      const key = i > 0 ? kv.slice(0, i) : kv;
      const err = validateEnv(key, []);
      if (err) {
        message.error(err);
        return;
      }
      if (envKeys.has(key)) {
        message.error(`环境变量 ${key} 重复`);
        return;
      }
      envKeys.add(key);
    }
    await apply(containerName);
    if (!useConfigStore.getState().error) {
      message.success('配置已应用，容器已重启');
    }
  };

  return (
    <div className="config-manager">
      <div className="config-manager-header">
        <Typography.Title level={4} className="config-manager-title">
          容器配置
        </Typography.Title>
        <Space>
          {/* 血缘：来源模板 + 漂移状态 + 从模板同步（重建容器） */}
          {flavorStatus && (
            <Popconfirm
              title="从模板重新同步"
              description={`将按模板「${flavorStatus.flavor}」当前声明重新展开（镜像/挂载/网络/entry/用户映射/env 重新解析），并重建容器。本地的自启/常驻设置保留，其余本地修改将被模板覆盖。`}
              okText="重新同步并重建"
              cancelText="取消"
              okButtonProps={{ danger: true }}
              onConfirm={handleSync}
              disabled={!flavorStatus.exists || syncing || applying || dirty}
            >
              <Button
                icon={<ForkOutlined />}
                loading={syncing}
                disabled={!flavorStatus.exists || applying || dirty}
                title={
                  !flavorStatus.exists
                    ? '来源模板已删除，仅展示血缘'
                    : dirty
                      ? '有未保存的本地修改，请先保存或放弃'
                      : '按模板当前声明重新展开并重建'
                }
              >
                模板: {flavorStatus.flavor}
                {flavorStatus.exists && flavorStatus.drifted ? '（有变更）' : ''}
              </Button>
            </Popconfirm>
          )}
          <Button
            icon={<ReloadOutlined />}
            onClick={() => load(containerName)}
            loading={loading}
            disabled={applying}
          >
            刷新
          </Button>
          <Popconfirm
            title="保存并重启容器"
            description={`将提交并重建容器以应用新的挂载、网络、环境变量与用户配置，期间容器会短暂停止。${
              !(edit?.user_home ?? true) ? '警告：用户一致性映射已关闭！' : ''
            }`}
            okText="保存并重启"
            cancelText="取消"
            okButtonProps={{ danger: true }}
            onConfirm={handleApply}
            disabled={!edit || applying || !dirty}
          >
            <Button type="primary" icon={<SaveOutlined />} loading={applying} disabled={!edit || !dirty}>
              保存并重启
            </Button>
          </Popconfirm>
        </Space>
      </div>

      {error && (
        <Alert
          type="error"
          showIcon
          message="操作失败"
          description={error}
          closable
          onClose={clearError}
        />
      )}

      {!loading && flavorStatus?.exists && flavorStatus.drifted && !dirty && (
        <Alert
          type="info"
          showIcon
          message={`模板「${flavorStatus.flavor}」与当前配置存在差异`}
          description="来源模板已修改（或宿主显示环境变化导致展开结果不同）。可点击右上角「模板: …」按钮按模板重新同步（重建容器）。"
        />
      )}

      {!loading && edit && dirty && (
        <Alert
          type="warning"
          showIcon
          message="有未保存的修改"
          description="修改将在保存并重启容器后生效。"
        />
      )}

      {loading ? (
        <div className="config-manager-loading">
          <Spin tip="加载配置…" size="large">
            <div className="spin-block" />
          </Spin>
        </div>
      ) : !edit ? null : (
        // 统一编辑器（与主 GUI 创建共用）：受控 onChange → store.update
        // （dirty 置位）；edit 模式携带 inspect 投影 + 宿主用户对照
        <ContainerConfigEditor
          mode="edit"
          value={edit}
          onChange={(next) => update(() => next)}
          effective={effective}
          hostUser={hostUser}
        />
      )}
    </div>
  );
}

export function ConfigManager(props: ConfigManagerProps) {
  // antd App 包裹由 WorkerView 提供(单一上下文,避免嵌套)
  return <ConfigManagerInner {...props} />;
}
