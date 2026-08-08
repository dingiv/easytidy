// 容器配置管理器（zustand 驱动）。
//
// 状态在 stores/configStore：saved/effective/edit/hostUser + **dirty 标志位**
// （编辑动作显式置位,不再深比较推断——比较法对引擎注入 env/mounts/
// userns 回显的过滤有漏网,未修改也误报"配置已修改",2026-08-08 实测）。
// 保存即"保存并重启容器",成功后重载,不存在"已保存未生效"状态。

import { useEffect } from 'react';
import {
  App as AntApp,
  Alert,
  Button,
  Popconfirm,
  Space,
  Spin,
  Tabs,
  Typography,
} from 'antd';
import {
  ApiOutlined,
  CodeOutlined,
  FolderOpenOutlined,
  ReloadOutlined,
  SaveOutlined,
  SettingOutlined,
  UserOutlined,
} from '@ant-design/icons';
import type { ContainerConfig } from '../types';
import { validateEnv } from './config/utils';
import { useConfigStore } from '../stores/configStore';
import { MountsPane } from './config/MountsPane';
import { NetworkPane } from './config/NetworkPane';
import { EnvPane } from './config/EnvPane';
import { UserPane } from './config/UserPane';
import { ContainerPane } from './config/ContainerPane';
import './ConfigManager.css';

interface ConfigManagerProps {
  containerName: string;
}

function ConfigManagerInner({ containerName }: ConfigManagerProps) {
  const { message } = AntApp.useApp();
  const { effective, hostUser, edit, loading, applying, error, dirty, load, update, apply, clearError } =
    useConfigStore();

  useEffect(() => {
    load(containerName);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [containerName]);

  // ---------- 编辑回调（统一走 store.update → dirty 置位） ----------

  const addMount = (m: ContainerConfig['mounts'][number]) =>
    update((prev) => ({ ...prev, mounts: [...prev.mounts, m] }));
  const removeMount = (idx: number) =>
    update((prev) => ({ ...prev, mounts: prev.mounts.filter((_, i) => i !== idx) }));
  const setNetworkMode = (mode: ContainerConfig['network']['mode']) =>
    update((prev) => ({ ...prev, network: { ...prev.network, mode } }));
  const addPort = (p: ContainerConfig['network']['ports'][number]) =>
    update((prev) => ({ ...prev, network: { ...prev.network, ports: [...prev.network.ports, p] } }));
  const removePort = (idx: number) =>
    update((prev) => ({
      ...prev,
      network: { ...prev.network, ports: prev.network.ports.filter((_, i) => i !== idx) },
    }));
  const addEnv = (key: string, value: string) =>
    update((prev) => ({ ...prev, env: [...prev.env, `${key}=${value}`] }));
  const removeEnv = (idx: number) =>
    update((prev) => ({ ...prev, env: prev.env.filter((_, i) => i !== idx) }));
  const setUserHome = (v: boolean) => update((prev) => ({ ...prev, user_home: v }));
  const setEntry = (v: string) => update((prev) => ({ ...prev, entry: v }));
  const setSilentBoot = (v: boolean) => update((prev) => ({ ...prev, silent_boot: v }));

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
        <Tabs
          className="config-manager-tabs"
          items={[
            {
              key: 'mounts',
              label: (
                <span>
                  <FolderOpenOutlined /> 挂载
                </span>
              ),
              children: (
                <MountsPane mounts={edit.mounts} onAdd={addMount} onRemove={removeMount} />
              ),
            },
            {
              key: 'network',
              label: (
                <span>
                  <ApiOutlined /> 网络
                </span>
              ),
              children: (
                <NetworkPane
                  network={edit.network}
                  onModeChange={setNetworkMode}
                  onAddPort={addPort}
                  onRemovePort={removePort}
                />
              ),
            },
            {
              key: 'container',
              label: (
                <span>
                  <SettingOutlined /> 容器
                </span>
              ),
              children: (
                <ContainerPane edit={edit} onEntryChange={setEntry} onSilentBootChange={setSilentBoot} />
              ),
            },
            {
              key: 'env',
              label: (
                <span>
                  <CodeOutlined /> 环境变量
                </span>
              ),
              children: (
                <EnvPane
                  env={edit.env}
                  effectiveEnv={effective?.env ?? null}
                  onAdd={addEnv}
                  onRemove={removeEnv}
                />
              ),
            },
            {
              key: 'user',
              label: (
                <span>
                  <UserOutlined /> 用户
                </span>
              ),
              children: (
                <UserPane
                  userHome={edit.user_home}
                  onUserHomeChange={setUserHome}
                  hostUser={hostUser}
                  effective={effective}
                />
              ),
            },
          ]}
        />
      )}
    </div>
  );
}

export function ConfigManager({ containerName }: ConfigManagerProps) {
  // antd App 包裹：让 message 等静态方法继承暗色主题与中文 locale
  return (
    <AntApp>
      <ConfigManagerInner containerName={containerName} />
    </AntApp>
  );
}
