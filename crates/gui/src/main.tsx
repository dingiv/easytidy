import React from 'react';
import ReactDOM from 'react-dom/client';
import { ConfigProvider, theme } from 'antd';
import zhCN from 'antd/locale/zh_CN';
import App from './App';
import './styles.css';

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    {/* antd 暗色主题 + 中文 locale，与应用深色风格一致 */}
    <ConfigProvider
      locale={zhCN}
      theme={{
        algorithm: theme.darkAlgorithm,
        token: {
          colorPrimary: '#89b4fa',
          colorBgBase: '#1e1e2e',
          colorBgContainer: '#181825',
          borderRadius: 6,
        },
      }}
    >
      <App />
    </ConfigProvider>
  </React.StrictMode>,
);
