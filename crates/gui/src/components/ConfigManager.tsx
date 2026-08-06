import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type { ContainerConfig } from '../types';

export function ConfigManager() {
  const [config, setConfig] = useState<ContainerConfig | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [entryAppInput, setEntryAppInput] = useState('');
  const [silentBoot, setSilentBoot] = useState(false);

  useEffect(() => {
    loadConfig();
  }, []);

  const loadConfig = async () => {
    setLoading(true);
    setError(null);
    try {
      const result = await invoke<ContainerConfig>('config_get');
      setConfig(result);
      setEntryAppInput(result.entry_app || '');
      setSilentBoot(result.silent_boot);
    } catch (err: any) {
      setError(err.message || 'Failed to load configuration');
      console.error('config_get failed:', err);
    } finally {
      setLoading(false);
    }
  };

  const handleSaveEntryApp = async () => {
    setError(null);
    try {
      await invoke('config_set', {
        key: 'entry_app',
        value: entryAppInput || null,
      });
      await loadConfig();
    } catch (err: any) {
      setError(err.message || 'Failed to save entry app');
      console.error('config_set failed:', err);
    }
  };

  const handleToggleSilentBoot = async () => {
    setError(null);
    const newValue = !silentBoot;
    try {
      await invoke('config_set', {
        key: 'silent_boot',
        value: newValue,
      });
      setSilentBoot(newValue);
    } catch (err: any) {
      setError(err.message || 'Failed to toggle silent boot');
      console.error('config_set failed:', err);
    }
  };

  const handleRebuildContainer = async () => {
    if (!confirm('Rebuild container? This will remove and recreate the container.')) {
      return;
    }
    setError(null);
    try {
      // For M3, this is a stub - full implementation would:
      // 1. Stop container
      // 2. Remove container
      // 3. Recreate with same config
      alert('Container rebuild will be implemented in M4');
    } catch (err: any) {
      setError(err.message || 'Failed to rebuild container');
      console.error('rebuild failed:', err);
    }
  };

  if (loading) {
    return <div className="loading">Loading configuration...</div>;
  }

  if (!config) {
    return <div className="error-message">Failed to load configuration</div>;
  }

  return (
    <div className="config-manager">
      <h3>Container Configuration</h3>

      {error && (
        <div className="error-message">
          {error}
        </div>
      )}

      {/* Network Configuration */}
      <section className="config-section">
        <h4>Network</h4>
        <div className="config-row">
          <label>Mode:</label>
          <span>{config.network.mode}</span>
        </div>
        {config.network.port_mappings && config.network.port_mappings.length > 0 && (
          <div className="config-row">
            <label>Port Mappings:</label>
            <div>
              {config.network.port_mappings.map((mapping, idx) => (
                <div key={idx}>
                  {mapping.container_port} → {mapping.host_port}
                </div>
              ))}
            </div>
          </div>
        )}
      </section>

      {/* Mounts */}
      <section className="config-section">
        <h4>Mounts</h4>
        {config.mounts.length > 0 ? (
          <div className="mounts-list">
            {config.mounts.map((mount, idx) => (
              <div key={idx} className="mount-item">
                <code>{mount.source}</code>
                <span>→</span>
                <code>{mount.target}</code>
              </div>
            ))}
          </div>
        ) : (
          <div className="empty-message">No mounts configured</div>
        )}
      </section>

      {/* Entry App */}
      <section className="config-section">
        <h4>Entry Application</h4>
        <div className="config-row">
          <input
            type="text"
            value={entryAppInput}
            onChange={(e) => setEntryAppInput(e.target.value)}
            placeholder="Command to run on container start"
          />
          <button className="secondary-button" onClick={handleSaveEntryApp}>
            Save
          </button>
        </div>
      </section>

      {/* Silent Boot */}
      <section className="config-section">
        <h4>Boot Options</h4>
        <div className="config-row">
          <label>
            <input
              type="checkbox"
              checked={silentBoot}
              onChange={handleToggleSilentBoot}
            />
            Silent Boot (auto-start entry app without GUI)
          </label>
        </div>
      </section>

      {/* Rebuild */}
      <section className="config-section">
        <h4>Actions</h4>
        <button className="danger-button" onClick={handleRebuildContainer}>
          Rebuild Container (M4)
        </button>
      </section>
    </div>
  );
}
