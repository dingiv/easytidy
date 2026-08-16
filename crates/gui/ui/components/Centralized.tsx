import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Tabs } from 'antd';
import type { ContainerSummary } from '../types';
import { EnvPanel } from './EnvPanel';
import logo from '../assets/logo.png';

export function Centralized() {
  const [containers, setContainers] = useState<ContainerSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [showCreateDialog, setShowCreateDialog] = useState(false);
  const [createImage, setCreateImage] = useState('docker.io/library/ubuntu:latest');
  const [createName, setCreateName] = useState('');

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

  const handleCreateContainer = async () => {
    if (!createName.trim()) {
      setError('Container name is required');
      return;
    }
    setError(null);
    try {
      await invoke('create_container', {
        image: createImage,
        name: createName,
      });
      setShowCreateDialog(false);
      setCreateName('');
      await loadContainers();
    } catch (err: any) {
      setError(err.message || 'Failed to create container');
      console.error('create_container failed:', err);
    }
  };

  const handleStartContainer = async (name: string) => {
    setError(null);
    try {
      await invoke('start_container', { name });
      await loadContainers();
    } catch (err: any) {
      setError(err.message || 'Failed to start container');
      console.error('start_container failed:', err);
    }
  };

  const handleStopContainer = async (name: string) => {
    setError(null);
    try {
      await invoke('stop_container', { name });
      await loadContainers();
    } catch (err: any) {
      setError(err.message || 'Failed to stop container');
      console.error('stop_container failed:', err);
    }
  };

  const handleRestartContainer = async (name: string) => {
    setError(null);
    try {
      await invoke('restart_container', { name });
      await loadContainers();
    } catch (err: any) {
      setError(err.message || 'Failed to restart container');
      console.error('restart_container failed:', err);
    }
  };

  const handleRemoveContainer = async (name: string) => {
    if (!confirm(`Remove container "${name}"?`)) {
      return;
    }
    setError(null);
    try {
      await invoke('remove_container', { name, force: false });
      await loadContainers();
    } catch (err: any) {
      setError(err.message || 'Failed to remove container');
      console.error('remove_container failed:', err);
    }
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
            children: (
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

                {/* Create Container Dialog */}
                {showCreateDialog && (
                  <div className="dialog-overlay" onClick={() => setShowCreateDialog(false)}>
                    <div className="dialog" onClick={(e) => e.stopPropagation()}>
                      <h3>Create New Container</h3>
                      <div className="dialog-content">
                        <div className="form-group">
                          <label>Image:</label>
                          <input
                            type="text"
                            value={createImage}
                            onChange={(e) => setCreateImage(e.target.value)}
                            placeholder="docker.io/library/ubuntu:latest"
                          />
                        </div>
                        <div className="form-group">
                          <label>Name:</label>
                          <input
                            type="text"
                            value={createName}
                            onChange={(e) => setCreateName(e.target.value)}
                            placeholder="my-container"
                          />
                        </div>
                      </div>
                      <div className="dialog-actions">
                        <button className="secondary-button" onClick={() => setShowCreateDialog(false)}>
                          Cancel
                        </button>
                        <button className="primary-button" onClick={handleCreateContainer}>
                          Create
                        </button>
                      </div>
                    </div>
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
        ]}
      />
    </div>
  );
}
