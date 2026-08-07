// per-container 窗口：浏览器式多面板（open + close + multi-panel）。
//
// - 头部工具栏：方形图标按钮打开面板——Terminal（下拉选身份 node/root）、
//   Passthrough、Config；Close Container（圆角方形图标，唯一保留的容器操作）
// - 标签栏：每个打开的面板一个标签（标题 + 小叉关闭）；重复打开同类面板聚焦已有
// - 内容区：所有打开的面板常驻渲染（display 切换）——终端会话不因切换丢失；
//   关闭面板才卸载（终端流经模块级缓存 + server attach 保持，重开回放）

import { useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Dropdown, Tooltip } from 'antd';
import {
  CloseOutlined,
  CodeOutlined,
  DownOutlined,
  ExportOutlined,
  MenuFoldOutlined,
  MenuUnfoldOutlined,
  SettingOutlined,
} from '@ant-design/icons';
import { useUiStore } from '../stores/uiStore';
import { Terminal } from './Terminal';
import { FileBrowser } from './FileBrowser';
import { FileEditor } from './FileEditor';
import { PassthroughManager } from './PassthroughManager';
import { ConfigManager } from './ConfigManager';

interface PerContainerProps {
  containerName: string;
}

/** 打开的面板 */
interface Pane {
  id: string;
  kind: 'terminal' | 'passthrough' | 'config' | 'editor';
  title: string;
  /** 终端身份（root 终端独立会话） */
  asRoot?: boolean;
  /** 编辑器面板：文件路径 */
  path?: string;
}

const AUTO_COLLOPSE_WIDTH = 150

