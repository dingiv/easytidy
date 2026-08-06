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

  return (
    <div className="per-container">
      {/* Menu bar */}
      <header className="menu-bar">
        <div className="menu-title">easytidy - {containerName}</div>
        <nav className="menu-items">
          <button className="menu-item" onClick={() => window.location.reload()}>
            Back to Centralized
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
          <div className="panel-header">File Browser</div>
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
            {activeTab === 'terminal' && (
              <div className="tab-pane terminal-pane">
                <Terminal />
              </div>
            )}
            {activeTab === 'passthrough' && (
              <div className="tab-pane">
                <PassthroughManager />
              </div>
            )}
            {activeTab === 'config' && (
              <div className="tab-pane">
                <ConfigManager />
              </div>
            )}
          </div>
        </section>
      </main>
    </div>
  );
}
