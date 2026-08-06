import { useEffect, useMemo, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import {
  App as AntApp,
  Alert,
  Button,
  Empty,
  Input,
  InputNumber,
  Popconfirm,
  Radio,
  Select,
  Space,
  Spin,
  Switch,
  Table,
  Tabs,
  Typography,
} from 'antd';
import type { TableProps } from 'antd';
import {
  ApiOutlined,
  DeleteOutlined,
  FolderOpenOutlined,
  PlusOutlined,
  ReloadOutlined,
  SaveOutlined,
  SettingOutlined,
} from '@ant-design/icons';
import type {
  ContainerConfig,
  ContainerConfigResult,
  MountConfig,
  NetworkMode,
  PortMapping,
} from '../types';
import './ConfigManager.css';

interface ConfigManagerProps {
  containerName: string;
}

/** 归一化：entry 转空字符串、深拷贝可变字段，便于受控编辑与比较 */
function normalizeConfig(cfg: ContainerConfig): ContainerConfig {
  return {
    ...cfg,
    entry: cfg.entry ?? '',
    mounts: cfg.mounts.map((m) => ({ ...m })),
    network: {
      mode: cfg.network.mode,
      ports: cfg.network.ports.map((p) => ({ ...p })),
    },
  };
}

function mountsEqual(a: MountConfig[], b: MountConfig[]): boolean {
  if (a.length !== b.length) return false;
  return a.every((m, i) => {
    const n = b[i];
    return (
      m.host_path === n.host_path &&
      m.container_path === n.container_path &&
      m.read_only === n.read_only
    );
  });
}

function networksEqual(a: ContainerConfig['network'], b: ContainerConfig['network']): boolean {
  if (a.mode !== b.mode) return false;
  if (a.ports.length !== b.ports.length) return false;
  return a.ports.every((p, i) => {
    const q = b.ports[i];
    return (
      p.host_port === q.host_port &&
      p.container_port === q.container_port &&
      p.protocol === q.protocol
    );
  });
}

function configsEqual(a: ContainerConfig, b: ContainerConfig): boolean {
  return (
    a.entry === b.entry &&
    a.silent_boot === b.silent_boot &&
    a.persistent === b.persistent &&
    mountsEqual(a.mounts, b.mounts) &&
    networksEqual(a.network, b.network)
  );
}

const EMPTY_PORT: PortMapping = { host_port: 0, container_port: 0, protocol: 'tcp' };
const PROTOCOL_OPTIONS = [
  { value: 'tcp', label: 'TCP' },
  { value: 'udp', label: 'UDP' },
];

function ConfigManagerInner({ containerName }: ConfigManagerProps) {
  const { message } = AntApp.useApp();

  // 已保存（配置文件侧）vs 实际生效（podman 侧）
  const [saved, setSaved] = useState<ContainerConfig | null>(null);
  const [effective, setEffective] = useState<ContainerConfig | null>(null);
  // 本地编辑态
  const [edit, setEdit] = useState<ContainerConfig | null>(null);

  const [loading, setLoading] = useState(true);
  const [applying, setApplying] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // 新增挂载行输入
  const [newMount, setNewMount] = useState({ host_path: '', container_path: '', read_only: false });
  // 新增端口行输入
  const [newPort, setNewPort] = useState<PortMapping>({ ...EMPTY_PORT });

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
      setEffective(result.effective ? normalizeConfig(result.effective) : null);
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
    () => !!saved && !!effective && !configsEqual(saved, effective),
    [saved, effective],
  );

  // ---------- 挂载编辑 ----------

  const addMount = () => {
    const host = newMount.host_path.trim();
    const target = newMount.container_path.trim();
    if (!host) {
      message.error('宿主路径不能为空');
      return;
    }
    if (!target) {
      message.error('容器路径不能为空');
      return;
    }
    setEdit((prev) =>
      prev
        ? {
            ...prev,
            mounts: [
              ...prev.mounts,
              { host_path: host, container_path: target, read_only: newMount.read_only },
            ],
          }
        : prev,
    );
    setNewMount({ host_path: '', container_path: '', read_only: false });
  };

  const removeMount = (idx: number) => {
    setEdit((prev) =>
      prev ? { ...prev, mounts: prev.mounts.filter((_, i) => i !== idx) } : prev,
    );
  };

  // ---------- 网络编辑 ----------

  const setNetworkMode = (mode: NetworkMode) => {
    setEdit((prev) => (prev ? { ...prev, network: { ...prev.network, mode } } : prev));
  };

  const addPort = () => {
    if (
      !Number.isInteger(newPort.host_port) ||
      newPort.host_port < 1 ||
      newPort.host_port > 65535
    ) {
      message.error('宿主端口需为 1-65535 的整数');
      return;
    }
    if (
      !Number.isInteger(newPort.container_port) ||
      newPort.container_port < 1 ||
      newPort.container_port > 65535
    ) {
      message.error('容器端口需为 1-65535 的整数');
      return;
    }
    setEdit((prev) =>
      prev
        ? {
            ...prev,
            network: { ...prev.network, ports: [...prev.network.ports, { ...newPort }] },
          }
        : prev,
    );
    setNewPort({ ...EMPTY_PORT });
  };

  const removePort = (idx: number) => {
    setEdit((prev) =>
      prev
        ? { ...prev, network: { ...prev.network, ports: prev.network.ports.filter((_, i) => i !== idx) } }
        : prev,
    );
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

  // ---------- 表格 ----------

  const mountColumns: TableProps<MountConfig>['columns'] = [
    {
      title: '宿主路径',
      dataIndex: 'host_path',
      key: 'host_path',
      ellipsis: true,
      render: (v: string) => <span className="path-cell">{v}</span>,
    },
    {
      title: '容器路径',
      dataIndex: 'container_path',
      key: 'container_path',
      ellipsis: true,
      render: (v: string) => <span className="path-cell">{v}</span>,
    },
    {
      title: '只读',
      dataIndex: 'read_only',
      key: 'read_only',
      width: 90,
      render: (ro: boolean) => <Switch size="small" checked={ro} disabled />,
    },
    {
      title: '操作',
      key: 'actions',
      width: 70,
      render: (_: unknown, _rec: MountConfig, idx: number) => (
        <Button
          type="text"
          danger
          size="small"
          icon={<DeleteOutlined />}
          onClick={() => removeMount(idx)}
        />
      ),
    },
  ];

  const portColumns: TableProps<PortMapping>['columns'] = [
    {
      title: '宿主端口',
      dataIndex: 'host_port',
      key: 'host_port',
      width: 120,
      render: (v: number) => <span className="port-cell">{v}</span>,
    },
    {
      title: '容器端口',
      dataIndex: 'container_port',
      key: 'container_port',
      width: 120,
      render: (v: number) => <span className="port-cell">{v}</span>,
    },
    {
      title: '协议',
      dataIndex: 'protocol',
      key: 'protocol',
      width: 90,
      render: (p: string) => <span className="proto-cell">{p.toUpperCase()}</span>,
    },
    {
      title: '操作',
      key: 'actions',
      width: 70,
      render: (_: unknown, _rec: PortMapping, idx: number) => (
        <Button
          type="text"
          danger
          size="small"
          icon={<DeleteOutlined />}
          onClick={() => removePort(idx)}
        />
      ),
    },
  ];

  // ---------- 各 tab 内容 ----------

  const mountsPane = (
    <div className="config-pane">
      <Table
        size="small"
        rowKey={(_rec: MountConfig, i) => `mount-${i}`}
        columns={mountColumns}
        dataSource={edit?.mounts ?? []}
        pagination={false}
        locale={{ emptyText: <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="暂无路径映射" /> }}
      />
      <div className="add-row">
        <Input
          placeholder="宿主路径，如 /home/user/data"
          value={newMount.host_path}
          onChange={(e) => setNewMount((m) => ({ ...m, host_path: e.target.value }))}
          onPressEnter={addMount}
        />
        <Input
          placeholder="容器路径，如 /data"
          value={newMount.container_path}
          onChange={(e) => setNewMount((m) => ({ ...m, container_path: e.target.value }))}
          onPressEnter={addMount}
        />
        <Switch
          checked={newMount.read_only}
          onChange={(v) => setNewMount((m) => ({ ...m, read_only: v }))}
          checkedChildren="只读"
          unCheckedChildren="读写"
        />
        <Button type="primary" icon={<PlusOutlined />} onClick={addMount}>
          添加
        </Button>
      </div>
    </div>
  );

  const networkPane = (
    <div className="config-pane">
      <Radio.Group
        value={edit?.network.mode}
        onChange={(e) => setNetworkMode(e.target.value as NetworkMode)}
      >
        <Radio.Button value="host">host 模式</Radio.Button>
        <Radio.Button value="mapped">映射模式</Radio.Button>
      </Radio.Group>

      {edit?.network.mode === 'host' ? (
        <Alert
          type="info"
          showIcon
          message="host 模式下端口映射不可用"
          description="切换为映射模式后可配置端口转发；映射模式需要容器使用 bridge 网络。"
          className="mode-hint"
        />
      ) : (
        <>
          <Table
            size="small"
            rowKey={(_rec: PortMapping, i) => `port-${i}`}
            columns={portColumns}
            dataSource={edit?.network.ports ?? []}
            pagination={false}
            locale={{ emptyText: <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="暂无端口映射" /> }}
          />
          <div className="add-row">
            <InputNumber
              min={1}
              max={65535}
              precision={0}
              placeholder="宿主端口"
              value={newPort.host_port || null}
              onChange={(v) => setNewPort((p) => ({ ...p, host_port: v ?? 0 }))}
              style={{ width: 140 }}
            />
            <InputNumber
              min={1}
              max={65535}
              precision={0}
              placeholder="容器端口"
              value={newPort.container_port || null}
              onChange={(v) => setNewPort((p) => ({ ...p, container_port: v ?? 0 }))}
              style={{ width: 140 }}
            />
            <Select
              value={newPort.protocol}
              onChange={(v) => setNewPort((p) => ({ ...p, protocol: v as PortMapping['protocol'] }))}
              options={PROTOCOL_OPTIONS}
              style={{ width: 100 }}
            />
            <Button type="primary" icon={<PlusOutlined />} onClick={addPort}>
              添加
            </Button>
          </div>
        </>
      )}
    </div>
  );

  const containerPane = (
    <div className="config-fields">
      <div className="config-field">
        <label>镜像</label>
        <Typography.Text code>{edit?.image}</Typography.Text>
      </div>
      <div className="config-field">
        <label>入口应用（entry）</label>
        <Input
          placeholder="容器启动时运行的命令，留空则不启动应用"
          value={edit?.entry ?? ''}
          onChange={(e) =>
            setEdit((prev) => (prev ? { ...prev, entry: e.target.value } : prev))
          }
        />
        <span className="section-hint">随「保存并重启」一起生效：容器重建后由 server 链式拉起。</span>
      </div>
      <div className="config-field">
        <label>静默启动</label>
        <Space>
          <Switch
            checked={edit?.silent_boot ?? false}
            onChange={(v) =>
              setEdit((prev) => (prev ? { ...prev, silent_boot: v } : prev))
            }
            checkedChildren="开"
            unCheckedChildren="关"
          />
          <Typography.Text type="secondary">
            宿主开机时无头启动容器并拉起 entry 应用（不弹 GUI）
          </Typography.Text>
        </Space>
      </div>
      <div className="config-field">
        <label>持久化</label>
        <Typography.Text>{edit?.persistent ? '是' : '否'}</Typography.Text>
      </div>
    </div>
  );

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
            description="将提交并重建容器以应用新的挂载与网络配置，期间容器会短暂停止。"
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
              children: mountsPane,
            },
            {
              key: 'network',
              label: (
                <span>
                  <ApiOutlined /> 网络
                </span>
              ),
              children: networkPane,
            },
            {
              key: 'container',
              label: (
                <span>
                  <SettingOutlined /> 容器
                </span>
              ),
              children: containerPane,
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
