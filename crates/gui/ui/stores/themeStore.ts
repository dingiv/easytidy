// 主题 store（zustand）。
//
// 只持有「当前主题 id」+「切换函数」。具体色板/角色 token 由 SCSS
// `[data-theme="<id>"]` 选择器输出 CSS 变量；切换主题 = 改
// `document.documentElement.dataset.theme` + 同步 antd theme algorithm。
//
// 持久化：localStorage 'easytidy.theme'（用户上次选择）。

import { create } from 'zustand';

export type ThemeId = 'mocha' | 'latte' | 'tokyo-night' | 'nord';

export interface ThemeMeta {
  id: ThemeId;
  /** 显示名（中英） */
  label: string;
  /** antd algorithm：'dark' / 'light' —— 来自调色板 base 亮度 */
  mode: 'dark' | 'light';
}

/** 注册顺序也是 Settings UI 渲染顺序。色板来自 ui/themes/_palette-*.scss。 */
export const THEMES: readonly ThemeMeta[] = [
  { id: 'mocha', label: 'Catppuccin Mocha', mode: 'dark' },
  { id: 'latte', label: 'Catppuccin Latte', mode: 'light' },
  { id: 'tokyo-night', label: 'Tokyo Night', mode: 'dark' },
  { id: 'nord', label: 'Nord', mode: 'dark' },
] as const;

const STORAGE_KEY = 'easytidy.theme';
const DEFAULT_THEME: ThemeId = 'mocha';

interface ThemeState {
  /** 当前主题 id（store + document.documentElement.dataset.theme 同步） */
  theme: ThemeId;
  /** 切换主题：更新 store + DOM + localStorage */
  setTheme: (id: ThemeId) => void;
}

/** 启动时同步 DOM（防闪：必须在首屏 CSS 应用前设置 dataset.theme） */
function readPersistedTheme(): ThemeId {
  if (typeof window === 'undefined') return DEFAULT_THEME;
  const v = window.localStorage.getItem(STORAGE_KEY);
  if (v && (THEMES as readonly ThemeMeta[]).some((t) => t.id === v)) {
    return v as ThemeId;
  }
  return DEFAULT_THEME;
}

function applyToDom(id: ThemeId) {
  if (typeof document === 'undefined') return;
  document.documentElement.dataset.theme = id;
}

export const useThemeStore = create<ThemeState>((set) => {
  const initial = readPersistedTheme();
  // 模块加载即同步 DOM —— 防 CSS 变量为 undefined 时闪一下默认色
  applyToDom(initial);

  return {
    theme: initial,
    setTheme: (id) => {
      applyToDom(id);
      if (typeof window !== 'undefined') {
        window.localStorage.setItem(STORAGE_KEY, id);
      }
      set({ theme: id });
    },
  };
});

/** 取 antd 主题算法（darkAlgorithm / lightAlgorithm）。 */
export function themeModeOf(id: ThemeId): 'dark' | 'light' {
  return THEMES.find((t) => t.id === id)?.mode ?? 'dark';
}