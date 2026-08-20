// 容器管理（主 GUI）：已创建容器的全生命周期操作。
//
// 与模板分离：容器是实例（存快照），模板是配置的批量管理层（存意图，
// 见 FlavorsPanel）。创建走页内 ContainerCreateForm（与单实例 GUI 配置
// 管理同一套 ContainerConfig 编辑器）；模板「启动」经 createRequest
// 触发器从外部打开本面板的创建表单并预选模板（Centralized 持有该状态）。

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
  Space,
  Spin,
  Tag,
  Typography,
} from 'antd';
import {
  CameraOutlined,
  DeleteOutlined,
  ForkOutlined,
  PlayCircleOutlined,
  PlusOutlined,
  ReloadOutlined,
  StopOutlined,
  ExportOutlined,
  ToolOutlined,
} from '@ant-design/icons';
import type { EnvView } from '../types';
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

interface ContainersPanelProps {
  /** 外部请求打开创建表单（模板「启动」预选；名称就绪后自动展开）。
   *  每次触发用新 token，确保连续多次「启动」同一模板也能打开表单 */
  createRequest?: { flavor?: string; token: number } | null;
  /** 创建表单打开/关闭时同步外部状态（Centralized 清除触发器） */
  onCreateRequestConsumed?: () => void;
}

function ContainersPanelInner({
  createRequest,
  onCreateRequestConsumed,
}: ContainersPanelProps) {
  const { message } = AntApp.useApp();

  const [envs, setEnvs] = useState<EnvView[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [rebuilding, setRebuilding] = useState<string | null>(null);

  // 新建（页内表单；creatingFlavor = 模板「启动」预选）
  const [creating, setCreating] = useState(false);
  const [creatingFlavor, setCreatingFlavor] = useState<string | undefined>(undefined);

  // 快照（按容器记录可选标签）
  const [snapshotTag, setSnapshotTag] = useState<Record<string, string>>({});

  // fork（从快照派生新容器）
  const [forkTarget, setForkTarget] = useState<EnvView | null>(null);
  const [forkSnapshot, setForkSnapshot] = useState('');
  const [forkNewName, setForkNewName] = useState('');
  const [forking, setForking] = useState(false);

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

  useEffect(() => {
    load();
  }, []);

  // 消费外部创建请求（模板「启动」）：打开页内表单 + 预选
  useEffect(() => {
    if (!createRequest) return;
    setCreatingFlavor(createRequest.flavor);
    setCreating(true);
    onCreateRequestConsumed?.();
  }, [createRequest, onCreateRequestConsumed]);

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

  /** 打开容器窗口（单实例 GUI） */
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
      ) : envs.length === 0 ? (
        <Empty
          image={Empty.PRESENTED_IMAGE_SIMPLE}
          description="暂无容器，点击「新建容器」或从「模板」tab 快速启动"
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
    </div>
  );
}

/** antd App 包裹（message/modal 继承暗色主题与中文 locale） */
export function ContainersPanel(props: ContainersPanelProps) {
  return (
    <AntApp>
      <ContainersPanelInner {...props} />
    </AntApp>
  );
}
