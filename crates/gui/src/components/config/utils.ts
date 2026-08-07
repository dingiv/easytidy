// 配置管理器纯函数模块：归一化 / 相等比较 / env 校验 / 系统注入常量。
// 不依赖 React——供 ConfigManager 主组件与各面板组件共用。

import type {
  ContainerConfig,
  ContainerConfigView,
  MountConfig,
} from '../../types';

/** 归一化：entry 转空字符串、深拷贝可变字段，便于受控编辑与比较 */
export function normalizeConfig(cfg: ContainerConfig): ContainerConfig {
  return {
    ...cfg,
    entry: cfg.entry ?? '',
    mounts: cfg.mounts.map((m) => ({ ...m })),
    network: {
      mode: cfg.network.mode,
      ports: cfg.network.ports.map((p) => ({ ...p })),
    },
    env: [...(cfg.env ?? [])],
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

export function mountsEqual(a: MountConfig[], b: MountConfig[]): boolean {
  if (a.length !== b.length) return false;
  return a.every((m, i) => {
    const n = b[i];
    return (
      m.host_path === n.host_path &&
      m.container_path === n.container_path &&
      m.read_only === n.read_only
    );
  });
}

export function networksEqual(
  a: ContainerConfig['network'],
  b: ContainerConfig['network'],
): boolean {
  if (a.mode !== b.mode) return false;
  if (a.ports.length !== b.ports.length) return false;
  return a.ports.every((p, i) => {
    const q = b.ports[i];
    return (
      p.host_port === q.host_port &&
      p.container_port === q.container_port &&
      p.protocol === q.protocol
    );
  });
}

/** env 列表按 KEY→VALUE 映射比较（顺序无关——podman 回显顺序不保证） */
export function envsEqual(a: string[], b: string[]): boolean {
  const map = (xs: string[]) => {
    const m = new Map<string, string>();
    for (const kv of xs) {
      const i = kv.indexOf('=');
      if (i > 0) m.set(kv.slice(0, i), kv.slice(i + 1));
    }
    return m;
  };
  const ma = map(a);
  const mb = map(b);
  if (ma.size !== mb.size) return false;
  for (const [k, v] of ma) {
    if (mb.get(k) !== v) return false;
  }
  return true;
}

/** podman 自动补的环境变量（非用户可配） */
export const PODMAN_DEFAULT_ENV_KEYS = new Set(['PATH', 'HOSTNAME', 'TERM', 'HOME']);

/** 引擎保留注入的前缀（EASYTIDY_USER_*，create 时后写覆盖，用户不可编辑） */
export const EASYTIDY_SYSTEM_ENV_PREFIX = 'EASYTIDY_USER_';

/** 是否为系统注入 env（podman 默认 / easytidy 引擎注入） */
export function isSystemEnv(kv: string): boolean {
  const i = kv.indexOf('=');
  const key = i > 0 ? kv.slice(0, i) : kv;
  return PODMAN_DEFAULT_ENV_KEYS.has(key) || key.startsWith(EASYTIDY_SYSTEM_ENV_PREFIX);
}

/**
 * saved 与 effective 的 env 比较：两侧过滤系统注入后子集比较。
 * 若直接比较，EASYTIDY_USER_* 与 PATH/HOSTNAME 恒不等 → pendingRestart 恒真
 * （2026-08-07 XDG_DATA_DIRS 排障中实测）。
 */
export function envRestartEqual(savedEnv: string[], effectiveEnv: string[]): boolean {
  return envsEqual(
    savedEnv.filter((kv) => !isSystemEnv(kv)),
    effectiveEnv.filter((kv) => !isSystemEnv(kv)),
  );
}

export function configsEqual(a: ContainerConfig, b: ContainerConfig): boolean {
  return (
    a.entry === b.entry &&
    a.silent_boot === b.silent_boot &&
    a.persistent === b.persistent &&
    mountsEqual(a.mounts, b.mounts) &&
    networksEqual(a.network, b.network) &&
    envsEqual(a.env, b.env) &&
    a.user_home === b.user_home
  );
}

/** 引擎内部挂载（create 时自动注入，非用户可配）：server 二进制 + socket 目录 */
const ENGINE_MOUNT_PATHS = ['/usr/bin/easytidy-server', '/run/easytidy'];

/** 过滤引擎内部挂载（effective 的 mounts 含它们，与 saved 比较需剔除——
 * 否则 pendingRestart 恒真，未修改也提示"配置已修改"，实测） */
function userMounts(mounts: MountConfig[]): MountConfig[] {
  return mounts.filter(
    (m) =>
      !ENGINE_MOUNT_PATHS.some(
        (p) => m.container_path === p || m.container_path.startsWith(`${p}/`),
      ),
  );
}

/**
 * saved（configfile 期望）与 effective（inspect 实际）是否一致。
 * view 形状与 ContainerConfig 不同（无 entry/silent_boot/persistent），
 * 不能复用 configsEqual；mounts 侧过滤引擎内部挂载；user_home 侧检验
 * userns_mode（keep-id 下 podman 可能回显 "private"/None，此时只按 saved 判定）。
 */
export function viewMatchesSaved(saved: ContainerConfig, view: ContainerConfigView): boolean {
  if (!mountsEqual(saved.mounts, userMounts(view.mounts))) return false;
  if (!networksEqual(saved.network, view.network)) return false;
  if (!envRestartEqual(saved.env, view.env)) return false;
  if (saved.user_home && view.userns_mode !== null && view.userns_mode !== 'keep-id') {
    return false;
  }
  return true;
}

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
