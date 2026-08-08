// 容器配置管理器（主组件）：状态管理 + 保存并重启 + Tabs 组装。
// 各面板拆分为 components/config/ 下的独立组件（MountsPane/NetworkPane/
// EnvPane/UserPane/ContainerPane），纯函数在 config/utils.ts。

import { useEffect, useMemo, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
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
import type {
  ContainerConfig,
  ContainerConfigResult,
  ContainerConfigView,
  HostUser,
  MountConfig,
  NetworkMode,
  PortMapping,
} from '../types';
import {
  configsEqual,
  normalizeConfig,
  normalizeView,
  validateEnv,
  viewMatchesSaved,
} from './config/utils';
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

  // 已保存（配置文件侧）vs 实际生效（podman 侧）
  const [saved, setSaved] = useState<ContainerConfig | null>(null);
  const [effective, setEffective] = useState<ContainerConfigView | null>(null);
  // 宿主用户（uid 映射语义对照表数据源；null = 探测失败）
  const [hostUser, setHostUser] = useState<HostUser | null>(null);
  // 本地编辑态
  const [edit, setEdit] = useState<ContainerConfig | null>(null);

  const [loading, setLoading] = useState(true);
  const [applying, setApplying] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    loadConfig();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [containerName]);

  const loadConfig = async () => {
    setLoading(true);
    setError(null);
    try {
      const result = await invoke<ContainerConfigResult>('get_container_config', {
        name: containerName,
      });
      const savedCfg = normalizeConfig(result.config);
      setSaved(savedCfg);
      setEffective(result.effective ? normalizeView(result.effective) : null);
      setHostUser(result.host_user ?? null);
      setEdit(normalizeConfig(result.config));
    } catch (err: any) {
      setError(err?.message || 'Failed to load container config');
      console.error('get_container_config failed:', err);
    } finally {
      setLoading(false);
    }
  };

  const dirty = useMemo(
    () => !!edit && !!saved && !configsEqual(edit, saved),
    [edit, saved],
  );
  const pendingRestart = useMemo(
    () => !!saved && !!effective && !viewMatchesSaved(saved, effective),
    [saved, effective],
  );

  // ---------- 编辑回调（各面板经 props 驱动 edit 态） ----------

  const addMount = (m: MountConfig) => {
    setEdit((prev) => (prev ? { ...prev, mounts: [...prev.mounts, m] } : prev));
  };

  const removeMount = (idx: number) => {
    setEdit((prev) => (prev ? { ...prev, mounts: prev.mounts.filter((_, i) => i !== idx) } : prev));
  };

  const setNetworkMode = (mode: NetworkMode) => {
    setEdit((prev) => (prev ? { ...prev, network: { ...prev.network, mode } } : prev));
  };

  const addPort = (p: PortMapping) => {
    setEdit((prev) =>
      prev ? { ...prev, network: { ...prev.network, ports: [...prev.network.ports, p] } } : prev,
    );
  };

  const removePort = (idx: number) => {
    setEdit((prev) =>
      prev
        ? { ...prev, network: { ...prev.network, ports: prev.network.ports.filter((_, i) => i !== idx) } }
        : prev,
    );
  };

  const addEnv = (key: string, value: string) => {
    setEdit((prev) => (prev ? { ...prev, env: [...prev.env, `${key}=${value}`] } : prev));
  };

  const removeEnv = (idx: number) => {
    setEdit((prev) => (prev ? { ...prev, env: prev.env.filter((_, i) => i !== idx) } : prev));
  };

  const setUserHome = (v: boolean) => {
    setEdit((prev) => (prev ? { ...prev, user_home: v } : prev));
  };

  const setEntry = (v: string) => {
    setEdit((prev) => (prev ? { ...prev, entry: v } : prev));
  };

  const setSilentBoot = (v: boolean) => {
    setEdit((prev) => (prev ? { ...prev, silent_boot: v } : prev));
  };

  // ---------- 保存并重启 ----------

  const handleApply = async () => {
    if (!edit) return;
    for (const m of edit.mounts) {
      if (!m.host_path.trim()) {
        message.error('存在宿主路径为空的挂载项');
        return;
      }
      if (!m.container_path.trim()) {
        message.error('存在容器路径为空的挂载项');
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
    // env 全量二次校验（防陈旧状态；与 EnvPane 添加时同规则）
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
    const payload: ContainerConfig = {
      ...edit,
      entry: edit.entry && edit.entry.trim() ? edit.entry.trim() : null,
    };
    setApplying(true);
    setError(null);
    try {
      const newId = await invoke<string>('apply_container_config', {
        name: containerName,
        config: payload,
      });
      console.log(`container recreated, new id: ${newId}`);
      message.success('配置已应用，容器已重启');
      await loadConfig();
    } catch (err: any) {
      const msg = err?.message || '应用配置失败';
      message.error(msg);
      console.error('apply_container_config failed:', err);
    } finally {
      setApplying(false);
    }
  };

  return (
    <div className="config-manager">
      <div className="config-manager-header">
        <Typography.Title level={4} className="config-manager-title">
          容器配置
        </Typography.Title>
        <Space>
          <Button icon={<ReloadOutlined />} onClick={loadConfig} loading={loading} disabled={applying}>
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
            disabled={!edit || applying}
          >
            <Button type="primary" icon={<SaveOutlined />} loading={applying} disabled={!edit}>
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
          onClose={() => setError(null)}
        />
      )}

      {!loading && saved && edit && (
        <>
          {dirty && (
            <Alert
              type="warning"
              showIcon
              message="有未保存的修改"
              description="修改将在保存并重启容器后生效。"
            />
          )}
          {!dirty && pendingRestart && (
            <Alert
              type="warning"
              showIcon
              message="配置已修改，重启后生效"
              description="已保存的配置与容器当前实际配置不一致，请保存并重启以生效。"
            />
          )}
        </>
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
                <MountsPane
                  mounts={edit.mounts}
                  onAdd={addMount}
                  onRemove={removeMount}
                />
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
                  onSilentBootChange={setSilentBoot}
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