export function PerContainer({ containerName }: PerContainerProps) {
  const [panes, setPanes] = useState<Pane[]>(() => [
    // 默认打开一个 node 终端
    { id: useUiStore.getState().nextPaneId(), kind: 'terminal', title: '终端', asRoot: false },
  ]);
  const [activePaneId, setActivePaneId] = useState<string | null>(null);
  // 初始激活第一个 pane（惰性：首个渲染后设置）
  const activeId = activePaneId ?? panes[0]?.id ?? null;

  // 侧边栏：折叠 + 宽度（拖拽调宽，低于 200px 自动折叠）
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  const [sidebarWidth, setSidebarWidth] = useState(300);
  const dragRef = useRef<{ startX: number; startWidth: number } | null>(null);

  const dragEnd = () => {
    dragRef.current = null;
    document.removeEventListener('mousemove', onDragMove);
    document.removeEventListener('mouseup', dragEnd);
  };
  const onDragMove = (e: MouseEvent) => {
    if (!dragRef.current) return;
    const w = dragRef.current.startWidth + (e.clientX - dragRef.current.startX);
    if (w < AUTO_COLLOPSE_WIDTH) {
      // 低于 200px 自动折叠
      setSidebarCollapsed(true);
      dragEnd();
    } else {
      setSidebarWidth(Math.min(w, 500));
    }
  };
  const onDragStart = (e: React.MouseEvent) => {
    dragRef.current = { startX: e.clientX, startWidth: sidebarWidth };
    document.addEventListener('mousemove', onDragMove);
    document.addEventListener('mouseup', dragEnd);
    e.preventDefault();
  };

  const handleCloseContainer = async () => {
    try {
      await invoke('container_shutdown');
    } catch (err) {
      console.error('container_shutdown failed:', err);
    }
  };

  /** 打开面板：同类（terminal 含身份）已存在则聚焦，否则新建 */
  const openPane = (kind: Pane['kind'], asRoot?: boolean) => {
    setPanes((prev) => {
      const existing = prev.find((p) => p.kind === kind && p.asRoot === asRoot);
      if (existing) {
        setActivePaneId(existing.id);
        return prev;
      }
      const title =
        kind === 'terminal' ? (asRoot ? '终端 (root)' : '终端') :
        kind === 'passthrough' ? 'Passthrough' : '配置';
      const pane: Pane = {
        id: useUiStore.getState().nextPaneId(),
        kind,
        title,
        asRoot,
      };
      setActivePaneId(pane.id);
      return [...prev, pane];
    });
  };

  /** 关闭面板：关闭后激活相邻面板 */
  const closePane = (id: string) => {
    setPanes((prev) => {
      const idx = prev.findIndex((p) => p.id === id);
      if (idx === -1) return prev;
      const next = prev.filter((p) => p.id !== id);
      if (activeId === id) {
        const neighbor = next[Math.min(idx, next.length - 1)];
        setActivePaneId(neighbor ? neighbor.id : null);
      }
      return next;
    });
  };

  /** 打开文本编辑器面板（文件浏览器双击回调）：同文件已开则聚焦，否则新建 */
  const openEditor = (path: string) => {
    setPanes((prev) => {
      const existing = prev.find((p) => p.kind === 'editor' && p.path === path);
      if (existing) {
        setActivePaneId(existing.id);
        return prev;
      }
      const name = path.split('/').filter(Boolean).pop() ?? path;
      const pane: Pane = {
        id: useUiStore.getState().nextPaneId(),
        kind: 'editor',
        title: name,
        path,
      };
      setActivePaneId(pane.id);
      return [...prev, pane];
    });
  };

  return (
    <div className="per-container app">
      {/* 左右布局：左 = 标题 + 文件浏览器；右 = 工具栏 + 标签栏 + 面板 */}
      <div className="per-layout">
        {/* 左列：标题 + 文件浏览器；右边界可拖拽调宽（折叠按钮在工具栏第一个） */}
        <aside
          className={`per-left ${sidebarCollapsed ? 'collapsed' : ''}`}
          style={{ width: sidebarCollapsed ? 0 : sidebarWidth }}
        >
          {!sidebarCollapsed && (
            <>
              <div className="menu-title">easytidy - {containerName}</div>
              <div className="per-left-browser">
                {/* 双击文本文件 → 在右侧面板打开编辑器 */}
                <FileBrowser onOpenFile={openEditor} />
              </div>
            </>
          )}
          {!sidebarCollapsed && (
            <div className="sidebar-resizer" onMouseDown={onDragStart} title="拖动调整宽度" />
          )}
        </aside>

        {/* 右列：工具栏（上）+ 标签栏 + 面板 */}
        <div className="per-right">
          {/* 工具栏：方形图标按钮（折叠侧边栏放第一个，close 跟在 Config 后） */}
          <header className="menu-bar">
            <nav className="menu-items">
              <Tooltip title={sidebarCollapsed ? '展开侧边栏' : '折叠侧边栏'} mouseEnterDelay={4}>
                <button
                  className="tool-button"
                  onClick={() => setSidebarCollapsed((c) => !c)}
                >
                  {sidebarCollapsed ? <MenuUnfoldOutlined /> : <MenuFoldOutlined />}
                </button>
              </Tooltip>
              <Dropdown
                menu={{
                  items: [
                    { key: 'user', label: 'node' },
                    { key: 'root', label: 'root' },
                  ],
                  onClick: ({ key }) => openPane('terminal', key === 'root'),
                }}
                placement="bottomLeft"
              >
                <Tooltip title="打开终端（node/root）" mouseEnterDelay={4}>
                  <button className="tool-button">
                    <CodeOutlined />
                    <DownOutlined style={{ fontSize: 10 }} />
                  </button>
                </Tooltip>
              </Dropdown>
              <Tooltip title="打开 Passthrough" mouseEnterDelay={4}>
                <button className="tool-button" onClick={() => openPane('passthrough')}>
                  <ExportOutlined />
                </button>
              </Tooltip>
              <Tooltip title="打开配置" mouseEnterDelay={4}>
                <button className="tool-button" onClick={() => openPane('config')}>
                  <SettingOutlined />
                </button>
              </Tooltip>
              <Tooltip title="关闭容器" mouseEnterDelay={4}>
                <button className="tool-button danger" onClick={handleCloseContainer}>
                  <CloseOutlined />
                </button>
              </Tooltip>
            </nav>
          </header>

          {/* 标签栏：可关闭 */}
          {panes.length > 0 && (
            <div className="pane-tabs">
              {panes.map((p) => (
                <div
                  key={p.id}
                  className={`pane-tab ${activeId === p.id ? 'active' : ''}`}
                  onClick={() => setActivePaneId(p.id)}
                  title={p.title}
                >
                  <span className="pane-tab-title">{p.title}</span>
                  <CloseOutlined
                    className="pane-tab-close"
                    onClick={(e) => {
                      e.stopPropagation();
                      closePane(p.id);
                    }}
                  />
                </div>
              ))}
            </div>
          )}

          {/* 面板内容：常驻渲染 + display 切换（关闭面板才卸载） */}
          <section className="main-panel">
            {panes.map((p) => (
              <div
                key={p.id}
                className="tab-pane"
                style={{
                  display: activeId === p.id ? undefined : 'none',
                  height: '100%',
                  padding: p.kind === 'terminal' ? 0 : undefined,
                  overflow: p.kind === 'terminal' ? 'hidden' : undefined,
                }}
              >
                {p.kind === 'terminal' && <Terminal asRoot={p.asRoot ?? false} />}
                {p.kind === 'passthrough' && <PassthroughManager />}
                {p.kind === 'config' && <ConfigManager containerName={containerName} />}
                {p.kind === 'editor' && p.path && (
                  <FileEditor
                    path={p.path}
                    onClose={() => closePane(p.id)}
                    onSaved={() => closePane(p.id)}
                  />
                )}
              </div>
            ))}
            {panes.length === 0 && (
              <div className="pane-empty">
                从工具栏打开面板（终端 / Passthrough / 配置）
              </div>
            )}
          </section>
        </div>
      </div>
    </div>
  );
}
