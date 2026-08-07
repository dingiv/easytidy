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
  CodeOutlined,
  DeleteOutlined,
  FolderOpenOutlined,
  PlusOutlined,
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
    env: [...(cfg.env ?? [])],
  };
}

/** 归一化 effective（inspect 投影）——与 ContainerConfig 形状不同，独立处理 */
function normalizeView(v: ContainerConfigView): ContainerConfigView {
  return {
    ...v,
    mounts: v.mounts.map((m) => ({ ...m })),
    network: {
      mode: v.network.mode,
      ports: v.network.ports.map((p) => ({ ...p })),
    },
    env: [...(v.env ?? [])],
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

/** env 列表按 KEY→VALUE 映射比较（顺序无关——podman 回显顺序不保证） */
function envsEqual(a: string[], b: string[]): boolean {
  const map = (xs: string[]) => {
    const m = new Map<string, string>();
    for (const kv of xs) {
      const i = kv.indexOf('=');
      if (i > 0) m.set(kv.slice(0, i), kv.slice(i + 1));
    }
    return m;
  };
  const ma = map(a);
  const mb = map(b);
  if (ma.size !== mb.size) return false;
  for (const [k, v] of ma) {
    if (mb.get(k) !== v) return false;
  }
  return true;
}

/** podman 自动补的环境变量（非用户可配） */
const PODMAN_DEFAULT_ENV_KEYS = new Set(['PATH', 'HOSTNAME', 'TERM', 'HOME']);

/** 引擎保留注入的前缀（EASYTIDY_USER_*，create 时后写覆盖，用户不可编辑） */
const EASYTIDY_SYSTEM_ENV_PREFIX = 'EASYTIDY_USER_';

/** 是否为系统注入 env（podman 默认 / easytidy 引擎注入） */
function isSystemEnv(kv: string): boolean {
  const i = kv.indexOf('=');
  const key = i > 0 ? kv.slice(0, i) : kv;
  return PODMAN_DEFAULT_ENV_KEYS.has(key) || key.startsWith(EASYTIDY_SYSTEM_ENV_PREFIX);
}

/**
 * saved 与 effective 的 env 比较：两侧过滤系统注入后子集比较。
 * 若直接比较，EASYTIDY_USER_* 与 PATH/HOSTNAME 恒不等 → pendingRestart 恒真
 * （2026-08-07 XDG_DATA_DIRS 排障中实测）。
 */
function envRestartEqual(savedEnv: string[], effectiveEnv: string[]): boolean {
  return envsEqual(
    savedEnv.filter((kv) => !isSystemEnv(kv)),
    effectiveEnv.filter((kv) => !isSystemEnv(kv)),
  );
}

function configsEqual(a: ContainerConfig, b: ContainerConfig): boolean {
  return (
    a.entry === b.entry &&
    a.silent_boot === b.silent_boot &&
    a.persistent === b.persistent &&
    mountsEqual(a.mounts, b.mounts) &&
    networksEqual(a.network, b.network) &&
    envsEqual(a.env, b.env) &&
    a.user_home === b.user_home
  );
}

/**
 * saved（configfile 期望）与 effective（inspect 实际）是否一致。
 * view 形状与 ContainerConfig 不同（无 entry/silent_boot/persistent），
 * 不能复用 configsEqual；user_home 侧检验 userns_mode（keep-id 下 podman
 * 可能回显 "private"/None，此时只按 saved 判定）。
 */
function viewMatchesSaved(saved: ContainerConfig, view: ContainerConfigView): boolean {
  if (!mountsEqual(saved.mounts, view.mounts)) return false;
  if (!networksEqual(saved.network, view.network)) return false;
  if (!envRestartEqual(saved.env, view.env)) return false;
  if (saved.user_home && view.userns_mode !== null && view.userns_mode !== 'keep-id') {
    return false;
  }
  return true;
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
  const [effective, setEffective] = useState<ContainerConfigView | null>(null);
  // 宿主用户（uid 映射语义对照表数据源；null = 探测失败）
  const [hostUser, setHostUser] = useState<HostUser | null>(null);
  // 本地编辑态
  const [edit, setEdit] = useState<ContainerConfig | null>(null);

  const [loading, setLoading] = useState(true);
  const [applying, setApplying] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // 新增挂载行输入
  const [newMount, setNewMount] = useState({ host_path: '', container_path: '', read_only: false });
  // 新增端口行输入
  const [newPort, setNewPort] = useState<PortMapping>({ ...EMPTY_PORT });
  // 新增环境变量行输入
  const [newEnv, setNewEnv] = useState({ key: '', value: '' });

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

  // ---------- 环境变量编辑 ----------

  /** 校验单个 env 条目（返回错误信息，空串 = 通过） */
  const validateEnv = (key: string, existing: string[]): string => {
    if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(key)) {
      return '环境变量名需以字母或下划线开头，仅含字母/数字/下划线';
    }
    if (key.startsWith(EASYTIDY_SYSTEM_ENV_PREFIX)) {
      return `环境变量名不能使用 ${EASYTIDY_SYSTEM_ENV_PREFIX} 前缀（引擎保留注入，手动修改无效）`;
    }
    if (existing.some((kv) => kv.slice(0, kv.indexOf('=')) === key)) {
      return `环境变量 ${key} 已存在`;
    }
    return '';
  };

  const addEnv = () => {
    const key = newEnv.key.trim();
    const value = newEnv.value;
    const err = validateEnv(key, edit?.env ?? []);
    if (err) {
      message.error(err);
      return;
    }
    setEdit((prev) => (prev ? { ...prev, env: [...prev.env, `${key}=${value}`] } : prev));
    setNewEnv({ key: '', value: '' });
  };

  const removeEnv = (idx: number) => {
    setEdit((prev) => (prev ? { ...prev, env: prev.env.filter((_, i) => i !== idx) } : prev));
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
    // env 全量二次校验（防陈旧状态；与 addEnv 同规则）
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

  // ---------- 环境变量 ----------

  const envColumns: TableProps<{ key: string; value: string }>['columns'] = [
    {
      title: '变量名',
      dataIndex: 'key',
      key: 'key',
      ellipsis: true,
      render: (v: string) => <span className="env-key-cell">{v}</span>,
    },
    {
      title: '值',
      dataIndex: 'value',
      key: 'value',
      ellipsis: true,
      render: (v: string) => <Typography.Text>{v}</Typography.Text>,
    },
    {
      title: '操作',
      key: 'actions',
      width: 70,
      render: (_: unknown, _rec: { key: string; value: string }, idx: number) => (
        <Button
          type="text"
          danger
          size="small"
          icon={<DeleteOutlined />}
          onClick={() => removeEnv(idx)}
        />
      ),
    },
  ];

  /** 解析 "K=V"（value 可含 =） */
  const parseEnv = (kv: string) => {
    const i = kv.indexOf('=');
    return { key: i > 0 ? kv.slice(0, i) : kv, value: i > 0 ? kv.slice(i + 1) : '' };
  };

  const envPane = (
    <div className="config-pane">
      <Table
        size="small"
        rowKey={(_rec: { key: string; value: string }, i) => `env-${i}`}
        columns={envColumns}
        dataSource={(edit?.env ?? []).map(parseEnv)}
        pagination={false}
        locale={{ emptyText: <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="暂无自定义环境变量" /> }}
      />
      <div className="add-row">
        <Input
          placeholder="变量名，如 TEST_KEY"
          value={newEnv.key}
          onChange={(e) => setNewEnv((en) => ({ ...en, key: e.target.value }))}
          onPressEnter={addEnv}
          style={{ width: 220 }}
        />
        <Input
          placeholder="值（可留空）"
          value={newEnv.value}
          onChange={(e) => setNewEnv((en) => ({ ...en, value: e.target.value }))}
          onPressEnter={addEnv}
        />
        <Button type="primary" icon={<PlusOutlined />} onClick={addEnv}>
          添加
        </Button>
      </div>
      <span className="section-hint">
        环境变量随「保存并重启」在容器重建后生效；EASYTIDY_USER_* 由引擎保留注入，不可手动修改。
      </span>

      <div className="config-subsection">
        <Typography.Text strong>生效环境（含系统注入，只读）</Typography.Text>
        {effective ? (
          <Table
            size="small"
            rowKey={(_rec: { key: string; value: string; src: string }, i) => `eff-env-${i}`}
            dataSource={(effective.env ?? [])
              .map((kv) => {
                const { key, value } = parseEnv(kv);
                let src = '用户配置';
                if (key.startsWith(EASYTIDY_SYSTEM_ENV_PREFIX)) src = 'easytidy 系统注入';
                else if (PODMAN_DEFAULT_ENV_KEYS.has(key)) src = 'podman 默认';
                return { key, value, src };
              })
              .sort((a, b) => (a.src === b.src ? 0 : a.src === '用户配置' ? -1 : 1))}
            pagination={false}
            locale={{ emptyText: <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="无" /> }}
            columns={[
              {
                title: '变量名',
                dataIndex: 'key',
                key: 'key',
                ellipsis: true,
                render: (v: string) => <span className="env-key-cell">{v}</span>,
              },
              {
                title: '值',
                dataIndex: 'value',
                key: 'value',
                ellipsis: true,
              },
              {
                title: '来源',
                dataIndex: 'src',
                key: 'src',
                width: 150,
                render: (v: string) => (
                  <span className={v === '用户配置' ? 'env-src-user' : 'env-src-system'}>{v}</span>
                ),
              },
            ]}
          />
        ) : (
          <Typography.Text type="secondary">
            容器未创建，无实际生效环境（可编辑下方配置后保存并重启）。
          </Typography.Text>
        )}
      </div>
    </div>
  );

  // ---------- 用户与 uid 映射 ----------

  /** uid 映射语义对照表数据（keep-id 语义见 docs/12 实证：字面 uid_map 不代表实际属主） */
  const uidMapRows = () => {
    const hu = hostUser;
    if (!hu) {
      return [
        {
          key: 'host-missing',
          container: '容器内身份',
          host: '宿主身份',
          note: '宿主用户探测失败，容器以 root 运行（用户一致性映射未生效）',
        },
      ];
    }
    if (edit?.user_home) {
      // keep-id 激活（实测文件属主，2026-08-07）：容器 uid 0（root）的宿主身份是
      // **subuid 100000**（容器文件系统属主），不是宿主默认用户；
      // 容器 uid N（node）= 宿主登录用户 N
      return [
        {
          key: 'keep-id-root',
          container: `root（uid 0）· server/装包`,
          host: `宿主 subuid 100000（容器文件系统属主）`,
          note: '可写容器系统文件（apt/sudo）；写宿主 home 属主呈现 100000',
        },
        {
          key: 'keep-id-node',
          container: `node（uid ${hu.uid}）· 应用默认`,
          host: `${hu.name}（uid ${hu.uid}）`,
          note: '宿主 home 直读写（属主正确）、显示可用（GUI 应用沙盒完整）',
        },
      ];
    }
    // 默认 rootless：容器 uid 0 = 宿主用户；容器普通用户 = subuid 偏移
    return [
      {
        key: 'plain-root',
        container: 'root（uid 0）· server/装包',
        host: `${hu.name}（uid ${hu.uid}）`,
        note: '可写容器系统文件',
      },
      {
        key: 'plain-user',
        container: '容器内普通用户（uid N）',
        host: `宿主 subuid 100000+N（/etc/subuid）`,
        note: '无法访问宿主 home（0700）与显示 socket',
      },
    ];
  };

  const userPane = (
    <div className="config-pane">
      <div className="config-field">
        <label>用户一致性映射（keep-id）</label>
        <Space>
          <Switch
            checked={edit?.user_home ?? true}
            onChange={(v) => setEdit((prev) => (prev ? { ...prev, user_home: v } : prev))}
            checkedChildren="开"
            unCheckedChildren="关"
          />
          <Typography.Text type="secondary">
            容器内用户 uid 与宿主对齐（keep-id），应用默认以 node 用户运行
          </Typography.Text>
        </Space>
      </div>
      {!(edit?.user_home ?? true) && (
        <Alert
          type="warning"
          showIcon
          message="关闭用户一致性映射有风险"
          description="关闭后容器进程以 root 运行，宿主 $HOME 挂载与 keep-id 一并移除：GUI 应用将无法访问宿主显示 socket（图形界面不可用）。GUI 容器请保持开启；此开关随「保存并重启」重建容器后生效。"
          className="mode-hint"
        />
      )}

      <div className="config-subsection">
        <Typography.Text strong>uid 映射关系</Typography.Text>
        <Table
          size="small"
          rowKey={(r) => r.key}
          dataSource={uidMapRows()}
          pagination={false}
          columns={[
            {
              title: '容器内身份',
              dataIndex: 'container',
              key: 'container',
            },
            {
              title: '宿主身份',
              dataIndex: 'host',
              key: 'host',
            },
            {
              title: '能力',
              dataIndex: 'note',
              key: 'note',
            },
          ]}
        />
        <span className="section-hint">
          {effective?.userns_mode
            ? `podman 实际 userns 模式：${effective.userns_mode}（keep-id 语义见 docs/12；字面 uid_map 不代表实际属主）。`
            : '容器未创建，以上为按当前配置的预期映射（keep-id 语义见 docs/12）。'}
        </span>
      </div>

      <div className="config-subsection">
        <Typography.Text strong>root 与容器内默认用户（只读）</Typography.Text>
        <div className="config-field">
          <label>容器进程用户</label>
          <Typography.Text code>{effective?.user ?? '0:0（创建约定）'}</Typography.Text>
          <span className="section-hint">server 以容器内 root 运行才有装包权；应用层经 su 降权到 node。</span>
        </div>
        <div className="config-field">
          <label>容器内默认用户</label>
          <Typography.Text code>node</Typography.Text>
          <span className="section-hint">
            固定名（server CONTAINER_USER），uid/gid = 宿主{' '}
            {hostUser ? `${hostUser.uid}/${hostUser.gid}` : '（探测失败）'}；免密 sudo 已配置。
          </span>
        </div>
        <div className="config-field">
          <label>宿主用户</label>
          {hostUser ? (
            <Typography.Text code>
              {hostUser.name}（uid {hostUser.uid} / gid {hostUser.gid}）home={hostUser.home}
            </Typography.Text>
          ) : (
            <Typography.Text type="warning">探测失败——容器将降级 root 运行</Typography.Text>
          )}
        </div>
      </div>
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
            {
              key: 'env',
              label: (
                <span>
                  <CodeOutlined /> 环境变量
                </span>
              ),
              children: envPane,
            },
            {
              key: 'user',
              label: (
                <span>
                  <UserOutlined /> 用户
                </span>
              ),
              children: userPane,
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
