import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
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
} from '@ant-design/icons';
import type { EnvView } from '../types';
import { ContainerCreateForm } from './ContainerCreateForm';
import './EnvPanel.css';

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

function EnvPanelInner() {
  const { message } = AntApp.useApp();

  const [envs, setEnvs] = useState<EnvView[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  // 新建环境（页内表单，ContainerCreateForm 自带 flavor 展开 + 统一编辑器）
  const [creating, setCreating] = useState(false);

  // 快照（按环境记录可选标签）
  const [snapshotTag, setSnapshotTag] = useState<Record<string, string>>({});

  // fork（从快照派生新环境）
  const [forkTarget, setForkTarget] = useState<EnvView | null>(null);
  const [forkSnapshot, setForkSnapshot] = useState('');
  const [forkNewName, setForkNewName] = useState('');
  const [forking, setForking] = useState(false);

  const loadEnvs = async () => {
    setLoading(true);
    setError(null);
    try {
      const result = await invoke<EnvView[]>('env_list');
      setEnvs(result);
    } catch (err: any) {
      setError(err?.message || '加载环境列表失败');
      console.error('env_list failed:', err);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    loadEnvs();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // ---------- 运行 / 关闭 ----------

  const handleStart = async (env: EnvView) => {
    try {
      await invoke('env_start', { name: env.name });
      message.success(`环境「${env.name}」已运行`);
      await loadEnvs();
    } catch (err: any) {
      message.error(err?.message || '启动环境失败');
      console.error('env_start failed:', err);
    }
  };

  const handleStop = async (env: EnvView) => {
    try {
      await invoke('env_stop', { name: env.name });
      message.success(`环境「${env.name}」已关闭（环境保留，可随时恢复）`);
      await loadEnvs();
    } catch (err: any) {
      message.error(err?.message || '关闭环境失败');
      console.error('env_stop failed:', err);
    }
  };

  // ---------- 快照 ----------

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
      message.error(err?.message || '创建快照失败');
      console.error('env_snapshot failed:', err);
    }
  };

  // ---------- fork（快照派生新环境） ----------

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
      message.error('请输入快照标签（请先对源环境执行「快照」）');
      return;
    }
    if (!newName) {
      message.error('请输入新环境名称');
      return;
    }
    setForking(true);
    try {
      await invoke('env_fork', {
        name: forkTarget.name,
        snapshot,
        newName,
      });
      message.success(`已从快照「${snapshot}」派生新环境「${newName}」并运行`);
      setForkTarget(null);
      await loadEnvs();
    } catch (err: any) {
      message.error(err?.message || '派生（fork）失败');
      console.error('env_fork failed:', err);
    } finally {
      setForking(false);
    }
  };

  // ---------- 删除 ----------

  const handleDelete = async (env: EnvView) => {
    try {
      await invoke('env_rm', { name: env.name });
      message.success(`环境「${env.name}」已删除；其快照为独立资产已保留，可用于派生（fork）`);
      await loadEnvs();
    } catch (err: any) {
      message.error(err?.message || '删除环境失败');
      console.error('env_rm failed:', err);
    }
  };

  // ---------- 渲染 ----------

  // 新建：页内表单替换列表（用户偏好页内表单，非模态）；取消/成功返回列表
  if (creating) {
    return (
      <div className="env-panel">
        <ContainerCreateForm
          onCancel={() => setCreating(false)}
          onCreated={() => {
            setCreating(false);
            loadEnvs();
          }}
        />
      </div>
    );
  }

  return (
    <div className="env-panel">
      <div className="env-panel-header">
        <Typography.Title level={4} className="env-panel-title">
          环境
        </Typography.Title>
        <Space>
          <Button icon={<ReloadOutlined />} onClick={loadEnvs} loading={loading}>
            刷新
          </Button>
          <Button type="primary" icon={<PlusOutlined />} onClick={() => setCreating(true)}>
            新建环境
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
          <Spin tip="加载环境列表…" size="large">
            <div className="spin-block" />
          </Spin>
        </div>
      ) : envs.length === 0 ? (
        <Empty
          image={Empty.PRESENTED_IMAGE_SIMPLE}
          description="暂无环境，点击「新建环境」开始"
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
                      title={`删除环境「${env.name}」？`}
                      description="将清理该环境的容器、注册配置、桌面图标与 socket 目录；其快照为独立资产将保留（可被 fork 复用）。"
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

      {/* fork：从快照派生新环境 */}
      <Modal
        title={forkTarget ? `从快照派生新环境（源：${forkTarget.name}）` : '从快照派生新环境'}
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
            description="请先对源环境执行「快照」创建快照，再在此输入其标签派生新环境；新环境将继承源环境的全部配置，仅镜像换成快照。"
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
            <label>新环境名称</label>
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

export function EnvPanel() {
  // antd App 包裹：让 message 等静态方法继承暗色主题与中文 locale
  return (
    <AntApp>
      <EnvPanelInner />
    </AntApp>
  );
}
