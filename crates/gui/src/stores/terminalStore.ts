// 终端 PTY 流缓存（zustand）。
//
// 原为 Terminal.tsx 模块级 streamCache 全局变量（跨组件实例共享 PTY 流：
// 组件 remount 复用、不重新 pty_open）——按用户要求迁移到 zustand 管理。
// 按身份分键（user=node 常规 / root=root 终端），各自独立常驻会话。
// 缓存是"读一次"语义（组件挂载时决定复用还是新建），故用 getState 同步读，
// 不订阅响应式更新。

import { create } from 'zustand';
import type { Channel } from '@tauri-apps/api/core';
import type { PtyEvent } from '../types';

interface TerminalStream {
  streamId: number | null;
  channel: Channel<PtyEvent> | null;
  /** 最近 cwd（server TTY 事件驱动推送 pty.cwdChanged 更新） */
  cwd: string | null;
}

interface TerminalStore {
  /** 按身份分键的 PTY 流缓存 */
  streams: { user: TerminalStream; root: TerminalStream };
  /** 取指定身份的缓存（不存在/已失效返回空对象引用不变） */
  getStream: (asRoot: boolean) => TerminalStream;
  /** 写入缓存（新建流成功后） */
  setStream: (asRoot: boolean, streamId: number, channel: Channel<PtyEvent>) => void;
  /** 失效缓存（会话退出后，下次挂载新建） */
  clearStream: (asRoot: boolean) => void;
  /** 更新 cwd（server 主动推送） */
  setCwd: (asRoot: boolean, cwd: string) => void;
}

export const useTerminalStore = create<TerminalStore>((set, get) => ({
  streams: {
    user: { streamId: null, channel: null, cwd: null },
    root: { streamId: null, channel: null, cwd: null },
  },
  getStream: (asRoot) => get().streams[asRoot ? 'root' : 'user'],
  setStream: (asRoot, streamId, channel) =>
    set((s) => ({
      streams: {
        ...s.streams,
        [asRoot ? 'root' : 'user']: { streamId, channel, cwd: s.streams[asRoot ? 'root' : 'user'].cwd },
      },
    })),
  clearStream: (asRoot) =>
    set((s) => ({
      streams: {
        ...s.streams,
        [asRoot ? 'root' : 'user']: { streamId: null, channel: null, cwd: null },
      },
    })),
  setCwd: (asRoot, cwd) =>
    set((s) => ({
      streams: {
        ...s.streams,
        [asRoot ? 'root' : 'user']: { ...s.streams[asRoot ? 'root' : 'user'], cwd },
      },
    })),
}));
