// 容器管理（主 GUI 合并面板）：快速启动 + 我的容器。
//
// 本模块的目的是让用户快速启动容器——flavor 模板（快速启动区，一键展开
// 创建）与环境列表（我的容器区，已创建容器的全生命周期操作）原是两个
// tab，概念重叠（模板 = 容器配置的批量管理层），合并为一个面板：
//
// - 快速启动：flavor 模板卡片（▶ 启动 → 页内创建表单预选模板；编辑/
//   删除/同步派生）。派生计数与批量同步 = 模板血缘链的 UI 面
// - 我的容器：环境卡片（运行/关闭/打开/快照 commit/fork/重建 rebuild/
//   删除）。rebuild 按注册表当前配置重建（应用外部改的 config.toml），
//   commit = 快照（fork 的源）
//
// 新建 = 页内 ContainerCreateForm（与配置管理同一套 ContainerConfig
// 编辑器），替换列表呈现。

import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { errMsg } from '../lib/errors';
import {
  App as AntApp,
  Alert,
  Button,
  Card,
  Empty,
  Input,
  Modal,
  Popconfirm,
  Select,
  Space,
  Spin,
  Switch,
  Tag,
  Tooltip,
  Typography,
} from 'antd';
import {
  CameraOutlined,
  DeleteOutlined,
  EditOutlined,
  ForkOutlined,
  PlayCircleOutlined,
  PlusOutlined,
  ReloadOutlined,
  RocketOutlined,
  StopOutlined,
  SyncOutlined,
  ExportOutlined,
  ToolOutlined,
} from '@ant-design/icons';
import type { EnvView, Flavor, MountConfig } from '../types';
import { ContainerCreateForm } from './ContainerCreateForm';
import './ContainersPanel.css';

/** 状态 → 中文标签 + 颜色（普通用户视角） */
function statusMeta(status: string): { label: string; color: string } {
  switch (status) {
    case 'running':
      return { label: '运行中', color: 'green' };
    case 'exited':
    case 'created':
    case 'paused':
      return { label: '已停止', color: 'default' };
    case 'missing':
      return { label: '未创建', color: 'red' };
    default:
      return { label: status, color: 'default' };
  }
}

/** 空白 flavor（新建表单初始值；user_home 与 Rust 共享基座对齐：bool 默认 true） */
function emptyFlavor(): Flavor {
  return {
    name: '',
    image: 'docker.io/library/ubuntu:24.04',
    gui: true,
    setup: [],
    entry: null,
    entry_args: [],
    mounts: [],
    user_home: true,
    network: { mode: 'host', ports: [] },
  };
}

/** mounts 行文本（"host:container[:ro]"）→ MountConfig */
function parseMountLine(line: string): MountConfig | null {
  const parts = line.split(':').map((p) => p.trim());
  if (parts.length < 2 || !parts[0] || !parts[1]) return null;
  // 末段 "ro" = 只读（其余段合并为容器路径）
  const ro = parts.length >= 3 && parts[parts.length - 1] === 'ro';
  const container = ro ? parts.slice(1, -1).join(':') : parts.slice(1).join(':');
  return { host_path: parts[0], container_path: container, read_only: ro };
}

/** MountConfig → 行文本 */
function mountToLine(m: MountConfig): string {
  return `${m.host_path}:${m.container_path}${m.read_only ? ':ro' : ''}`;
}

