// 收藏（pin 到工具栏）状态（zustand）。
//
// 数据源：passthrough_state.pinned（PassthroughManager 加载后同步）；
// PerContainer 工具栏订阅展示——pin/unpin 在两处即时反映，不做模块级全局变量。

import { create } from 'zustand';
import type { PinnedApp } from '../types';

interface FavoritesStore {
  pinned: PinnedApp[];
  /** 整体替换（passthrough_state 加载后） */
  setPinned: (apps: PinnedApp[]) => void;
  /** 单个收藏（pin） */
  upsertPinned: (app: PinnedApp) => void;
  /** 取消收藏（unpin） */
  removePinned: (id: string) => void;
}

export const useFavoritesStore = create<FavoritesStore>((set) => ({
  pinned: [],
  setPinned: (apps) => set({ pinned: apps }),
  upsertPinned: (app) =>
    set((s) => {
      const idx = s.pinned.findIndex((a) => a.id === app.id);
      if (idx >= 0) {
        const next = [...s.pinned];
        next[idx] = app;
        return { pinned: next };
      }
      return { pinned: [...s.pinned, app] };
    }),
  removePinned: (id) =>
    set((s) => ({ pinned: s.pinned.filter((a) => a.id !== id) })),
}));
