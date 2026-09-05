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
} from '@ant-design/icons';
import type { EnvView } from '../types';
import './ContainersPanel.css';

/** 快照/重建统一下拉的 4 个动作（均不询问用户确认，点击即执行）。
 *  快照：podman commit 独立资产（未接管容器同样适用）——squash 单层（小体积）
 *  vs 普通 commit（保留分层，更快）。重建：按注册表配置（仅已接管）——安全
 *  （保留旧容器至新容器就绪、失败回滚）vs 快速（删旧重建、无回滚）。 */
const ACTIONS = [
  { key: 'snapshot', label: '快照', hint: 'squash 单层，体积小' },
  { key: 'snapshot-quick', label: '快速快照', hint: '普通 commit，保留分层，更快' },
  { key: 'rebuild', label: '重建', hint: '安全：保留旧容器至新容器就绪，失败回滚' },
  { key: 'rebuild-quick', label: '快速重建', hint: 'commit 数据后删旧重建，无回滚' },
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
  { refreshTick = 0 }: ContainersPanelProps,
  ref: React.Ref<ContainerRef>,
) {
  const { message } = AntApp.useApp();
  const [envs, setEnvs] = useState<EnvView[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  // 正在执行快照/重建动作的容器名（按钮 loading 用）
  const [acting, setActing] = useState<string | null>(null);
  // 下拉记忆：上次使用动作（localStorage，跨容器共享）；菜单置顶并标记
  const [lastAction, setLastAction] = useState<string>(readLastAction);

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
      await load();
    } catch (err: any) {
      setError(errMsg(err, `删除容器「${env.name}」失败`));
      console.error('env_rm failed:', err);
    }
  };

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
              <Card key={env.name} size="small" className="env-card">
                <div className="env-card-body">
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