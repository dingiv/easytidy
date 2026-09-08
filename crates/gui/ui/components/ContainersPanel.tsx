// 容器管理（主 GUI）：已创建容器的全生命周期操作。
//
// 与模板分离：容器是实例（存快照），模板是配置的批量管理层（存意图，
// 见 FlavorsPanel）。列表只读"实例",创建由 MasterView 通过 sidebar
// 「新建容器」图标独立开 pane(`ContainerCreateForm` 直接作为
// `new-container` pane 内容);模板「启动」经 MasterView 调 openPane
// 同样走到 `new-container`,把 flavor 预填进表单。
//
// 列表刷新:`refreshTick` 监听 — MasterView 在「new-container」表单
// 提交成功后 +1,触发本面板重新拉取(避免对容器列表做 IPC 轮询)。

import { forwardRef, useEffect, useImperativeHandle, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { errMsg } from '../lib/errors';
import {
  App as AntApp,
  Alert,
  Button,
  Card,
  Checkbox,
  Dropdown,
  Empty,
  Popconfirm,
  Space,
  Spin,
  Tag,
  Typography,
} from 'antd';
import {
  DeleteOutlined,
  DownOutlined,
  ForkOutlined,
  PlayCircleOutlined,
  ReloadOutlined,
  StopOutlined,
  ExportOutlined,
  CopyOutlined,
} from '@ant-design/icons';
import type { ContainerConfig, EnvView } from '../types';
import './ContainersPanel.css';

/** 快照/重建统一下拉的 4 个动作（均不询问用户确认，点击即执行）。
 *  「快速」= 用普通 commit 代替默认 squash（保留分层、更快）：
 *  - 快照：squash 单层镜像（小体积）/ 快速快照：普通 commit 镜像（更快）
 *    （均为独立资产，未接管容器同样适用）
 *  - 重建：squash commit + 安全重建 / 快速重建：普通 commit + 安全重建
 *    （按注册表配置，保留旧容器至新容器就绪、失败自动回滚；仅已接管） */
const ACTIONS = [
  { key: 'snapshot', label: '快照', hint: 'squash 单层，体积小' },
  { key: 'snapshot-quick', label: '快速快照', hint: '普通 commit，保留分层，更快' },
  { key: 'rebuild', label: '重建', hint: 'squash commit + 安全重建（失败回滚）' },
  { key: 'rebuild-quick', label: '快速重建', hint: '普通 commit + 安全重建（失败回滚），更快' },
] as const;

/** 下拉记忆：上次使用动作的 localStorage key（跨容器共享） */
const LAST_ACTION_KEY = 'easytidy.containers.last-action';

function readLastAction(): string {
  const v = localStorage.getItem(LAST_ACTION_KEY);
  return v && ACTIONS.some((a) => a.key === v) ? v : ACTIONS[0].key;
}

/** 暴露给 MasterView 的句柄:创建/重建等动作后触发本面板重拉 */
export interface ContainerRef {
  reload(): Promise<void>;
}

interface ContainersPanelProps {
  /** 由 MasterView 在创建/同步等动作后 +1,本面板监听触发 load() */
  refreshTick?: number;
  /** 容器卡片「复制」：读注册配置后回调 MasterView 开新建容器表单（预填） */
  onCopyConfig(config: ContainerConfig, sourceName: string): void;
}

/** 状态 → 中文标签 + 颜色(普通用户视角) */
function statusMeta(status: string): { label: string; color: string } {
  switch (status) {
    case 'running':
      return { label: '运行中', color: 'green' };
    case 'exited':
    case 'created':
    case 'paused':
      return { label: '已停止', color: 'default' };
    case 'missing':
      // 仅注册表配置存在、podman 容器已不存在（未经 easytidy 创建 / 被外部删除）
      return { label: '容器丢失', color: 'red' };
    default:
      return { label: status, color: 'default' };
  }
}

function ContainersPanelInner(
  { refreshTick = 0, onCopyConfig }: ContainersPanelProps,
  ref: React.Ref<ContainerRef>,
) {
  const { message, modal } = AntApp.useApp();
  const [envs, setEnvs] = useState<EnvView[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  // 正在执行快照/重建动作的容器名（按钮 loading 用）
  const [acting, setActing] = useState<string | null>(null);
  // 下拉记忆：上次使用动作（localStorage，跨容器共享）；菜单置顶并标记
  const [lastAction, setLastAction] = useState<string>(readLastAction);
  // 多选删除：选中的容器名集合 + 批量删除进行中
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [batchDeleting, setBatchDeleting] = useState(false);

  const load = async () => {
    setLoading(true);
    setError(null);
    try {
      setEnvs(await invoke<EnvView[]>('env_list'));
    } catch (err: any) {
      setError(errMsg(err, '加载失败'));
      console.error('load (envs) failed:', err);
    } finally {
      setLoading(false);
    }
  };

  // 暴露 reload 给 MasterView(创建/重建等动作后调用)
  useImperativeHandle(ref, () => ({ reload: load }), []);

  // 挂载即拉
  useEffect(() => {
    load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // MasterView 创建/同步成功后 +1 触发刷新
  useEffect(() => {
    if (refreshTick === 0) return;
    load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [refreshTick]);

  /** 按钮标签 = 上次使用的动作（记忆）；悬停展开全部 4 项 */
  const lastActionLabel = ACTIONS.find((a) => a.key === lastAction)?.label ?? '快照/重建';

  const handleStart = async (env: EnvView) => {
    try {
      await invoke('env_start', { name: env.name });
      await load();
    } catch (err: any) {
      setError(errMsg(err, `启动容器「${env.name}」失败`));
      console.error('env_start failed:', err);
    }
  };

  const handleStop = async (env: EnvView) => {
    try {
      await invoke('env_stop', { name: env.name });
      await load();
    } catch (err: any) {
      setError(errMsg(err, `关闭容器「${env.name}」失败`));
      console.error('env_stop failed:', err);
    }
  };

  /** 打开容器控制台(Worker GUI) */
  const handleOpen = async (env: EnvView) => {
    try {
      await invoke('open_container_window', { name: env.name });
    } catch (err: any) {
      setError(errMsg(err, '打开容器窗口失败'));
      console.error('open_container_window failed:', err);
    }
  };

  /** 快照/重建统一动作：不询问用户确认，点击即执行。 */
  const handleAction = async (env: EnvView, key: string) => {
    // 记忆：下次下拉优先显示（置顶 + 标记）
    setLastAction(key);
    localStorage.setItem(LAST_ACTION_KEY, key);
    setActing(env.name);
    try {
      switch (key) {
        case 'snapshot':
        case 'snapshot-quick': {
          const squash = key === 'snapshot';
          const imageRef = await invoke<string>('env_snapshot', {
            name: env.name,
            snapshotName: null,
            squash,
          });
          message.success(
            squash
              ? `快照已创建：${imageRef}（squash 单层）`
              : `快速快照已创建：${imageRef}（保留分层）`,
          );
          break;
        }
        case 'rebuild':
        case 'rebuild-quick': {
          const quick = key === 'rebuild-quick';
          await invoke('env_rebuild', { name: env.name, quick });
          message.success(
            quick
              ? `容器「${env.name}」快速重建完成`
              : `容器「${env.name}」已重建并启动`,
          );
          break;
        }
      }
      await load();
    } catch (err: any) {
      const label = ACTIONS.find((a) => a.key === key)?.label ?? key;
      setError(errMsg(err, `容器「${env.name}」${label}失败`));
      console.error('container action failed:', err);
    } finally {
      setActing(null);
    }
  };

  const handleDelete = async (env: EnvView) => {
    try {
      await invoke('env_rm', { name: env.name });
      // 若该容器在多选集合里，删除后同步移除（避免残留已删名字）
      setSelected((prev) => {
        if (!prev.has(env.name)) return prev;
        const next = new Set(prev);
        next.delete(env.name);
        return next;
      });
      await load();
    } catch (err: any) {
      setError(errMsg(err, `删除容器「${env.name}」失败`));
      console.error('env_rm failed:', err);
    }
  };

  /** 复制配置：读注册表配置 → MasterView 开「新建容器」pane 预填（名字 <原名>_copy）。
   *  仅已接管容器可用（未接管无注册配置）；missing 配置仍在,同样可复制。 */
  const handleCopy = async (env: EnvView) => {
    try {
      const config = await invoke<ContainerConfig>('env_copy_config', { name: env.name });
      onCopyConfig(config, env.name);
    } catch (err: any) {
      setError(errMsg(err, `复制容器「${env.name}」配置失败`));
      console.error('env_copy_config failed:', err);
    }
  };

  /** 多选删除：勾选 / 取消勾选单个容器 */
  const toggleSelect = (name: string, checked: boolean) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (checked) next.add(name);
      else next.delete(name);
      return next;
    });
  };

  const allSelected = envs.length > 0 && selected.size === envs.length;

  /** 全选 / 取消全选 */
  const toggleSelectAll = () => {
    setSelected(allSelected ? new Set() : new Set(envs.map((e) => e.name)));
  };

  /** 批量删除：弹确认（列出将删容器名）→ 逐个 env_rm */
  const confirmBatchDelete = () => {
    const names = Array.from(selected);
    if (names.length === 0) return;
    modal.confirm({
      title: `删除 ${names.length} 个容器?`,
      width: 480,
      content: (
        <div>
          <p style={{ marginBottom: 8 }}>
            将删除以下容器（已接管的会一并清理注册配置、桌面图标与 socket 目录；其快照镜像作为独立资产保留）：
          </p>
          <ul style={{ maxHeight: 220, overflow: 'auto', paddingLeft: 18, margin: 0 }}>
            {names.map((n) => (
              <li key={n}>{n}</li>
            ))}
          </ul>
        </div>
      ),
      okText: '删除',
      okButtonProps: { danger: true },
      cancelText: '取消',
      onOk: () => batchDelete(names),
    });
  };

  /** 逐个 env_rm（串行：PodmanState 为锁），收集成功/失败，汇总提示 */
  const batchDelete = async (names: string[]) => {
    setBatchDeleting(true);
    let ok = 0;
    const failed: { name: string; err: string }[] = [];
    for (const name of names) {
      try {
        await invoke('env_rm', { name });
        ok += 1;
      } catch (err: any) {
        failed.push({ name, err: errMsg(err, '删除失败') });
        console.error('batch env_rm failed:', name, err);
      }
    }
    setBatchDeleting(false);
    setSelected(new Set());
    await load();
    if (failed.length === 0) {
      message.success(`已删除 ${ok} 个容器`);
    } else {
      message.warning(`删除完成：成功 ${ok} 个，失败 ${failed.length} 个`);
      modal.error({
        title: `部分容器删除失败（${failed.length} 个）`,
        width: 480,
        content: (
          <ul style={{ maxHeight: 260, overflow: 'auto', paddingLeft: 18, margin: 0 }}>
            {failed.map((f) => (
              <li key={f.name}>
                <strong>{f.name}</strong>：{f.err}
              </li>
            ))}
          </ul>
        ),
      });
    }
  };

  return (
    <div className="env-panel">
      <div className="env-panel-header">
        <Typography.Title level={4} className="env-panel-title">
          容器
        </Typography.Title>
        <Space>
          <Button size="small" onClick={toggleSelectAll}>
            {allSelected ? '取消全选' : '全选'}
          </Button>
          {selected.size > 0 && (
            <>
              <Typography.Text type="secondary">
                已选 {selected.size} / {envs.length}
              </Typography.Text>
              <Button
                size="small"
                danger
                icon={<DeleteOutlined />}
                loading={batchDeleting}
                onClick={confirmBatchDelete}
              >
                批量删除
              </Button>
            </>
          )}
          <Button icon={<ReloadOutlined />} onClick={load} loading={loading}>
            刷新
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
      ) : envs.length === 0 ? (
        <Empty
          image={Empty.PRESENTED_IMAGE_SIMPLE}
          description="暂无容器,点击左侧「配置编辑器」或先到「模板」面板按模板快速启动"
        />
      ) : (
        <div className="env-list">
          {envs.map((env) => {
            const st = statusMeta(env.status);
            const running = env.status === 'running';
            const missing = env.status === 'missing';
            const managed = env.managed;
            return (
              <Card
                key={env.name}
                size="small"
                className={`env-card ${selected.has(env.name) ? 'env-card-selected' : ''}`}
              >
                <div className="env-card-body">
                  <div className="env-card-main">
                    <Checkbox
                      checked={selected.has(env.name)}
                      onChange={(e) => toggleSelect(env.name, e.target.checked)}
                    />
                    <div className="env-info">
                    <div className="env-name-row">
                      <Typography.Text strong className="env-name">
                        {env.name}
                      </Typography.Text>
                      <Tag color={managed ? 'blue' : 'orange'}>
                        {managed ? '已接管' : '未接管'}
                      </Tag>
                      <Tag color={st.color}>{st.label}</Tag>
                    </div>
                    <Typography.Text code className="env-image">
                      {env.image}
                    </Typography.Text>
                  </div>
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
                    {/* 控制台/重建依赖 easytidy server 或注册配置，未接管容器不适用；
                        快照仅 podman commit，不依赖注册配置，未接管容器同样可快照 */}
                    {managed && (
                      <Button
                        size="small"
                        icon={<ExportOutlined />}
                        onClick={() => handleOpen(env)}
                        title="打开容器控制台(Worker GUI)"
                      >
                        控制台
                      </Button>
                    )}
                    {/* 复制：读注册配置预填新建容器表单（名字 <原名>_copy）；
                        仅已接管（未接管无配置）；missing 配置仍在,同样可复制 */}
                    {managed && (
                      <Button
                        size="small"
                        icon={<CopyOutlined />}
                        onClick={() => handleCopy(env)}
                        title="复制配置：用该容器的配置预填「新建容器」表单"
                      >
                        复制
                      </Button>
                    )}
                    {/* 快照/重建统一下拉：hover 展开全部动作（记忆：上次使用置顶+标记），
                        均不询问确认点击即执行。快照未接管容器同样适用；重建仅已接管。 */}
                    <Dropdown
                      trigger={['hover']}
                      disabled={missing}
                      menu={{
                        items: [
                          ...ACTIONS.filter((a) => a.key === lastAction),
                          ...ACTIONS.filter((a) => a.key !== lastAction),
                        ].map((a) => {
                          const isRebuild = a.key === 'rebuild' || a.key === 'rebuild-quick';
                          return {
                            key: a.key,
                            disabled: acting === env.name || (isRebuild && !managed),
                            label: (
                              <div className="action-menu-item">
                                <div>
                                  {a.label}
                                  {a.key === lastAction && (
                                    <Tag color="blue" className="action-menu-last">
                                      上次
                                    </Tag>
                                  )}
                                </div>
                                <div className="action-menu-hint">{a.hint}</div>
                              </div>
                            ),
                          };
                        }),
                        onClick: ({ key }) => handleAction(env, key),
                      }}
                    >
                      <Button
                        size="small"
                        icon={<ForkOutlined />}
                        loading={acting === env.name}
                        disabled={missing}
                        title="快照 / 重建（悬停展开全部动作）"
                      >
                        {lastActionLabel}
                        <DownOutlined />
                      </Button>
                    </Dropdown>
                    <Popconfirm
                      title={`删除容器「${env.name}」?`}
                      description={
                        managed
                          ? '将清理该容器的容器、注册配置、桌面图标与 socket 目录;其快照为独立镜像资产将保留。'
                          : '将删除该容器（未接管，无 easytidy 注册配置与图标，仅移除容器本身）。'
                      }
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
    </div>
  );
}

/** antd App 包裹由 MasterView 提供(本面板仅暴露 forwardRef 容器) */
export const ContainersPanel = forwardRef<ContainerRef, ContainersPanelProps>(
  ContainersPanelInner,
);