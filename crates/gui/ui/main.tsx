import React from 'react';
import ReactDOM from 'react-dom/client';
import { ConfigProvider, theme } from 'antd';
import zhCN from 'antd/locale/zh_CN';
import App from './App';
import { THEMES, useThemeStore, themeModeOf } from './stores/themeStore';
import './styles.scss';

// 把当前主题映射到 antd algorithm + primary 色 token（主色取自 CSS 变量）。
// antd v6 token 默认值需 Color string，所以从 :root CSS 变量读 primary：
const cssVar = (name: string) =>
  getComputedStyle(document.documentElement).getPropertyValue(name).trim();

function Root() {
  const themeId = useThemeStore((s) => s.theme);
  const mode = themeModeOf(themeId);
  return (
    <ConfigProvider
      locale={zhCN}
      theme={{
        algorithm: mode === 'dark' ? theme.darkAlgorithm : theme.defaultAlgorithm,
        token: {
          // 主色 / 背景从 CSS 变量同步（运行时随主题切换即时更新）
          colorPrimary: cssVar('--accent-primary') || undefined,
          colorBgBase: cssVar('--bg-base') || undefined,
          colorBgContainer: cssVar('--bg-surface') || undefined,
          colorBgElevated: cssVar('--bg-surface') || undefined,
          colorBgLayout: cssVar('--bg-mantle') || undefined,
          colorText: cssVar('--fg-default') || undefined,
          colorBorder: cssVar('--border-default') || undefined,
          colorError: cssVar('--accent-danger') || undefined,
          colorWarning: cssVar('--accent-warning') || undefined,
          colorSuccess: cssVar('--accent-success') || undefined,
          colorInfo: cssVar('--accent-info') || undefined,
          borderRadius: 6,
        },
      }}
    >
      <App />
    </ConfigProvider>
  );
}

// React DevTools 调试时可读 window.__easytidyThemes__（避免 unused warning）
declare global {
  interface Window {
    __easytidyThemes__?: typeof THEMES;
  }
}
window.__easytidyThemes__ = THEMES;

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <Root />
  </React.StrictMode>,
);