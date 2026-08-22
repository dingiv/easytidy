import { App as AntApp } from 'antd';
import { MasterView } from './components/MasterView';
import { WorkerView } from './components/WorkerView';
import { useAppMode } from './hooks/useAppMode';

function App() {
  const { mode, error } = useAppMode();

  // antd App 上下文：让各面板的 message/modal/notification 生效
  // （无上下文时 App.useApp() 静默不渲染——主 GUI 曾因此吞掉创建错误）
  return (
    <AntApp>
      <AppInner mode={mode} error={error} />
    </AntApp>
  );
}

function AppInner({ mode, error }: { mode: any; error: string | null }) {
  if (error) {
    return (
      <div className="app">
        <div className="error-screen">
          <h2>Application Error</h2>
          <p>{error}</p>
        </div>
      </div>
    );
  }

  if (!mode) {
    return (
      <div className="app">
        <div className="loading-screen">
          <p>Loading easytidy...</p>
        </div>
      </div>
    );
  }

  // Master mode
  if ('Master' in mode) {
    return (
      <div className="app">
        <header className="menu-bar">
          <div className="menu-title">easytidy</div>
          <nav className="menu-items">
            <button className="menu-item">File</button>
            <button className="menu-item">Edit</button>
            <button className="menu-item">View</button>
            <button className="menu-item">Help</button>
          </nav>
        </header>
        <main className="main-content master-main">
          <MasterView />
        </main>
      </div>
    );
  }

  // Worker mode
  if ('Worker' in mode) {
    return <WorkerView containerName={mode.Worker.name} />;
  }

  return (
    <div className="app">
      <div className="error-screen">
        <h2>Unknown Mode</h2>
        <p>Unable to determine application mode</p>
      </div>
    </div>
  );
}

export default App;
