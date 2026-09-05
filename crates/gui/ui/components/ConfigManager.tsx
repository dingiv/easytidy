// 容器配置管理器（编辑既有容器）—— 配置编辑器的「实例编辑」入口。
//
// 本入口复用 `ConfigEditorPane` Shell：仅承载 entry-specific 逻辑：
//   - zustand store 订阅（load / apply）
//   - handleApply 提交校验（mounts / ports / env —— entry-specific 提交语义）
//   - header 操作（刷新 / 快速重建）
//   - 警示（未保存修改）
//
// 布局/标题/Spin/Alert 位置由 Shell 统一。

import { useEffect } from 'react';
import {
  App as AntApp,
  Alert,
  Button,
  Popconfirm,
  Space,
} from 'antd';
import {
  ReloadOutlined,
  SaveOutlined,
} from '@ant-design/icons';
import { validateEnv } from './config/utils';
import { ConfigEditorPane } from './config/ConfigEditorPane';
import { useConfigStore } from '../stores/configStore';

interface ConfigManagerProps {
  containerName: string;
}

function ConfigManagerInner({ containerName }: ConfigManagerProps) {
  const { message } = AntApp.useApp();
  const {
    effective, hostUser, edit, loading, applying, error, dirty,
    load, update, apply, clearError,
  } = useConfigStore();

  useEffect(() => {
    load(containerName);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [containerName]);

  // ---------- 快速重建 ----------

  const handleApply = async () => {
    if (!edit) return;
    // 提交前校验（编辑操作已校验，此处防陈旧状态）
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
      message.success('配置已应用，容器已快速重建');
    }
  };

  // 加载失败且无缓存 edit：只显示错误，避免编辑器拿不到 value（ContainerConfigEditor 必填）
  if (!loading && !edit) {
    return (
      <div className="config-editor-pane">
        {error && (
          <Alert
            type="error"
            showIcon
            message="操作失败"
            description={error}
            closable={Boolean(clearError)}
            onClose={clearError}
          />
        )}
      </div>
    );
  }

  return (
    <ConfigEditorPane
      title="容器配置"
      mode="edit"
      value={edit!}
      onChange={(next) => update(() => next)}
      effective={effective}
      hostUser={hostUser}
      error={error}
      onClearError={clearError}
      loading={loading}
      headerActions={
        <Space>
          <Button
            icon={<ReloadOutlined />}
            onClick={() => load(containerName)}
            loading={loading}
            disabled={applying}
          >
            刷新
          </Button>
          <Popconfirm
            title="快速重建容器"
            description={`将提交并快速重建容器（普通 commit + 安全流程：保留旧容器、失败自动回滚）以应用新的挂载、网络、环境变量与用户配置，期间容器会短暂停止。${
              !(edit?.keep_id ?? true) ? '警告：用户一致性映射（keep-id）已关闭！' : ''
            }`}
            okText="快速重建"
            cancelText="取消"
            okButtonProps={{ danger: true }}
            onConfirm={handleApply}
            disabled={!edit || applying || !dirty}
          >
            <Button
              type="primary"
              icon={<SaveOutlined />}
              loading={applying}
              disabled={!edit || !dirty}
            >
              快速重建
            </Button>
          </Popconfirm>
        </Space>
      }
      notices={
        <>
          {!loading && edit && dirty && (
            <Alert
              type="warning"
              showIcon
              message="有未保存的修改"
              description="修改将在快速重建容器后生效。"
            />
          )}
        </>
      }
    />
  );
}

export function ConfigManager(props: ConfigManagerProps) {
  // antd App 包裹由 WorkerView 提供(单一上下文,避免嵌套)
  return <ConfigManagerInner {...props} />;
}
