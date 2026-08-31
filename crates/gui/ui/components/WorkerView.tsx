// Worker GUI 窗口：单容器浏览器式多面板（open + close + multi-panel）。
//
// - 头部工具栏：方形图标按钮打开面板——Terminal（下拉选身份 用户/root）、
//   Passthrough、Config；Close Container（圆角方形图标，唯一保留的容器操作）
// - 标签栏：每个打开的面板一个标签（标题 + 小叉关闭）；重复打开同类面板聚焦已有
// - 内容区：所有打开的面板常驻渲染（display 切换）——终端会话不因切换丢失；
//   关闭面板才卸载（用户终端：server attach 保持，重开回放；root 终端：
//   宿主 root 通道共享会话，重开回放——关面板 = detach 不杀会话，
//   root_terminal_close 才真正关闭 root shell）

import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { errMsg } from '../lib/errors';
import { App as AntApp, Dropdown, Tooltip } from 'antd';
import {
  CloseOutlined,
  CodeOutlined,
  DownOutlined,
  ExportOutlined,
  MenuFoldOutlined,
  MenuUnfoldOutlined,
  SettingOutlined,
} from '@ant-design/icons';
import { useFileBrowserStore } from '../stores/fileBrowserStore';
import { useFavoritesStore } from '../stores/favoritesStore';
import { useTerminalStore } from '../stores/terminalStore';
import { useUiStore } from '../stores/uiStore';
import type { PassthroughState, PinnedApp, TerminalInfo } from '../types';
import logo from '../assets/logo.png';
import { PinnedAppIcon } from './PinnedAppIcon';
import { Terminal } from './Terminal';
import { RootTerminal } from './RootTerminal';
import { FileBrowser } from './FileBrowser';
import { FileEditor } from './FileEditor';
import { ImageViewer } from './ImageViewer';
import { PassthroughManager } from './PassthroughManager';
import { ConfigManager } from './ConfigManager';

interface WorkerViewProps {
  containerName: string;
}

/** 打开的面板 */
interface Pane {
  id: string;
  kind: 'terminal' | 'root' | 'passthrough' | 'config' | 'editor' | 'image';
  title: string;
  /** 用户终端会话 stream_id（null = 尚未建立/新开；attach 重连用）。
   *  root 终端单例共享会话，无 stream_id（流 ID 恒 ROOT_STREAM_ID） */
  streamId?: number | null;
  /** 编辑器/图片面板：文件路径 */
  path?: string;
}

const AUTO_COLLOPSE_WIDTH = 150

/** 用户终端标签：显示命令（默认登录 shell 显示「终端」） */
function terminalTitle(t: TerminalInfo): string {
  const cmd = t.cmd || '终端';
  return cmd.length > 24 ? `${cmd.slice(0, 24)}…` : cmd;
}

/** root 面板 id（单例：每容器一个共享 root 会话） */
const ROOT_PANE_ID = 'root-terminal';

