// 配置管理器纯函数模块：归一化 / env 校验 / 系统注入常量。
// 不依赖 React——供 ConfigManager 主组件与各面板组件共用。

import type { ContainerConfig, ContainerConfigView } from '../../types';

/** 归一化：entry 转空字符串、深拷贝可变字段，便于受控编辑与比较 */
export function normalizeConfig(cfg: ContainerConfig): ContainerConfig {
  return {
    ...cfg,
    entry: cfg.entry ?? '',
    entry_args: [...(cfg.entry_args ?? [])],
    mounts: cfg.mounts.map((m) => ({ ...m })),
    network: {
      mode: cfg.network.mode,
      ports: cfg.network.ports.map((p) => ({ ...p })),
    },
    env: [...(cfg.env ?? [])],
    uidmaps: (cfg.uidmaps ?? []).map((m) => ({ ...m })),
    gidmaps: (cfg.gidmaps ?? []).map((m) => ({ ...m })),
  };
}

/** 归一化 effective（inspect 投影）——与 ContainerConfig 形状不同，独立处理 */
export function normalizeView(v: ContainerConfigView): ContainerConfigView {
  return {
    ...v,
    mounts: v.mounts.map((m) => ({ ...m })),
    network: {
      mode: v.network.mode,
      ports: v.network.ports.map((p) => ({ ...p })),
    },
    env: [...(v.env ?? [])],
  };
}

/** podman 自动注入的环境变量（EnvPane 生效环境来源标注用） */
export const PODMAN_DEFAULT_ENV_KEYS = new Set([
  'PATH',
  'HOSTNAME',
  'TERM',
  'HOME',
  'container',
]);

/** 引擎保留注入的前缀（EASYTIDY_USER_*，create 时后写覆盖，用户不可编辑） */
export const EASYTIDY_SYSTEM_ENV_PREFIX = 'EASYTIDY_USER_';

/** 校验单个 env 条目（返回错误信息，空串 = 通过） */
export function validateEnv(key: string, existing: string[]): string {
  if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(key)) {
    return '环境变量名需以字母或下划线开头，仅含字母/数字/下划线';
  }
  if (key.startsWith(EASYTIDY_SYSTEM_ENV_PREFIX)) {
    return `环境变量名不能使用 ${EASYTIDY_SYSTEM_ENV_PREFIX} 前缀（引擎保留注入，手动修改无效）`;
  }
  if (existing.some((kv) => kv.slice(0, kv.indexOf('=')) === key)) {
    return `环境变量 ${key} 已存在`;
  }
  return '';
}

/** 解析 "K=V"（value 可含 =） */
export function parseEnv(kv: string): { key: string; value: string } {
  const i = kv.indexOf('=');
  return { key: i > 0 ? kv.slice(0, i) : kv, value: i > 0 ? kv.slice(i + 1) : '' };
}
