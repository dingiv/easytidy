import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Terminal } from './Terminal';
import { FileBrowser } from './FileBrowser';
import { PassthroughManager } from './PassthroughManager';
import { ConfigManager } from './ConfigManager';

interface PerContainerProps {
  containerName: string;
}

export function PerContainer({ containerName }: PerContainerProps) {
  const [activeTab, setActiveTab] = useState<'terminal' | 'passthrough' | 'config'>('terminal');

  const handleCloseContainer = async () => {
    try {
      await invoke('container_shutdown');
    } catch (err) {
      console.error('container_shutdown failed:', err);
    }
  };

  const [exporting, setExporting] = useState(false);

  /** 导出本容器管理 GUI 的桌面快捷方式（菜单 + 桌面，双击打开此管理界面） */
  const handleExportGuiShortcut = async () => {
    setExporting(true);
    try {
      const path = await invoke<string>('export_gui_shortcut');
      console.log('GUI shortcut exported:', path);
    } catch (err) {
      console.error('export_gui_shortcut failed:', err);
    } finally {
      setExporting(false);
    }
  };

  return (
    <div className="per-container app">
      {/* Menu bar */}
      <header className="menu-bar">
        <div className="menu-title">easytidy - {containerName}</div>
        <nav className="menu-items">
          <button className="menu-item" onClick={() => window.location.reload()}>
            Back to Centralized
          </button>
          <button className="menu-item" onClick={handleExportGuiShortcut} disabled={exporting}>
            {exporting ? '导出中…' : '导出桌面图标'}
          </button>
          <button className="menu-item danger" onClick={handleCloseContainer}>
            Close Container
          </button>
        </nav>
      </header>

      {/* Main content */}
      <main className="main-content">
        {/* Left sidebar: File browser */}
        <aside className="sidebar-left">
          {/* <div className="panel-header">File Browser</div> */}
          <FileBrowser />
        </aside>

        {/* Right panel: Tab panel */}
        <section className="main-panel">
          <div className="tab-nav">
            <button
              className={activeTab === 'terminal' ? 'tab-active' : ''}
              onClick={() => setActiveTab('terminal')}
            >
              Terminal
            </button>
            <button
              className={activeTab === 'passthrough' ? 'tab-active' : ''}
              onClick={() => setActiveTab('passthrough')}
            >
              Passthrough Manager
            </button>
            <button
              className={activeTab === 'config' ? 'tab-active' : ''}
              onClick={() => setActiveTab('config')}
            >
              Config Manager
            </button>
          </div>

          <div className="tab-content">
            {/* 三 tab 常驻渲染、display 切换：终端组件不卸载——
                卸载重建会丢 shell 会话且掩盖连接/焦点问题（2026-08-07 实测） */}
            <div className="tab-pane terminal-pane" style={{ display: activeTab === 'terminal' ? undefined : 'none' }}>
              <Terminal />
            </div>
            <div className="tab-pane" style={{ display: activeTab === 'passthrough' ? undefined : 'none' }}>
              <PassthroughManager />
            </div>
            <div className="tab-pane" style={{ display: activeTab === 'config' ? undefined : 'none' }}>
              <ConfigManager containerName={containerName} />
            </div>
          </div>
        </section>
      </main>
    </div>
  );
}
