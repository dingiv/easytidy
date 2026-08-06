import React, { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';

/// 应用模式：中心化或 per-container
type AppMode = 'centralized' | 'container';

/// 主应用组件
function App() {
  const [mode] = useState<AppMode>('centralized'); // TODO: 从 Tauri 命令行参数获取
  const [activeTab, setActiveTab] = useState<'terminal' | 'passthrough' | 'config'>('terminal');

  return (
    <div className="app">
      {/* 菜单栏 */}
      <header className="menu-bar">
        <div className="menu-title">easytidy</div>
        <nav className="menu-items">
          <button className="menu-item">文件</button>
          <button className="menu-item">编辑</button>
          <button className="menu-item">视图</button>
          <button className="menu-item">帮助</button>
        </nav>
      </header>

      {/* 主内容区 */}
      <main className="main-content">
        {/* 左侧边栏：文件浏览器 */}
        <aside className="sidebar-left">
          <div className="panel-header">文件浏览器</div>
          <div className="file-browser-placeholder">
            {/* TODO: 实现文件浏览器 */}
            <p>文件浏览器占位符</p>
            <p className="hint">容器内文件系统浏览（M3 集成）</p>
          </div>
        </aside>

        {/* 右侧主面板 */}
        <section className="main-panel">
          {mode === 'centralized' ? (
            // 中心化模式：容器列表
            <CentralizedView />
          ) : (
            // Per-container 模式：标签页面板
            <ContainerView activeTab={activeTab} onTabChange={setActiveTab} />
          )}
        </section>
      </main>
    </div>
  );
}

/// 中心化模式视图
function CentralizedView() {
  return (
    <div className="centralized-view">
      <div className="panel-header">容器管理</div>
      <div className="container-list-placeholder">
        {/* TODO: 实现容器列表 */}
        <p>容器列表占位符</p>
        <p className="hint">显示所有容器及其状态（M3 集成）</p>
      </div>
      <button className="primary-button">新建容器</button>
    </div>
  );
}

/// Per-container 模式视图
interface ContainerViewProps {
  activeTab: 'terminal' | 'passthrough' | 'config';
  onTabChange: (tab: 'terminal' | 'passthrough' | 'config') => void;
}

function ContainerView({ activeTab, onTabChange }: ContainerViewProps) {
  return (
    <div className="container-view">
      {/* 标签页导航 */}
      <div className="tab-nav">
        <button
          className={activeTab === 'terminal' ? 'tab-active' : ''}
          onClick={() => onTabChange('terminal')}
        >
          终端
        </button>
        <button
          className={activeTab === 'passthrough' ? 'tab-active' : ''}
          onClick={() => onTabChange('passthrough')}
        >
          passthrough 管理器
        </button>
        <button
          className={activeTab === 'config' ? 'tab-active' : ''}
          onClick={() => onTabChange('config')}
        >
          配置管理器
        </button>
      </div>

      {/* 标签页内容 */}
      <div className="tab-content">
        {activeTab === 'terminal' && (
          <div className="tab-pane">
            <div className="terminal-placeholder">
              {/* TODO: M3 集成 xterm.js */}
              <p>终端占位符</p>
              <p className="hint">PTY 集成（M3 实现）</p>
            </div>
          </div>
        )}
        {activeTab === 'passthrough' && (
          <div className="tab-pane">
            <div className="passthrough-placeholder">
              <p>passthrough 管理器占位符</p>
              <p className="hint">管理容器内应用导出到宿主（M3 实现）</p>
            </div>
          </div>
        )}
        {activeTab === 'config' && (
          <div className="tab-pane">
            <div className="config-placeholder">
              <p>配置管理器占位符</p>
              <p className="hint">容器配置管理（M3 实现）</p>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

export default App;
