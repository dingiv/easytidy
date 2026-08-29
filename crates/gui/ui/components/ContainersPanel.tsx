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
  Empty,
  Input,
  Popconfirm,
  Space,
  Spin,
  Tag,
  Typography,
} from 'antd';
import {
  CameraOutlined,
  DeleteOutlined,
  PlayCircleOutlined,
  ReloadOutlined,
  StopOutlined,
  ExportOutlined,
  ToolOutlined,
} from '@ant-design/icons';
import type { EnvView } from '../types';
import './ContainersPanel.css';

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
  const [rebuilding, setRebuilding] = useState<string | null>(null);

  // 快照(按容器记录可选标签)
  const [snapshotTag, setSnapshotTag] = useState<Record<string, string>>({});

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

  /** 打开容器窗口(Worker GUI) */
  const handleOpen = async (env: EnvView) => {
    try {
      await invoke('open_container_window', { name: env.name });
    } catch (err: any) {
      setError(errMsg(err, '打开容器窗口失败'));
      console.error('open_container_window failed:', err);
    }
  };

  /** 重建:按注册表当前配置 commit → 删旧 → 同名重建 → 启动 */
  const handleRebuild = async (env: EnvView) => {
    setRebuilding(env.name);
    try {
      await invoke('env_rebuild', { name: env.name });
      await load();
    } catch (err: any) {
      setError(errMsg(err, `重建容器「${env.name}」失败`));
      console.error('env_rebuild failed:', err);
    } finally {
      setRebuilding(null);
    }
  };

  /** 快照(commit 当前文件系统层为独立备份资产；标签可用于后续手动恢复/重建。
   *  未接管容器同样适用——commit 是 podman 原生能力，不依赖 easytidy 注册配置) */
  const handleSnapshot = async (env: EnvView) => {
    const tag = (snapshotTag[env.name] ?? '').trim();
    try {
      const imageRef = await invoke<string>('env_snapshot', {
        name: env.name,
        tag: tag || null,
      });
      setSnapshotTag((prev) => ({ ...prev, [env.name]: '' }));
      message.success(`快照已创建：${imageRef}（可作为基础镜像新建容器）`);
      await load();
    } catch (err: any) {
      setError(errMsg(err, `创建容器「${env.name}」快照失败`));
      console.error('env_snapshot failed:', err);
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
                      <Tag color={managed ? st.color : 'orange'}>
                        {managed ? st.label : '未接管'}
                      </Tag>
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
                    {/* 打开/重建依赖 easytidy server 或注册配置，未接管容器不适用；
                        快照仅 podman commit，不依赖注册配置，未接管容器同样可快照 */}
                    {managed && (
                      <Button
                        size="small"
                        icon={<ExportOutlined />}
                        onClick={() => handleOpen(env)}
                        title="打开容器窗口(Worker GUI)"
                      >
                        打开
                      </Button>
                    )}
                    {managed && (
                      <Popconfirm
                        title="重建容器"
                        description="按注册表(config.toml)当前配置 commit → 删除 → 同名重建 → 启动。用于应用外部修改的配置文件。"
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
                    )}
                    <Popconfirm
                      title="创建快照"
                      description={
                        <Input
                          placeholder="快照标签(可选,默认时间戳)"
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