function WorkerViewInner({ containerName }: WorkerViewProps) {
  const { message } = AntApp.useApp();
  // 收藏（pin 到工具栏）：PassthroughManager 经 store 同步；此处订阅展示
  const pinnedApps = useFavoritesStore((s) => s.pinned);
  // 会话同步状态：GUI 打开容器 → 先与 server 握手连接并同步必要数据
  //（终端列表 + passthrough 收藏），再决定初始面板——避免"默认面板先建、
  // 恢复列表后到"的重复建终端竞态
  const [panes, setPanes] = useState<Pane[]>([]);
  const [sessionReady, setSessionReady] = useState(false);
  const [activePaneId, setActivePaneId] = useState<string | null>(null);
  // 初始激活第一个 pane（惰性：首个渲染后设置）
  const activeId = activePaneId ?? panes[0]?.id ?? null;

  // 打开容器的会话初始化（握手 + 数据同步）：
  // 1. get_terminals 触发共享 socket 连接（hello 握手）→ 活跃用户终端
  //    列表，各恢复一个面板（附接重连，回放当前屏幕）
  // 2. passthrough_state 同步收藏（工具栏不依赖面板打开）
  // 3. root_terminal_status 探测共享 root 会话——存活则恢复 root 面板
  //    （attach 即回放；root 通道与 server socket 相互独立，探测失败
  //    仅表示未运行，不阻塞打开）
  // 4. 无活跃用户终端 → 默认单个用户终端（新建持久会话）
  useEffect(() => {
    (async () => {
      const [terminalsRes, stateRes, rootRes] = await Promise.allSettled([
        invoke<TerminalInfo[]>('get_terminals'),
        invoke<PassthroughState>('passthrough_state'),
        invoke<boolean>('root_terminal_status'),
      ]);
      const restored: Pane[] = [];
      if (terminalsRes.status === 'fulfilled') {
        restored.push(
          ...terminalsRes.value.map((t) => ({
            id: useUiStore.getState().nextPaneId(),
            kind: 'terminal' as const,
            title: terminalTitle(t),
            streamId: t.stream_id,
          })),
        );
      } else {
        console.error('get_terminals failed（回退默认终端）:', terminalsRes.reason);
      }
      if (rootRes.status === 'fulfilled' && rootRes.value) {
        restored.push({
          id: ROOT_PANE_ID,
          kind: 'root',
          title: '终端 (root)',
        });
      }
      if (restored.length > 0) {
        setPanes(restored);
      }
      if (stateRes.status === 'fulfilled') {
        useFavoritesStore.getState().setPinned(stateRes.value.pinned ?? []);
      } else {
        console.error('passthrough_state 同步失败:', stateRes.reason);
      }
      // 无活跃用户终端：默认打开一个用户终端（root 面板恢复与否不影响）
      const userTerminalCount = terminalsRes.status === 'fulfilled' ? terminalsRes.value.length : 0;
      if (userTerminalCount === 0) {
        setPanes((prev) =>
          prev.some((p) => p.kind === 'terminal')
            ? prev
            : [
                ...prev,
                {
                  id: useUiStore.getState().nextPaneId(),
                  kind: 'terminal' as const,
                  title: '终端',
                  streamId: null,
                },
              ],
        );
      }
      setActivePaneId(null); // 激活第一个
      setSessionReady(true);
    })();
  }, []);

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

  // 文件浏览器"跟随终端"：server TTY 事件驱动（pty.cwdChanged 主动推送，
  // 输入回车时毫秒级检测）→ store cwd 更新 → 订阅导航；开启时初始同步一次
  const [followTerminal, setFollowTerminal] = useState(false);
  const followRef = useRef({ activeId, panes });
  followRef.current = { activeId, panes };
  useEffect(() => {
    if (!followTerminal) return;
    // 初始同步：开启瞬间查一次激活终端的 pwd
    const syncOnce = async () => {
      const { activeId: aid, panes: ps } = followRef.current;
      const activePane = ps.find((p) => p.id === aid);
      if (!activePane || activePane.kind !== 'terminal') return;
      const sid = activePane.streamId ?? null;
      if (sid === null) return;
      try {
        const cwd = await invoke<string>('pty_cwd', { streamId: sid });
        if (cwd) useFileBrowserStore.getState().navigate(cwd);
      } catch (err) {
        console.error('pty_cwd (sync) failed:', err);
      }
    };
    syncOnce();
    // 订阅 cwd 变化（server 推送更新 store；按 stream_id 取激活终端）
    const unsub = useTerminalStore.subscribe((state) => {
      const { activeId: aid, panes: ps } = followRef.current;
      const activePane = ps.find((p) => p.id === aid);
      if (!activePane || activePane.kind !== 'terminal') return;
      const sid = activePane.streamId ?? null;
      if (sid === null) return;
      const cwd = state.cwdByStream[sid];
      if (cwd && cwd !== useFileBrowserStore.getState().currentPath) {
        useFileBrowserStore.getState().navigate(cwd);
      }
    });
    return unsub;
  }, [followTerminal]);

  const handleCloseContainer = async () => {
    try {
      await invoke('container_shutdown');
    } catch (err) {
      console.error('container_shutdown failed:', err);
    }
  };

  /** 点击收藏图标 → 拉起应用（server apps.launch，server 保活） */
  const launchPinned = async (p: PinnedApp) => {
    try {
      const pid = await invoke<number>('passthrough_launch', { id: p.id });
      message.success(`${p.name} 已启动 (pid=${pid})`);
    } catch (err: any) {
      message.error(errMsg(err, `启动 ${p.name} 失败`));
      console.error('passthrough_launch failed:', err);
    }
  };

  /** 悬停 ✕ → 取消收藏 */
  const unpinPinned = async (p: PinnedApp) => {
    try {
      await invoke('passthrough_set_pinned', {
        id: p.id,
        name: p.name,
        cmd: p.cmd,
        iconPath: p.icon ?? null,
        pinned: false,
      });
      useFavoritesStore.getState().removePinned(p.id);
    } catch (err: any) {
      message.error(errMsg(err, '取消收藏失败'));
      console.error('passthrough_set_pinned (unpin) failed:', err);
    }
  };

  /** 打开新用户终端（多实例：每次点击新建一个独立持久会话面板，可同时多开） */
  const openTerminal = () => {
    const pane: Pane = {
      id: useUiStore.getState().nextPaneId(),
      kind: 'terminal',
      title: '终端',
      streamId: null, // 新建持久会话，Terminal 建立后经 onStream 回填
    };
    setPanes((prev) => [...prev, pane]);
    setActivePaneId(pane.id);
  };

  /** 打开 root 终端（单例：每容器一个共享 root 会话；已开则聚焦） */
  const openRootTerminal = () => {
    setPanes((prev) => {
      const existing = prev.find((p) => p.kind === 'root');
      if (existing) {
        setActivePaneId(existing.id);
        return prev;
      }
      const pane: Pane = { id: ROOT_PANE_ID, kind: 'root', title: '终端 (root)' };
      setActivePaneId(pane.id);
      return [...prev, pane];
    });
  };

  /** 打开面板（非终端类：同类已存在则聚焦，否则新建） */
  const openPane = (kind: Pane['kind']) => {
    setPanes((prev) => {
      const existing = prev.find((p) => p.kind === kind);
      if (existing) {
        setActivePaneId(existing.id);
        return prev;
      }
      const title = kind === 'passthrough' ? 'Passthrough' : '容器配置';
      const pane: Pane = {
        id: useUiStore.getState().nextPaneId(),
        kind,
        title,
      };
      setActivePaneId(pane.id);
      return [...prev, pane];
    });
  };

  /** 终端会话建立后回填面板（新开路径拿到 stream_id） */
  const bindTerminalStream = (paneId: string, sid: number) => {
    setPanes((prev) =>
      prev.map((p) => (p.id === paneId ? { ...p, streamId: sid } : p)),
    );
  };

  /** 关闭面板：关闭后激活相邻面板。
   *  用户终端 = pty.close 终结会话（生命周期由 server 持有）；
   *  root 终端 = root_terminal_close（kill 容器内 root shell + root 通道退出）
   *  ——root 会话是共享的，关面板即关会话（与用户终端一致的产品语义） */
  const closePane = (id: string) => {
    const pane = panes.find((p) => p.id === id);
    if (pane && pane.kind === 'terminal' && pane.streamId != null) {
      const sid = pane.streamId;
      invoke('pty_close', { streamId: sid }).catch((err) =>
        console.error('pty_close failed:', err),
      );
      useTerminalStore.getState().clearCwd(sid);
    } else if (pane && pane.kind === 'root') {
      invoke('root_terminal_close').catch((err) =>
        console.error('root_terminal_close failed:', err),
      );
    }
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
    openFilePane('editor', path);
  };

  /** 打开图片预览面板（右键预览）：同文件已开则聚焦，否则新建 */
  const openImage = (path: string) => {
    openFilePane('image', path);
  };

  /** 打开文件类面板（editor/image）的通用逻辑 */
  const openFilePane = (kind: 'editor' | 'image', path: string) => {
    setPanes((prev) => {
      const existing = prev.find((p) => p.kind === kind && p.path === path);
      if (existing) {
        setActivePaneId(existing.id);
        return prev;
      }
      const name = path.split('/').filter(Boolean).pop() ?? path;
      const pane: Pane = {
        id: useUiStore.getState().nextPaneId(),
        kind,
        title: name,
        path,
      };
      setActivePaneId(pane.id);
      return [...prev, pane];
    });
  };

  return (
    <div className="worker-view app">
      {/* 左右布局：左 = 标题 + 文件浏览器；右 = 工具栏 + 标签栏 + 面板 */}
      <div className="per-layout">
        {/* 左列：标题 + 文件浏览器；右边界可拖拽调宽（折叠按钮在工具栏第一个） */}
        <aside
          className={`per-left ${sidebarCollapsed ? 'collapsed' : ''}`}
          style={{ width: sidebarCollapsed ? 0 : sidebarWidth }}
        >
          {!sidebarCollapsed && (
            <>
              <div className="menu-title"><img src={logo} className="app-logo" alt="easytidy" />easytidy - {containerName}</div>
              <div className="per-left-browser">
                {/* 双击文本文件 → 在右侧面板打开编辑器；跟随终端开关 */}
                <FileBrowser
                  onOpenFile={openEditor}
                  onPreviewImage={openImage}
                  followTerminal={followTerminal}
                  onToggleFollow={() => setFollowTerminal((f) => !f)}
                />
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
                    { key: 'user', label: '新建终端 (用户)' },
                    { key: 'root', label: '打开 root 终端（共享会话）' },
                  ],
                  onClick: ({ key }) => (key === 'root' ? openRootTerminal() : openTerminal()),
                }}
                placement="bottomLeft"
              >
                <Tooltip title="新建终端（多实例，用户/root）" mouseEnterDelay={4}>
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

              <hr />

              {/* 收藏栏：pin 到工具栏的 passthrough 应用；点击拉起，悬停 ✕ 取消收藏 */}
              {pinnedApps.length > 0 && (
                <div className="favorites-bar">
                  {pinnedApps.map((p) => (
                    <Tooltip key={p.id} title={p.name} mouseEnterDelay={4}>
                      <div className="favorite-item" onClick={() => launchPinned(p)}>
                        <PinnedAppIcon id={p.id} icon={p.icon} size={22} />
                        <CloseOutlined
                          className="favorite-unpin"
                          onClick={(e) => {
                            e.stopPropagation();
                            unpinPinned(p);
                          }}
                        />
                      </div>
                    </Tooltip>
                  ))}
                </div>
              )}

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
                  padding: p.kind === 'terminal' ? '1ch' : undefined,
                  overflow: p.kind === 'terminal' ? 'hidden' : undefined,
                }}
              >
                {p.kind === 'terminal' && (
                  <Terminal
                    streamId={p.streamId ?? null}
                    onStream={(sid) => bindTerminalStream(p.id, sid)}
                    onExit={() => closePane(p.id)}
                  />
                )}
                {p.kind === 'root' && <RootTerminal onExited={() => closePane(p.id)} />}
                {p.kind === 'passthrough' && <PassthroughManager />}
                {p.kind === 'config' && <ConfigManager containerName={containerName} />}
                {p.kind === 'editor' && p.path && (
                  <FileEditor
                    path={p.path}
                    onClose={() => closePane(p.id)}
                    onSaved={() => closePane(p.id)}
                  />
                )}
                {p.kind === 'image' && p.path && <ImageViewer path={p.path} />}
              </div>
            ))}
            {panes.length === 0 && (
              <div className="pane-empty">
                {sessionReady
                  ? '从工具栏打开面板（终端 / Passthrough / 容器配置）'
                  : '正在连接容器 server 并同步会话…'}
              </div>
            )}
          </section>
        </div>
      </div>
    </div>
  );
}

/** antd App 包裹：让子组件（FileBrowser 等）的 message/notification 可用 */
export function WorkerView(props: WorkerViewProps) {
  return (
    <AntApp>
      <WorkerViewInner {...props} />
    </AntApp>
  );
}
