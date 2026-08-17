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
  ForkOutlined,
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
  const {
    effective, hostUser, edit, loading, applying, syncing, error, dirty,
    flavorStatus, load, update, apply, syncFromFlavor, clearError,
  } = useConfigStore();

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
  const setEntryArgs = (v: string[]) => update((prev) => ({ ...prev, entry_args: v }));
  const setSilentBoot = (v: boolean) => update((prev) => ({ ...prev, silent_boot: v }));
  const setPersistent = (v: boolean) => update((prev) => ({ ...prev, persistent: v }));

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
                <ContainerPane
                  edit={edit}
                  onEntryChange={setEntry}
                  onEntryArgsChange={setEntryArgs}
                  onSilentBootChange={setSilentBoot}
                  onPersistentChange={setPersistent}
                />
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
