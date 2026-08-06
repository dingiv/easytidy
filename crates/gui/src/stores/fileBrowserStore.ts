// 文件浏览器导航状态（zustand）
//
// 单容器 GUI 内的文件浏览路径为共享状态，供 FileBrowser 等组件使用；
// 多实例 GUI 之间仍以配置文件互通（见 docs/08 需求 Q5），zustand 仅管理进程内状态。

import { create } from 'zustand';

interface FileBrowserState {
  /** 当前浏览路径 */
  currentPath: string;
  /** 导航到指定路径 */
  navigate: (path: string) => void;
  /** 回到父目录 */
  navigateToParent: () => void;
}

function parentPath(path: string): string {
  if (path === '/' || path === '') return '/';
  const parts = path.split('/').filter(Boolean);
  parts.pop();
  return parts.length ? `/${parts.join('/')}` : '/';
}

export const useFileBrowserStore = create<FileBrowserState>((set, get) => ({
  currentPath: '/',
  navigate: (path) => set({ currentPath: path }),
  navigateToParent: () => set({ currentPath: parentPath(get().currentPath) }),
}));
