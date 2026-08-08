// Passthrough 管理器：容器应用导出为宿主快捷方式。
// - 扫描应用（容器固定目录）:导出/撤销/auto-start/查看 .desktop 内容
// - 自定义应用（固定目录之外,如 `google-chrome-stable --disable-dev-shm-usage`）:
//   添加/导出/auto-start/移除

import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { AppIcon } from './AppIcon';
import { IconPickerModal } from './IconPickerModal';
import type {
  AppInfo,
  PassthroughApp,
  PassthroughState,
} from '../types';

export function PassthroughManager() {
  const [apps, setApps] = useState<AppInfo[]>([]);
  const [state, setState] = useState<PassthroughState | null>(null);
  const [selectedApps, setSelectedApps] = useState<Set<string>>(new Set());
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  // 展开查看 .desktop 内容的条目
  const [expandedContent, setExpandedContent] = useState<string | null>(null);
  // 自定义应用表单
  const [customName, setCustomName] = useState('');
  const [customCmd, setCustomCmd] = useState('');
  const [addingCustom, setAddingCustom] = useState(false);
  // 导出本容器 GUI 管理界面的桌面快捷方式
  const [exportingGui, setExportingGui] = useState(false);
  // 正在选择图标的自定义应用 id（null = 弹窗关闭）
  const [iconPickerFor, setIconPickerFor] = useState<string | null>(null);

  /** 导出本容器管理 GUI 的桌面快捷方式（菜单 + 桌面，双击打开此管理界面） */
  const handleExportGuiShortcut = async () => {
    setExportingGui(true);
    setError(null);
    try {
      await invoke('export_gui_shortcut');
      setError(null);
    } catch (err: any) {
      setError(err.message || 'Failed to export GUI shortcut');
      console.error('export_gui_shortcut failed:', err);
    } finally {
      setExportingGui(false);
    }
  };

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
      setSelectedApps(new Set(stateResult.exported.map((e) => e.desktop_file)));
    } catch (err: any) {
      setError(err.message || 'Failed to load data');
      console.error('passthrough load failed:', err);
    } finally {
      setLoading(false);
    }
  };

  /** 扫描应用的 auto-start 状态（查配置条目;未配置 = false） */
  const autoStartOf = (id: string): boolean =>
    state?.configured_apps.find((a) => a.id === id)?.auto_start ?? false;

  const handleAppToggle = (desktopFile: string) => {
    const newSelected = new Set(selectedApps);
    if (newSelected.has(desktopFile)) {
      newSelected.delete(desktopFile);
    } else {
      newSelected.add(desktopFile);
    }
    setSelectedApps(newSelected);
  };

  /** 设置某应用的 auto-start（容器启动时自动拉起） */
  const handleAutoStart = async (id: string, name: string, cmd: string, enabled: boolean) => {
    setError(null);
    try {
      await invoke('passthrough_set_auto_start', { id, name, cmd, enabled });
      await loadData();
    } catch (err: any) {
      setError(err.message || 'Failed to set auto-start');
      console.error('passthrough_set_auto_start failed:', err);
    }
  };

  const handleExport = async () => {
    setError(null);
    try {
      for (const desktopFile of selectedApps) {
        const app = apps.find((a) => a.desktop_file === desktopFile);
        if (!app) continue;
        await invoke('passthrough_export', { app });
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

  /** 导出自定义应用（构造 AppInfoFrontend 走现有导出流；
   *  icon = 宿主 ~/.easytidy/icons 路径，export 时 Icon= 直接用） */
  const handleExportCustom = async (custom: PassthroughApp) => {
    setError(null);
    try {
      const app: AppInfo = {
        name: custom.name,
        icon_path: custom.icon ?? '',
        exec: custom.cmd,
        desktop_file: custom.id,
        startup_notify: false,
      };
      await invoke('passthrough_export', { app });
      await loadData();
    } catch (err: any) {
      setError(err.message || 'Failed to export custom app');
      console.error('passthrough_export (custom) failed:', err);
    }
  };

  const handleAddCustom = async () => {
    if (!customName.trim() || !customCmd.trim()) {
      setError('名称与命令不能为空');
      return;
    }
    setAddingCustom(true);
    setError(null);
    try {
      await invoke('passthrough_add_custom', { name: customName.trim(), cmd: customCmd.trim() });
      setCustomName('');
      setCustomCmd('');
      await loadData();
    } catch (err: any) {
      setError(err.message || 'Failed to add custom app');
      console.error('passthrough_add_custom failed:', err);
    } finally {
      setAddingCustom(false);
    }
  };

  const handleRemoveCustom = async (id: string) => {
    setError(null);
    try {
      await invoke('passthrough_remove_app', { id });
      await loadData();
    } catch (err: any) {
      setError(err.message || 'Failed to remove custom app');
      console.error('passthrough_remove_app failed:', err);
    }
  };

  /** 自定义应用（来自配置条目,custom: 前缀） */
  const customApps = state?.configured_apps.filter((a) => a.id.startsWith('custom:')) ?? [];

  if (loading) {
    return <div className="loading">Loading applications...</div>;
  }

  return (
    <div className="passthrough-manager">
      <div className="passthrough-header">
        <h3>Desktop Applications Passthrough</h3>
        <div className="passthrough-header-actions">
          <button
            className="secondary-button"
            onClick={handleExportGuiShortcut}
            disabled={exportingGui}
            title="导出本容器管理界面的桌面快捷方式"
          >
            {exportingGui ? '导出中…' : '导出桌面图标'}
          </button>
          <button
            className="primary-button"
            onClick={handleExport}
            disabled={selectedApps.size === 0}
          >
            Export Selected ({selectedApps.size})
          </button>
        </div>
      </div>

      {error && (
        <div className="error-message">
          {error}
        </div>
      )}

      <div className="apps-list">
        {apps.map((app) => {
          const isExported =
            state?.exported.some((e) => e.desktop_file === app.desktop_file) ?? false;
          const isSelected = selectedApps.has(app.desktop_file);
          const autoStart = autoStartOf(app.desktop_file);

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
                {/* 容器内图标：经 server（socket）拉取显示，不走宿主文件系统 */}
                <AppIcon path={app.icon_path} />
              </div>
              <div className="app-info">
                <div className="app-name">{app.name}</div>
                {app.comment && (
                  <div className="app-comment">{app.comment}</div>
                )}
                <div className="app-desktop-file">{app.desktop_file}</div>
                <label className="app-toggle">
                  <input
                    type="checkbox"
                    checked={autoStart}
                    onChange={(e) =>
                      handleAutoStart(app.desktop_file, app.name, app.exec, e.target.checked)
                    }
                  />
                  <span>容器启动时自动拉起</span>
                </label>
              </div>
              <div className="app-actions">
                {isExported && (
                  <>
                    <button
                      className="secondary-button"
                      onClick={() =>
                        setExpandedContent(expandedContent === app.desktop_file ? null : app.desktop_file)
                      }
                    >
                      {expandedContent === app.desktop_file ? '收起内容' : '查看 .desktop'}
                    </button>
                    <button
                      className="secondary-button"
                      onClick={() => handleRevoke(app.desktop_file)}
                    >
                      Revoke
                    </button>
                  </>
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

      {expandedContent && (
        <div className="desktop-content-section">
          <h4>{expandedContent} 的 .desktop 配置</h4>
          <pre className="desktop-content">
            {state?.exported.find((e) => e.desktop_file === expandedContent)?.content ?? ''}
          </pre>
        </div>
      )}

      <div className="custom-section">
        <h4>自定义应用（容器固定目录之外）</h4>
        <div className="custom-form">
          <input
            placeholder="名称，如 Chrome (自定义)"
            value={customName}
            onChange={(e) => setCustomName(e.target.value)}
          />
          <input
            placeholder="命令，如 google-chrome-stable --disable-dev-shm-usage"
            value={customCmd}
            onChange={(e) => setCustomCmd(e.target.value)}
          />
          <button className="primary-button" onClick={handleAddCustom} disabled={addingCustom}>
            {addingCustom ? '添加中…' : '添加'}
          </button>
        </div>
        {customApps.map((custom) => {
          const isExported =
            state?.exported.some((e) => e.desktop_file === custom.id) ?? false;
          return (
            <div
              key={custom.id}
              className={`app-item ${isExported ? 'exported' : ''}`}
            >
              <div className="app-icon">
                {custom.icon ? (
                  // 宿主本地图标（~/.easytidy/icons/）
                  <img
                    className="app-icon-img"
                    src={`file://${custom.icon}`}
                    alt=""
                    style={{ width: 28, height: 28, objectFit: 'contain' }}
                  />
                ) : (
                  <span>⚙️</span>
                )}
              </div>
              <div className="app-info">
                <div className="app-name">{custom.name}</div>
                <div className="app-desktop-file">{custom.cmd}</div>
                <label className="app-toggle">
                  <input
                    type="checkbox"
                    checked={custom.auto_start}
                    onChange={(e) =>
                      handleAutoStart(custom.id, custom.name, custom.cmd, e.target.checked)
                    }
                  />
                  <span>容器启动时自动拉起</span>
                </label>
              </div>
              <div className="app-actions">
                {!isExported && (
                  <button className="secondary-button" onClick={() => handleExportCustom(custom)}>
                    Export
                  </button>
                )}
                {isExported && (
                  <button className="secondary-button" onClick={() => handleRevoke(custom.id)}>
                    Revoke
                  </button>
                )}
                <button
                  className="secondary-button"
                  onClick={() => setIconPickerFor(custom.id)}
                >
                  选择图标
                </button>
                <button className="secondary-button danger" onClick={() => handleRemoveCustom(custom.id)}>
                  Remove
                </button>
              </div>
            </div>
          );
        })}
      </div>

      {/* 自定义应用图标选择（宿主机 / 容器内两个入口） */}
      <IconPickerModal
        open={iconPickerFor !== null}
        appId={iconPickerFor ?? ''}
        onClose={() => setIconPickerFor(null)}
        onChanged={loadData}
      />
    </div>
  );
}
