// 终端状态（zustand）：按 stream_id 键的 cwd 缓存。
//
// 多终端实例化后，PTY 流由各 Terminal 组件持有（每条会话一条专用连接），
// store 只保留跨组件共享的最小状态：cwd（server TTY 事件驱动推送
// pty.cwdChanged 更新，文件浏览器"跟随终端"订阅用）。按 stream_id 键——
// 多终端面板各自独立，互不干扰。

import { create } from 'zustand';

interface TerminalStore {
  /** stream_id → 最近 cwd（server pty.cwdChanged 主动推送更新） */
  cwdByStream: Record<number, string>;
  /** 更新 cwd（server 主动推送） */
  setCwd: (streamId: number, cwd: string) => void;
  /** 清理（会话退出/面板关闭） */
  clearCwd: (streamId: number) => void;
}

export const useTerminalStore = create<TerminalStore>((set) => ({
  cwdByStream: {},
  setCwd: (streamId, cwd) =>
    set((s) => ({ cwdByStream: { ...s.cwdByStream, [streamId]: cwd } })),
  clearCwd: (streamId) =>
    set((s) => {
      const next = { ...s.cwdByStream };
      delete next[streamId];
      return { cwdByStream: next };
    }),
}));
