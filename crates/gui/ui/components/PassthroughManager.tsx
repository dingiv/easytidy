// Passthrough 管理器：容器应用导出为宿主快捷方式。
// - 扫描应用（容器固定目录）:导出/撤销/auto-start/查看 .desktop 内容
// - 自定义应用（固定目录之外,如 `google-chrome-stable --disable-dev-shm-usage`）:
//   添加/导出/auto-start/移除

import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { errMsg } from '../lib/errors';
import { App as AntApp, Dropdown, Radio, Tooltip } from 'antd';
import {
  DownOutlined,
  PictureOutlined,
  PlayCircleOutlined,
  PushpinFilled,
  PushpinOutlined,
} from '@ant-design/icons';
import { AppIcon } from './AppIcon';
import { IconPickerModal } from './IconPickerModal';
import { useFavoritesStore } from '../stores/favoritesStore';
import type {
  AppInfo,
  PassthroughApp,
  PassthroughState,
} from '../types';

export function PassthroughManager() {
  const { message } = AntApp.useApp();
  const [apps, setApps] = useState<AppInfo[]>([]);
  const [state, setState] = useState<PassthroughState | null>(null);
  const [selectedApps, setSelectedApps] = useState<Set<string>>(new Set());
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  // 正在启动的应用 id（按钮加载态 + 防重复点击；null = 无进行中）
  const [launchingId, setLaunchingId] = useState<string | null>(null);
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
  // 新增应用表单：已选定的图标宿主路径（null = 未选）
  const [customIcon, setCustomIcon] = useState<string | null>(null);
  // 新增应用表单的容器内选图标弹窗（独立选取模式）
  const [pickingNewIcon, setPickingNewIcon] = useState(false);

  /** 导出本容器管理 GUI 的桌面快捷方式（菜单 + 桌面，双击打开此管理界面） */
  const handleExportGuiShortcut = async () => {
    setExportingGui(true);
    setError(null);
    try {
      await invoke('export_gui_shortcut');
      setError(null);
    } catch (err: any) {
      setError(errMsg(err, 'Failed to export GUI shortcut'));
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
      // 收藏列表同步到 store（工具栏订阅展示）
      useFavoritesStore.getState().setPinned(stateResult.pinned ?? []);
    } catch (err: any) {
      setError(errMsg(err, 'Failed to load data'));
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
      setError(errMsg(err, 'Failed to set auto-start'));
      console.error('passthrough_set_auto_start failed:', err);
    }
  };

  /** 收藏/取消收藏（pin 到工具栏） */
  const handlePinToggle = async (
    app: { id: string; name: string; cmd: string; icon?: string | null },
    pinned: boolean,
  ) => {
    setError(null);
    try {
      await invoke('passthrough_set_pinned', {
        id: app.id,
        name: app.name,
        cmd: app.cmd,
        iconPath: app.icon ?? null,
        pinned,
      });
      // 即时更新 store（工具栏无需等待重新加载）
      if (pinned) {
        useFavoritesStore
          .getState()
          .upsertPinned({ id: app.id, name: app.name, cmd: app.cmd, icon: app.icon ?? undefined });
      } else {
        useFavoritesStore.getState().removePinned(app.id);
      }
      await loadData();
    } catch (err: any) {
      setError(errMsg(err, 'Failed to pin app'));
      console.error('passthrough_set_pinned failed:', err);
    }
  };

  /** 是否已收藏（pin 到工具栏） */
  const isPinned = (id: string): boolean =>
    state?.pinned.some((p) => p.id === id) ?? false;

  /** 容器自启动模式（systemd user unit：off/silent/gui） */
  const handleSetBootMode = async (mode: string) => {
    setError(null);
    try {
      await invoke('passthrough_set_boot_mode', { mode });
      await loadData();
    } catch (err: any) {
      setError(errMsg(err, '设置自启动模式失败'));
      console.error('passthrough_set_boot_mode failed:', err);
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
      setError(errMsg(err, 'Failed to export apps'));
      console.error('passthrough_export failed:', err);
    }
  };

  const handleRevoke = async (desktopFile: string) => {
    setError(null);
    try {
      await invoke('passthrough_revoke', { desktopFile });
      await loadData();
    } catch (err: any) {
      setError(errMsg(err, 'Failed to revoke app'));
      console.error('passthrough_revoke failed:', err);
    }
  };

  /** 立即启动容器内应用（经 server apps.launch 拉起，server 保活、独立于连接存活）。
   *  列表里点一下即拉起某个扫描到的 / 自定义应用，无需先收藏。返回 pid，
   *  命令即时退出（如未安装 → 127）时后端已回报错误。 */
  const handleLaunch = async (id: string, name: string, cmd: string) => {
    setError(null);
    setLaunchingId(id);
    try {
      const pid = await invoke<number>('passthrough_launch_app', { id, name, cmd });
      message.success(`${name} 已启动 (pid=${pid})`);
    } catch (err: any) {
      setError(errMsg(err, `启动 ${name} 失败`));
      console.error('passthrough_launch_app failed:', err);
    } finally {
      setLaunchingId(null);
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
      setError(errMsg(err, 'Failed to export custom app'));
      console.error('passthrough_export (custom) failed:', err);
    }
  };

  /** 新增应用表单：图标下拉（从宿主机导入 / 从容器导入 / 清除） */
  const handleIconMenu = async ({ key }: { key: string }) => {
    setError(null);
    try {
      if (key === 'host') {
        // 从宿主机导入：宿主原生对话框（正常宿主 API，不涉及容器）
        const hostPath = await invoke<string>('passthrough_pick_host_icon');
        setCustomIcon(hostPath);
      } else if (key === 'container') {
        // 从容器导入：容器内迷你文件浏览器（独立选取模式弹窗）
        setPickingNewIcon(true);
      } else if (key === 'clear') {
        setCustomIcon(null);
      }
    } catch (err: any) {
      setError(errMsg(err, '导入图标失败'));
      console.error('import icon failed:', err);
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
      const id = `custom:${customName.trim()}`;
      await invoke('passthrough_add_custom', { name: customName.trim(), cmd: customCmd.trim() });
      // 表单选定的图标 → 写入应用配置（导出时 Icon= 直接用）
      if (customIcon) {
        await invoke('passthrough_set_custom_icon', { id, icon: customIcon });
      }
      setCustomName('');
      setCustomCmd('');
      setCustomIcon(null);
      await loadData();
    } catch (err: any) {
      setError(errMsg(err, 'Failed to add custom app'));
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
      setError(errMsg(err, 'Failed to remove custom app'));
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

      {/* 容器自启动（登录时）：静默 = 仅后台启动容器；非静默 = 再拉起管理窗口。
       *  应用自启动在下方各应用行的「容器启动时自动拉起」开关（容器启动时
       *  经 server apps.launch 拉起） */}
      <div className="boot-section">
        <h4>容器自启动（登录时）</h4>
        <Radio.Group
          value={state?.boot_mode ?? 'off'}
          onChange={(e) => handleSetBootMode(e.target.value)}
          optionType="button"
          size="small"
          options={[
            { label: '关闭', value: 'off' },
            { label: '静默启动', value: 'silent' },
            { label: '非静默（+GUI）', value: 'gui' },
          ]}
        />
        <div className="boot-hint">
          {state?.boot_mode === 'gui'
            ? '用户登录后启动容器，并打开该容器的管理窗口'
            : state?.boot_mode === 'silent'
              ? '用户登录后仅后台启动容器，不打开窗口'
              : '未启用：用户登录后不自动启动容器'}
        </div>
      </div>

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
                <Tooltip title="立即启动">
                  <button
                    className="secondary-button icon-only"
                    onClick={() => handleLaunch(app.desktop_file, app.name, app.exec)}
                    disabled={launchingId === app.desktop_file}
                  >
                    <PlayCircleOutlined />
                  </button>
                </Tooltip>
                <Tooltip title={isPinned(app.desktop_file) ? '取消收藏' : '收藏到工具栏'}>
                  <button
                    className={`secondary-button icon-only ${isPinned(app.desktop_file) ? 'pinned' : ''}`}
                    onClick={() =>
                      handlePinToggle(
                        { id: app.desktop_file, name: app.name, cmd: app.exec, icon: app.icon_path },
                        !isPinned(app.desktop_file),
                      )
                    }
                  >
                    {isPinned(app.desktop_file) ? <PushpinFilled /> : <PushpinOutlined />}
                  </button>
                </Tooltip>
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
          {/* 图标两个导入路径：从宿主机导入（正常宿主 API）/ 从容器导入 */}
          <Dropdown
            menu={{
              items: [
                { key: 'host', label: '从宿主机导入…' },
                { key: 'container', label: '从容器导入…' },
                ...(customIcon ? [{ key: 'clear', label: '清除图标' }] : []),
              ],
              onClick: handleIconMenu,
            }}
            placement="bottomRight"
          >
            <button
              className="secondary-button custom-icon-btn"
              title={customIcon ? customIcon : '选择应用图标（宿主机/容器）'}
            >
              {customIcon ? (
                <img src={`file://${customIcon}`} className="custom-icon-preview" alt="" />
              ) : (
                <PictureOutlined />
              )}
              <DownOutlined style={{ fontSize: 10 }} />
            </button>
          </Dropdown>
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
                <Tooltip title="立即启动">
                  <button
                    className="secondary-button icon-only"
                    onClick={() => handleLaunch(custom.id, custom.name, custom.cmd)}
                    disabled={launchingId === custom.id}
                  >
                    <PlayCircleOutlined />
                  </button>
                </Tooltip>
                <Tooltip title={isPinned(custom.id) ? '取消收藏' : '收藏到工具栏'}>
                  <button
                    className={`secondary-button icon-only ${isPinned(custom.id) ? 'pinned' : ''}`}
                    onClick={() =>
                      handlePinToggle(
                        { id: custom.id, name: custom.name, cmd: custom.cmd, icon: custom.icon },
                        !isPinned(custom.id),
                      )
                    }
                  >
                    {isPinned(custom.id) ? <PushpinFilled /> : <PushpinOutlined />}
                  </button>
                </Tooltip>
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
                <Tooltip title="更换图标（宿主机/容器）">
                  <button
                    className="secondary-button icon-only"
                    onClick={() => setIconPickerFor(custom.id)}
                  >
                    <PictureOutlined />
                  </button>
                </Tooltip>
                <button className="secondary-button danger" onClick={() => handleRemoveCustom(custom.id)}>
                  Remove
                </button>
              </div>
            </div>
          );
        })}
      </div>

      {/* 自定义应用图标选择（宿主机 / 容器内两个入口；
       *  行内=写入配置；新增表单=独立选取模式经 onPicked 回填） */}
      <IconPickerModal
        open={iconPickerFor !== null || pickingNewIcon}
        appId={iconPickerFor}
        onClose={() => {
          setIconPickerFor(null);
          setPickingNewIcon(false);
        }}
        onChanged={loadData}
        onPicked={(path) => setCustomIcon(path)}
      />
    </div>
  );
}
