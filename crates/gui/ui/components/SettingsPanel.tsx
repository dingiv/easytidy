// easytidy 应用配置页（Settings）。
//
// 配置「easytidy 本身」的行为，区别于容器/镜像/模板等资源管理 pane。
// 当前板块：
// - 外观（主题）：4 个预设主题色卡，点击即切换（写 themeStore + localStorage，
//   antd algorithm/token 由 main.tsx Root 联动）
//
// 后续板块（预留结构）：默认终端、开机自启、语言等 —— 各自独立 <section>，
// 沿用 `.settings-section` 样式。

import { App as AntApp, Typography } from 'antd';
import { CheckOutlined } from '@ant-design/icons';
import { THEMES, useThemeStore, type ThemeMeta } from '../stores/themeStore';

export function SettingsPanel() {
  return (
    <div className="settings-panel">
      <ThemeSection />
    </div>
  );
}

/** 外观分区：主题色卡网格 */
function ThemeSection() {
  const { message } = AntApp.useApp();
  const theme = useThemeStore((s) => s.theme);
  const setTheme = useThemeStore((s) => s.setTheme);

  const pick = (t: ThemeMeta) => {
    if (t.id === theme) return;
    setTheme(t.id);
    message.success(`主题已切换：${t.label}`);
  };

  return (
    <section className="settings-section">
      <Typography.Title level={5} className="settings-section-title">
        外观
      </Typography.Title>
      <Typography.Text type="secondary" className="settings-section-desc">
        选择界面配色主题。切换立即生效并自动保存；antd 组件（按钮/弹窗等）
        会跟随主题同步明暗与主色。
      </Typography.Text>

      <div className="theme-grid" role="radiogroup" aria-label="主题选择">
        {THEMES.map((t) => (
          <ThemeCard
            key={t.id}
            meta={t}
            active={t.id === theme}
            onClick={() => pick(t)}
          />
        ))}
      </div>
    </section>
  );
}

/** 单张主题色卡：预览块（bg + 主色/辅色/危险色条）+ 名称 + 选中标记 */
function ThemeCard({
  meta,
  active,
  onClick,
}: {
  meta: ThemeMeta;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      role="radio"
      aria-checked={active}
      className={`theme-card ${active ? 'active' : ''}`}
      onClick={onClick}
    >
      {/* 预览：一个迷你 UI 缩略（sidebar + 主区 + 按钮条），色值取当前主题实例 */}
      <span
        className="theme-card-preview"
        data-theme={meta.id}
        aria-hidden
      >
        <span className="tcp-side" />
        <span className="tcp-main">
          <span className="tcp-bar" />
          <span className="tcp-line" />
          <span className="tcp-line short" />
          <span className="tcp-btns">
            <i className="tcp-btn primary" />
            <i className="tcp-btn danger" />
            <i className="tcp-btn accent2" />
          </span>
        </span>
      </span>
      <span className="theme-card-label">
        {meta.label}
        <em className="theme-card-mode">{meta.mode === 'dark' ? '深色' : '浅色'}</em>
      </span>
      {active && (
        <span className="theme-card-check">
          <CheckOutlined />
        </span>
      )}
    </button>
  );
}
