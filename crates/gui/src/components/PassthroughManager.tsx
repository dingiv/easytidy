import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type { AppInfo, PassthroughState } from '../types';

export function PassthroughManager() {
  const [apps, setApps] = useState<AppInfo[]>([]);
  const [state, setState] = useState<PassthroughState | null>(null);
  const [selectedApps, setSelectedApps] = useState<Set<string>>(new Set());
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    loadData();
  }, []);

  const loadData = async () => {
    setLoading(true);
    setError(null);
    try {
      const [appsResult, stateResult] = await Promise.all([
        invoke<AppInfo[]>('apps_list'),
        invoke<PassthroughState>('passthrough_state'),
      ]);
      setApps(appsResult);
      setState(stateResult);
      setSelectedApps(new Set(stateResult.exported_apps));
    } catch (err: any) {
      setError(err.message || 'Failed to load data');
      console.error('passthrough load failed:', err);
    } finally {
      setLoading(false);
    }
  };

  const handleAppToggle = (desktopFile: string) => {
    const newSelected = new Set(selectedApps);
    if (newSelected.has(desktopFile)) {
      newSelected.delete(desktopFile);
    } else {
      newSelected.add(desktopFile);
    }
    setSelectedApps(newSelected);
  };

  const handleExport = async () => {
    setError(null);
    try {
      for (const desktopFile of selectedApps) {
        await invoke('passthrough_export', { desktopFile });
      }
      await loadData();
    } catch (err: any) {
      setError(err.message || 'Failed to export apps');
      console.error('passthrough_export failed:', err);
    }
  };

  const handleRevoke = async (desktopFile: string) => {
    setError(null);
    try {
      await invoke('passthrough_revoke', { desktopFile });
      await loadData();
    } catch (err: any) {
      setError(err.message || 'Failed to revoke app');
      console.error('passthrough_revoke failed:', err);
    }
  };

  if (loading) {
    return <div className="loading">Loading applications...</div>;
  }

  return (
    <div className="passthrough-manager">
      <div className="passthrough-header">
        <h3>Desktop Applications Passthrough</h3>
        <button
          className="primary-button"
          onClick={handleExport}
          disabled={selectedApps.size === 0}
        >
          Export Selected ({selectedApps.size})
        </button>
      </div>

      {error && (
        <div className="error-message">
          {error}
        </div>
      )}

      <div className="auto-start-status">
        <label>
          <input
            type="checkbox"
            checked={state?.auto_start_enabled ?? false}
            disabled
          />
          Auto-start on container boot
        </label>
      </div>

      <div className="apps-list">
        {apps.map((app) => {
          const isExported = state?.exported_apps.includes(app.desktop_file) ?? false;
          const isSelected = selectedApps.has(app.desktop_file);

          return (
            <div
              key={app.desktop_file}
              className={`app-item ${isExported ? 'exported' : ''} ${isSelected ? 'selected' : ''}`}
            >
              <div className="app-checkbox">
                <input
                  type="checkbox"
                  checked={isSelected}
                  onChange={() => handleAppToggle(app.desktop_file)}
                />
              </div>
              <div className="app-icon">
                {app.icon_path && (
                  <img src={`file://${app.icon_path}`} alt="" />
                )}
                {!app.icon_path && <span>📦</span>}
              </div>
              <div className="app-info">
                <div className="app-name">{app.name}</div>
                {app.comment && (
                  <div className="app-comment">{app.comment}</div>
                )}
                <div className="app-desktop-file">{app.desktop_file}</div>
              </div>
              <div className="app-actions">
                {isExported && (
                  <button
                    className="secondary-button"
                    onClick={() => handleRevoke(app.desktop_file)}
                  >
                    Revoke
                  </button>
                )}
              </div>
            </div>
          );
        })}
        {apps.length === 0 && (
          <div className="empty-message">
            No desktop applications found in container
          </div>
        )}
      </div>
    </div>
  );
}
