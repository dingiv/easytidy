// 文件浏览器导航与选择状态（zustand）
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
  /** 选中的文件/目录路径（点击条目切换；右键菜单"选中"同义） */
  selectedPaths: string[];
  /** 容器内复制标记（右键"复制"设置，右键"粘贴"消费到当前目录） */
  copiedPath: string | null;
  /** 切换选中（点击条目） */
  toggleSelect: (path: string) => void;
  /** 设置为选中（右键"选中"） */
  setSelected: (path: string) => void;
  /** 清除选中 */
  clearSelection: () => void;
  /** 标记复制源（右键"复制"） */
  setCopied: (path: string) => void;
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
  selectedPaths: [],
  copiedPath: null,
  toggleSelect: (path) =>
    set((s) => ({
      selectedPaths: s.selectedPaths.includes(path)
        ? s.selectedPaths.filter((p) => p !== path)
        : [...s.selectedPaths, path],
    })),
  setSelected: (path) => set({ selectedPaths: [path] }),
  clearSelection: () => set({ selectedPaths: [] }),
  setCopied: (path) => set({ copiedPath: path }),
}));
