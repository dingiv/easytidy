// 配置管理器状态（zustand）。
//
// 统一管理 saved/effective/edit 三态 + dirty 标志位：
// - **dirty 由编辑动作显式置位**（update 置 true,load 重置 false）——
//   不再用深比较推断"是否有未保存修改"（比较法对引擎注入 env/mounts/
//   userns 回显的过滤有各种漏网,导致未修改也误报,2026-08-08 实测）
// - 保存（apply）即"快速重建容器"——成功后 reload,dirty 重置;
//   不存在"已保存未生效"状态（容器已按新配置重建）

import { create } from 'zustand';
import { invoke } from '@tauri-apps/api/core';
import { errMsg } from '../lib/errors';
import { normalizeConfig, normalizeView } from '../components/config/utils';
import type {
  ContainerConfig,
  ContainerConfigResult,
  ContainerConfigView,
  HostUser,
} from '../types';

interface ConfigState {
  /** configfile 期望配置（上次加载/保存后的快照） */
  saved: ContainerConfig | null;
  /** podman inspect 实际生效 */
  effective: ContainerConfigView | null;
  /** 宿主用户（uid 映射面板数据源） */
  hostUser: HostUser | null;
  /** 本地编辑态 */
  edit: ContainerConfig | null;
  loading: boolean;
  applying: boolean;
  error: string | null;
  /** 标志位：用户是否修改过（update 置 true,load/apply 后 false） */
  dirty: boolean;
  /** 加载容器配置（拉取后重置 dirty） */
  load: (name: string) => Promise<void>;
  /** 编辑动作：应用修改到 edit 并置 dirty（任意字段变更入口） */
  update: (mutator: (edit: ContainerConfig) => ContainerConfig) => void;
  /** 快速重建容器（成功后重载） */
  apply: (name: string) => Promise<void>;
  /** 重置错误 */
  clearError: () => void;
}

export const useConfigStore = create<ConfigState>((set, get) => ({
  saved: null,
  effective: null,
  hostUser: null,
  edit: null,
  loading: true,
  applying: false,
  error: null,
  dirty: false,

  load: async (name) => {
    set({ loading: true, error: null });
    try {
      const result = await invoke<ContainerConfigResult>('get_container_config', { name });
      set({
        saved: normalizeConfig(result.config),
        effective: result.effective ? normalizeView(result.effective) : null,
        hostUser: result.host_user ?? null,
        edit: normalizeConfig(result.config),
        dirty: false,
      });
    } catch (err: any) {
      set({ error: errMsg(err, '加载容器配置失败') });
      console.error('get_container_config failed:', err);
    } finally {
      set({ loading: false });
    }
  },

  update: (mutator) => {
    set((s) => {
      if (!s.edit) return s;
      return { edit: mutator(s.edit), dirty: true };
    });
  },

  apply: async (name) => {
    const { edit } = get();
    if (!edit) return;
    set({ applying: true, error: null });
    try {
      const payload: ContainerConfig = {
        ...edit,
        entry: edit.entry && edit.entry.trim() ? edit.entry.trim() : null,
      };
      const newId = await invoke<string>('apply_container_config', {
        name,
        config: payload,
      });
      console.log(`container recreated, new id: ${newId}`);
      // 快速重建成功：重载（dirty 重置,无"未生效"状态）
      await get().load(name);
    } catch (err: any) {
      set({ error: errMsg(err, '应用配置失败') });
      console.error('apply_container_config failed:', err);
    } finally {
      set({ applying: false });
    }
  },

  clearError: () => set({ error: null }),
}));
