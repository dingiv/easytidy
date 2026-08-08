// 通用 UI 状态（zustand）。
//
// paneSeq 原为 PerContainer.tsx 模块级 `let paneSeq = 0` 全局计数器——
// 按用户要求迁移到 zustand 管理（面板 id 全局唯一，多容器窗口不冲突）。

import { create } from 'zustand';

interface UiStore {
  /** 面板 id 计数器 */
  paneSeq: number;
  /** 生成下一个全局唯一面板 id */
  nextPaneId: () => string;
}

export const useUiStore = create<UiStore>((set, get) => ({
  paneSeq: 0,
  nextPaneId: () => {
    const seq = get().paneSeq + 1;
    set({ paneSeq: seq });
    return `pane-${seq}`;
  },
}));