function ContainersPanelInner() {
  const { message, modal } = AntApp.useApp();

  // ---------- 我的容器（环境列表） ----------
  const [envs, setEnvs] = useState<EnvView[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [rebuilding, setRebuilding] = useState<string | null>(null);

  // ---------- 快速启动（flavor 模板 + 血缘） ----------
  const [flavors, setFlavors] = useState<Flavor[]>([]);
  const [lineage, setLineage] = useState<Record<string, string[]>>({});
  const [syncingFlavor, setSyncingFlavor] = useState<string | null>(null);

  // ---------- 新建（页内表单；creatingFlavor = 模板卡「启动」预选） ----------
  const [creating, setCreating] = useState(false);
  const [creatingFlavor, setCreatingFlavor] = useState<string | undefined>(undefined);

  // ---------- 快照（按环境记录可选标签） ----------
  const [snapshotTag, setSnapshotTag] = useState<Record<string, string>>({});

  // ---------- fork（从快照派生新环境） ----------
  const [forkTarget, setForkTarget] = useState<EnvView | null>(null);
  const [forkSnapshot, setForkSnapshot] = useState('');
  const [forkNewName, setForkNewName] = useState('');
  const [forking, setForking] = useState(false);

  // ---------- flavor 编辑器（模板表单） ----------
  const [editing, setEditing] = useState<Flavor | null>(null);
  const [isNew, setIsNew] = useState(false);
  const [setupText, setSetupText] = useState('');
  const [mountsText, setMountsText] = useState('');
  const [saving, setSaving] = useState(false);

  const load = async () => {
    setLoading(true);
    setError(null);
    try {
      const [envList, flavorList, lineageMap] = await Promise.all([
        invoke<EnvView[]>('env_list'),
        invoke<Flavor[]>('flavor_list_detailed'),
        invoke<Record<string, string[]>>('flavor_lineage'),
      ]);
      setEnvs(envList);
      setFlavors(flavorList);
      setLineage(lineageMap ?? {});
    } catch (err: any) {
      setError(errMsg(err, '加载失败'));
      console.error('load (envs/flavors) failed:', err);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    load();
  }, []);

  // ---------- 我的容器：操作 ----------

  const handleStart = async (env: EnvView) => {
    try {
      await invoke('env_start', { name: env.name });
      message.success(`容器「${env.name}」已运行`);
      await load();
    } catch (err: any) {
      message.error(errMsg(err, '启动失败'));
      console.error('env_start failed:', err);
    }
  };

  const handleStop = async (env: EnvView) => {
    try {
      await invoke('env_stop', { name: env.name });
      message.success(`容器「${env.name}」已关闭（环境保留，可随时恢复）`);
      await load();
    } catch (err: any) {
      message.error(errMsg(err, '关闭失败'));
      console.error('env_stop failed:', err);
    }
  };

  /** 打开容器窗口（单实例 GUI；等价容器卡片 Open） */
  const handleOpen = async (env: EnvView) => {
    try {
      await invoke('open_container_window', { name: env.name });
    } catch (err: any) {
      message.error(errMsg(err, '打开容器窗口失败'));
      console.error('open_container_window failed:', err);
    }
  };

  /** 重建：按注册表（config.toml）当前配置 commit → 删旧 → 同名重建 → 启动 */
  const handleRebuild = async (env: EnvView) => {
    setRebuilding(env.name);
    try {
      await invoke('env_rebuild', { name: env.name });
      message.success(`容器「${env.name}」已按注册配置重建并启动`);
      await load();
    } catch (err: any) {
      message.error(errMsg(err, '重建失败'));
      console.error('env_rebuild failed:', err);
    } finally {
      setRebuilding(null);
    }
  };

  /** 快照（commit 当前文件系统层；fork 的源，独立资产） */
  const handleSnapshot = async (env: EnvView) => {
    const tag = (snapshotTag[env.name] ?? '').trim();
    try {
      const imageRef = await invoke<string>('env_snapshot', {
        name: env.name,
        tag: tag || null,
      });
      message.success(`快照完成：${imageRef}`);
      setSnapshotTag((prev) => ({ ...prev, [env.name]: '' }));
    } catch (err: any) {
      message.error(errMsg(err, '创建快照失败'));
      console.error('env_snapshot failed:', err);
    }
  };

  const openForkModal = (env: EnvView) => {
    setForkTarget(env);
    setForkSnapshot('');
    setForkNewName('');
  };

  const handleFork = async () => {
    if (!forkTarget) return;
    const snapshot = forkSnapshot.trim();
    const newName = forkNewName.trim();
    if (!snapshot) {
      message.error('请输入快照标签（请先对源容器执行「快照」）');
      return;
    }
    if (!newName) {
      message.error('请输入新容器名称');
      return;
    }
    setForking(true);
    try {
      await invoke('env_fork', { name: forkTarget.name, snapshot, newName });
      message.success(`已从快照「${snapshot}」派生新容器「${newName}」并运行`);
      setForkTarget(null);
      await load();
    } catch (err: any) {
      message.error(errMsg(err, '派生（fork）失败'));
      console.error('env_fork failed:', err);
    } finally {
      setForking(false);
    }
  };

  const handleDelete = async (env: EnvView) => {
    try {
      await invoke('env_rm', { name: env.name });
      message.success(`容器「${env.name}」已删除；其快照为独立资产已保留，可用于派生（fork）`);
      await load();
    } catch (err: any) {
      message.error(errMsg(err, '删除失败'));
      console.error('env_rm failed:', err);
    }
  };

  // ---------- 快速启动：flavor 操作 ----------

  /** 模板卡「启动」：页内创建表单 + 预选模板（输入名称后自动展开预填） */
  const launchFlavor = (f: Flavor) => {
    setCreatingFlavor(f.name);
    setCreating(true);
  };

  /** 模板编辑器打开（flavor=null → 新建） */
  const openEditor = (flavor: Flavor | null) => {
    const f = flavor ?? emptyFlavor();
    setEditing(f);
    setIsNew(flavor === null);
    setSetupText(f.setup.join('\n'));
    setMountsText(f.mounts.map(mountToLine).join('\n'));
  };

  const handleFlavorSave = async () => {
    const f = editing;
    if (!f) return;
    if (!f.name.trim() || !f.image.trim()) {
      message.warning('名称与镜像不能为空');
      return;
    }
    setSaving(true);
    try {
      const toSave: Flavor = {
        ...f,
        name: f.name.trim(),
        image: f.image.trim(),
        setup: setupText.split('\n').map((s) => s.trim()).filter(Boolean),
        mounts: mountsText
          .split('\n')
          .map((l) => l.trim())
          .filter(Boolean)
          .map(parseMountLine)
          .filter((m): m is MountConfig => m !== null),
      };
      await invoke('flavor_save', { flavor: toSave });
      message.success(`模板已保存：${toSave.name}`);
      setEditing(null);
      await load();
    } catch (err: any) {
      message.error(errMsg(err, '保存失败'));
      console.error('flavor_save failed:', err);
    } finally {
      setSaving(false);
    }
  };

  const handleFlavorDelete = (f: Flavor) => {
    modal.confirm({
      title: `删除模板 ${f.name}?`,
      content: '已创建的容器不受影响，仅删除模板。',
      okText: '删除',
      okButtonProps: { danger: true },
      cancelText: '取消',
      onOk: async () => {
        try {
          await invoke('flavor_delete', { name: f.name });
          message.success(`已删除：${f.name}`);
          await load();
        } catch (err: any) {
          message.error(errMsg(err, '删除失败'));
        }
      },
    });
  };

  /** 批量同步：把模板当前声明重新展开到全部派生容器（逐个重建） */
  const handleSyncAll = (f: Flavor) => {
    const derived = lineage[f.name] ?? [];
    if (derived.length === 0) return;
    modal.confirm({
      title: `按模板「${f.name}」重新同步全部派生容器？`,
      content: (
        <div>
          <p>以下 {derived.length} 个容器将按模板当前声明重新展开并逐个重建（本地的自启/常驻设置保留，其余本地修改被模板覆盖）：</p>
          <p style={{ paddingLeft: 12 }}>{derived.join('、')}</p>
        </div>
      ),
      okText: '全部重新同步',
      okButtonProps: { danger: true },
      cancelText: '取消',
      onOk: async () => {
        setSyncingFlavor(f.name);
        const failures: string[] = [];
        try {
          for (const name of derived) {
            try {
              await invoke('config_sync_from_flavor', { name });
            } catch (err: any) {
              failures.push(`${name}：${errMsg(err)}`);
            }
          }
          if (failures.length > 0) {
            modal.error({
              title: `同步完成，${failures.length} 个失败`,
              width: 620,
              content: <pre className="error-detail">{failures.join('\n\n')}</pre>,
              okText: '知道了',
            });
          } else {
            message.success(`已按模板「${f.name}」同步 ${derived.length} 个容器`);
          }
          await load();
        } finally {
          setSyncingFlavor(null);
        }
      },
    });
  };

  // ---------- 渲染 ----------

  // 新建：页内表单替换列表（预选模板时输入名称即自动展开）
  if (creating) {
    return (
      <div className="env-panel">
        <ContainerCreateForm
          initialFlavor={creatingFlavor}
          onCancel={() => {
            setCreating(false);
            setCreatingFlavor(undefined);
          }}
          onCreated={() => {
            setCreating(false);
            setCreatingFlavor(undefined);
            load();
          }}
        />
      </div>
    );
  }

  return (
    <div className="env-panel">
      <div className="env-panel-header">
        <Typography.Title level={4} className="env-panel-title">
          容器
        </Typography.Title>
        <Space>
          <Button icon={<ReloadOutlined />} onClick={load} loading={loading}>
            刷新
          </Button>
          <Button
            type="primary"
            icon={<PlusOutlined />}
            onClick={() => {
              setCreatingFlavor(undefined);
              setCreating(true);
            }}
          >
            新建容器
          </Button>
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

      {loading ? (
        <div className="env-panel-loading">
          <Spin tip="加载…" size="large">
            <div className="spin-block" />
          </Spin>
        </div>
      ) : (
        <>
          {/* ---------- 快速启动（flavor 模板） ---------- */}
          <section className="env-section">
            <Typography.Title level={5} className="env-section-title">
              快速启动
            </Typography.Title>
            <p className="panel-hint">模板一键展开创建预配置容器（GUI 透传 / 安装命令 / entry 应用）。</p>
            <div className="flavor-list">
              {flavors.map((f) => {
                const derived = lineage[f.name] ?? [];
                return (
                  <div key={f.name} className="flavor-item">
                    <div className="flavor-main">
                      <div className="flavor-title">
                        <span className="flavor-name">{f.name}</span>
                        {f.gui && <Tag color="blue">GUI</Tag>}
                        {f.entry && <Tag color="green">entry: {f.entry}</Tag>}
                        {f.setup.length > 0 && <Tag>setup ×{f.setup.length}</Tag>}
                        {f.mounts.length > 0 && <Tag>mounts ×{f.mounts.length}</Tag>}
                        <Tag>{f.network.mode === 'host' ? 'host 网络' : 'bridge'}</Tag>
                        {derived.length > 0 && (
                          <Tooltip title={`派生容器：${derived.join('、')}`}>
                            <Tag color="purple">派生 ×{derived.length}</Tag>
                          </Tooltip>
                        )}
                      </div>
                      <div className="flavor-image">{f.image}</div>
                    </div>
                    <div className="flavor-actions">
                      <Tooltip title="按模板创建容器（输入名称后自动展开预填）">
                        <Button
                          type="primary"
                          icon={<RocketOutlined />}
                          onClick={() => launchFlavor(f)}
                        >
                          启动
                        </Button>
                      </Tooltip>
                      {derived.length > 0 && (
                        <Tooltip title={`按模板当前声明重新同步全部派生容器（${derived.join('、')}）`}>
                          <Button
                            icon={<SyncOutlined />}
                            loading={syncingFlavor === f.name}
                            onClick={() => handleSyncAll(f)}
                          >
                            同步派生
                          </Button>
                        </Tooltip>
                      )}
                      <Button icon={<EditOutlined />} onClick={() => openEditor(f)} title="编辑" />
                      <Button danger icon={<DeleteOutlined />} onClick={() => handleFlavorDelete(f)} title="删除" />
                    </div>
                  </div>
                );
              })}
              {flavors.length === 0 && (
                <div className="empty-message">暂无模板，点击右侧「编辑模板」创建启动配置。</div>
              )}
            </div>
            <Space style={{ marginTop: 8 }}>
              <Button icon={<EditOutlined />} onClick={() => openEditor(null)}>
                编辑模板…
              </Button>
            </Space>
          </section>

          {/* ---------- 我的容器（环境列表） ---------- */}
          <section className="env-section">
            <Typography.Title level={5} className="env-section-title">
              我的容器
            </Typography.Title>
            {envs.length === 0 ? (
              <Empty
                image={Empty.PRESENTED_IMAGE_SIMPLE}
                description="暂无容器，从上方模板快速启动，或点击「新建容器」"
              />
            ) : (
              <div className="env-list">
                {envs.map((env) => {
                  const st = statusMeta(env.status);
                  const running = env.status === 'running';
                  const missing = env.status === 'missing';
                  return (
                    <Card key={env.name} size="small" className="env-card">
                      <div className="env-card-body">
                        <div className="env-info">
                          <div className="env-name-row">
                            <Typography.Text strong className="env-name">
                              {env.name}
                            </Typography.Text>
                            <Tag color={st.color}>{st.label}</Tag>
                          </div>
                          <Typography.Text code className="env-image">
                            {env.image}
                          </Typography.Text>
                        </div>
                        <Space wrap className="env-actions">
                          {running ? (
                            <Button size="small" icon={<StopOutlined />} onClick={() => handleStop(env)}>
                              关闭
                            </Button>
                          ) : !missing ? (
                            <Button
                              size="small"
                              type="primary"
                              ghost
                              icon={<PlayCircleOutlined />}
                              onClick={() => handleStart(env)}
                            >
                              运行
                            </Button>
                          ) : null}
                          <Button
                            size="small"
                            icon={<ExportOutlined />}
                            onClick={() => handleOpen(env)}
                            title="打开容器窗口（单实例 GUI）"
                          >
                            打开
                          </Button>
                          <Popconfirm
                            title="重建容器"
                            description="按注册表（config.toml）当前配置 commit → 删除 → 同名重建 → 启动。用于应用外部修改的配置文件。"
                            okText="重建"
                            cancelText="取消"
                            okButtonProps={{ danger: true }}
                            disabled={missing}
                            onConfirm={() => handleRebuild(env)}
                          >
                            <Button
                              size="small"
                              icon={<ToolOutlined />}
                              loading={rebuilding === env.name}
                              disabled={missing}
                              title="按注册配置重建"
                            >
                              重建
                            </Button>
                          </Popconfirm>
                          <Popconfirm
                            title="创建快照"
                            description={
                              <Input
                                placeholder="快照标签（可选，默认时间戳）"
                                value={snapshotTag[env.name] ?? ''}
                                onChange={(e) =>
                                  setSnapshotTag((prev) => ({ ...prev, [env.name]: e.target.value }))
                                }
                                onKeyDown={(e) => {
                                  if (e.key === 'Enter') handleSnapshot(env);
                                }}
                              />
                            }
                            okText="创建"
                            cancelText="取消"
                            disabled={missing}
                            onConfirm={() => handleSnapshot(env)}
                          >
                            <Button size="small" icon={<CameraOutlined />} disabled={missing}>
                              快照
                            </Button>
                          </Popconfirm>
                          <Button size="small" icon={<ForkOutlined />} onClick={() => openForkModal(env)}>
                            fork
                          </Button>
                          <Popconfirm
                            title={`删除容器「${env.name}」？`}
                            description="将清理该容器的容器、注册配置、桌面图标与 socket 目录；其快照为独立资产将保留（可被 fork 复用）。"
                            okText="删除"
                            cancelText="取消"
                            okButtonProps={{ danger: true }}
                            onConfirm={() => handleDelete(env)}
                          >
                            <Button size="small" danger icon={<DeleteOutlined />}>
                              删除
                            </Button>
                          </Popconfirm>
                        </Space>
                      </div>
                    </Card>
                  );
                })}
              </div>
            )}
          </section>
        </>
      )}

      {/* fork：从快照派生新容器 */}
      <Modal
        title={forkTarget ? `从快照派生新容器（源：${forkTarget.name}）` : '从快照派生新容器'}
        open={!!forkTarget}
        onCancel={() => setForkTarget(null)}
        onOk={handleFork}
        okText="派生"
        cancelText="取消"
        confirmLoading={forking}
      >
        <div className="env-modal-form">
          <Alert
            type="info"
            showIcon
            className="env-hint"
            message="提示"
            description="请先对源容器执行「快照」创建快照，再在此输入其标签派生新容器；新容器将继承源容器的全部配置，仅镜像换成快照。"
          />
          <div className="form-field">
            <label>快照标签</label>
            <Input
              placeholder="如：1723000000（快照完成提示中显示的标签）"
              value={forkSnapshot}
              onChange={(e) => setForkSnapshot(e.target.value)}
            />
          </div>
          <div className="form-field">
            <label>新容器名称</label>
            <Input
              placeholder="如：my-env-v2"
              value={forkNewName}
              onChange={(e) => setForkNewName(e.target.value)}
            />
          </div>
        </div>
      </Modal>

      {/* flavor 编辑器（模板表单） */}
      <Modal
        title={isNew ? '新建模板' : `编辑 ${editing?.name}`}
        open={editing !== null}
        onCancel={() => setEditing(null)}
        onOk={handleFlavorSave}
        okText="保存"
        cancelText="取消"
        confirmLoading={saving}
        width={640}
      >
        {editing && (
          <div className="flavor-form">
            <div className="form-row">
              <label>名称</label>
              <Input
                value={editing.name}
                onChange={(e) => setEditing({ ...editing, name: e.target.value })}
                placeholder="如 chrome、dev-gui"
                disabled={!isNew}
              />
            </div>
            <div className="form-row">
              <label>镜像</label>
              <Input
                value={editing.image}
                onChange={(e) => setEditing({ ...editing, image: e.target.value })}
                placeholder="docker.io/library/ubuntu:24.04"
              />
            </div>
            <div className="form-row">
              <label>GUI 透传</label>
              <Switch
                checked={editing.gui}
                onChange={(v) => setEditing({ ...editing, gui: v })}
              />
              <span className="form-hint">注入宿主显示环境 + 字体/图标 + 用户映射</span>
            </div>
            <div className="form-row">
              <label>entry 应用</label>
              <Input
                value={editing.entry ?? ''}
                onChange={(e) => setEditing({ ...editing, entry: e.target.value || null })}
                placeholder="容器内可执行名（可留空）"
              />
            </div>
            <div className="form-row">
              <label>entry 参数</label>
              <Input
                value={editing.entry_args.join(' ')}
                onChange={(e) =>
                  setEditing({
                    ...editing,
                    entry_args: e.target.value.split(/\s+/).filter(Boolean),
                  })
                }
                placeholder="空格分隔"
              />
            </div>
            <div className="form-row">
              <label>安装命令</label>
              <Input.TextArea
                rows={3}
                value={setupText}
                onChange={(e) => setSetupText(e.target.value)}
                placeholder={'每行一条，按序执行\n如：apt-get update -qq && apt-get install -y -qq sudo'}
              />
            </div>
            <div className="form-row">
              <label>额外挂载</label>
              <Input.TextArea
                rows={2}
                value={mountsText}
                onChange={(e) => setMountsText(e.target.value)}
                placeholder={'每行一条：宿主路径:容器路径[:ro]'}
              />
            </div>
            <div className="form-row">
              <label>网络模式</label>
              <Select
                value={editing.network.mode}
                onChange={(v) =>
                  setEditing({ ...editing, network: { ...editing.network, mode: v } })
                }
                style={{ width: 160 }}
                options={[
                  { value: 'host', label: 'host（宿主网络）' },
                  { value: 'mapped', label: 'bridge（默认）' },
                ]}
              />
            </div>
          </div>
        )}
      </Modal>
    </div>
  );
}

/** antd App 包裹（message/modal 继承暗色主题与中文 locale） */
export function ContainersPanel() {
  return (
    <AntApp>
      <ContainersPanelInner />
    </AntApp>
  );
}
