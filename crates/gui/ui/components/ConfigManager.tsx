// 容器配置管理器（编辑既有容器）—— 配置编辑器的「实例编辑」入口。
//
// 本入口复用 `ConfigEditorPane` Shell：仅承载 entry-specific 逻辑：
//   - zustand store 订阅（load / apply / syncFromTemplate）
//   - handleApply 提交校验（mounts / ports / env —— entry-specific 提交语义）
//   - header 操作（模板 sync / 刷新 / 保存并重启）
//   - 警示（模板漂移 / 未保存修改）
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
  ForkOutlined,
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
    effective, hostUser, edit, loading, applying, syncing, error, dirty,
    flavorStatus, load, update, apply, syncFromTemplate, clearError,
  } = useConfigStore();

  useEffect(() => {
    load(containerName);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [containerName]);

  // ---------- 从模板同步（血缘） ----------

  const handleSync = async () => {
    const note = await syncFromTemplate(containerName);
    if (note) message.success(note);
  };

  // ---------- 保存并重启 ----------

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
      message.success('配置已应用，容器已重启');
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
            <Button
              type="primary"
              icon={<SaveOutlined />}
              loading={applying}
              disabled={!edit || !dirty}
            >
              保存并重启
            </Button>
          </Popconfirm>
        </Space>
      }
      notices={
        <>
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
        </>
      }
    />
  );
}

export function ConfigManager(props: ConfigManagerProps) {
  // antd App 包裹由 WorkerView 提供(单一上下文,避免嵌套)
  return <ConfigManagerInner {...props} />;
}