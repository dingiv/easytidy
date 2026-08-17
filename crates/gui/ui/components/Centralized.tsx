import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { App as AntApp, Tabs } from 'antd';
import type { ContainerSummary } from '../types';
import { ContainerCreateForm } from './ContainerCreateForm';
import { EnvPanel } from './EnvPanel';
import { ImagesPanel } from './ImagesPanel';
import { FlavorsPanel } from './FlavorsPanel';
import logo from '../assets/logo.png';

export function Centralized() {
  const { modal } = AntApp.useApp();
  const [containers, setContainers] = useState<ContainerSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [showCreateDialog, setShowCreateDialog] = useState(false);

  useEffect(() => {
    loadContainers();
  }, []);

  const loadContainers = async () => {
    setLoading(true);
    setError(null);
    try {
      const result = await invoke<ContainerSummary[]>('list_containers');
      setContainers(result);
    } catch (err: any) {
      setError(err.message || 'Failed to load containers');
      console.error('list_containers failed:', err);
    } finally {
      setLoading(false);
    }
  };

  /** 容器操作失败：Modal 展示完整错误（启动链路多步易错，红条易错过且截断） */
  const showOpError = (action: string, err: any) => {
    const msg = typeof err === 'string' ? err : err?.message || JSON.stringify(err);
    console.error(`${action} failed:`, err);
    modal.error({
      title: `${action}失败`,
      width: 620,
      content: <pre className="error-detail">{msg}</pre>,
      okText: '知道了',
    });
  };

  const handleStartContainer = async (name: string) => {
    setError(null);
    try {
      await invoke('start_container', { name });
      await loadContainers();
    } catch (err: any) {
      showOpError(`启动容器 ${name}`, err);
    }
  };

  const handleStopContainer = async (name: string) => {
    setError(null);
    try {
      await invoke('stop_container', { name });
      await loadContainers();
    } catch (err: any) {
      showOpError(`停止容器 ${name}`, err);
    }
  };

  const handleRestartContainer = async (name: string) => {
    setError(null);
    try {
      await invoke('restart_container', { name });
      await loadContainers();
    } catch (err: any) {
      showOpError(`重启容器 ${name}`, err);
    }
  };

  /** 删除容器：运行中容器必须 force（podman 语义），确认弹窗明确告知 */
  const handleRemoveContainer = async (name: string) => {
    modal.confirm({
      title: `删除容器 "${name}"?`,
      content: '运行中的容器将被强制停止并删除；easytidy 注册配置与桌面图标一并清理。',
      okText: '删除',
      okButtonProps: { danger: true },
      cancelText: '取消',
      onOk: async () => {
        try {
          await invoke('remove_container', { name, force: true });
          await loadContainers();
        } catch (err: any) {
          showOpError(`删除容器 ${name}`, err);
        }
      },
    });
  };

  const handleOpenContainer = async (name: string) => {
    setError(null);
    try {
      await invoke('open_container_window', { name });
    } catch (err: any) {
      setError(err.message || 'Failed to open container window');
      console.error('open_container_window failed:', err);
    }
  };

  return (
    <div className="centralized">
      <Tabs
        className="centralized-tabs"
        defaultActiveKey="containers"
        items={[
          {
            key: 'containers',
            label: '容器',
            children: showCreateDialog ? (
              // 新建容器：页内表单（与配置管理同一套 ContainerConfig 编辑器），
              // 取消/成功返回列表
              <ContainerCreateForm
                onCancel={() => setShowCreateDialog(false)}
                onCreated={() => {
                  setShowCreateDialog(false);
                  loadContainers();
                }}
              />
            ) : (
              <>
                <div className="centralized-header">
                  <h2>
                    <img src={logo} className="app-logo" alt="easytidy" />
                    Container Management
                  </h2>
                  <div className="header-actions">
                    <button className="secondary-button" onClick={loadContainers}>
                      Refresh
                    </button>
                    <button className="primary-button" onClick={() => setShowCreateDialog(true)}>
                      New Container
                    </button>
                  </div>
                </div>

                {error && (
                  <div className="error-message">
                    {error}
                  </div>
                )}

                {loading ? (
                  <div className="loading">Loading containers...</div>
                ) : (
                  <div className="container-list">
                    {containers.map((container) => (
                      <div key={container.name} className="container-item">
                        <div className="container-info">
                          <h3>{container.name}</h3>
                          <div className="container-meta">
                            <span className={`status status-${container.status.toLowerCase()}`}>
                              {container.status}
                            </span>
                            <span className="image">{container.image}</span>
                            {container.managed && (
                              <span className="managed-badge">Managed</span>
                            )}
                          </div>
                        </div>
                        <div className="container-actions">
                          <button
                            className="action-button"
                            onClick={() => handleStartContainer(container.name)}
                            disabled={container.status === 'running'}
                            title="Start"
                          >
                            ▶
                          </button>
                          <button
                            className="action-button"
                            onClick={() => handleStopContainer(container.name)}
                            disabled={container.status !== 'running'}
                            title="Stop"
                          >
                            ⏸
                          </button>
                          <button
                            className="action-button"
                            onClick={() => handleRestartContainer(container.name)}
                            disabled={container.status !== 'running'}
                            title="Restart"
                          >
                            ↻
                          </button>
                          <button
                            className="action-button open-button"
                            onClick={() => handleOpenContainer(container.name)}
                            title="Open"
                          >
                            Open
                          </button>
                          <button
                            className="action-button delete-button"
                            onClick={() => handleRemoveContainer(container.name)}
                            title="Delete"
                          >
                            🗑
                          </button>
                        </div>
                      </div>
                    ))}
                    {containers.length === 0 && (
                      <div className="empty-message">
                        No containers found. Create one to get started.
                      </div>
                    )}
                  </div>
                )}
              </>
            ),
          },
          {
            key: 'env',
            label: '环境',
            children: <EnvPanel />,
          },
          {
            key: 'images',
            label: '镜像',
            children: <ImagesPanel />,
          },
          {
            key: 'flavors',
            label: 'Flavor',
            children: <FlavorsPanel />,
          },
        ]}
      />
    </div>
  );
}
