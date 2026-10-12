// 配置管理器纯函数模块：归一化 / env 校验 / 系统注入常量。
// 不依赖 React——供 ConfigManager 主组件与各面板组件共用。

import type {
  ContainerConfig,
  ContainerConfigView,
  MountConfig,
  VolumeMount,
} from '../../types';

/** easytidy 引擎内部挂载目标（Rust `Podman::{SERVER,DOCK,ETS}_TARGET` + socket
 *  目录的镜像）——「当前生效」里过滤掉，免淹没用户挂载（footer 会报省略数）。 */
export const ENGINE_MOUNT_TARGETS = new Set([
  '/run/easytidy-bin/easytidy-server',
  '/run/easytidy-bin/easytidy-dock',
  '/run/easytidy-bin/ets',
  '/run/easytidy',
]);

/** 「当前生效」与配置的对照状态：same=完全一致 / changed=同位但字段变了 /
 *  extra=配置里没有（改动未应用到容器）——两者都是「重建后才生效」。 */
export type EffectiveTag = 'same' | 'changed' | 'extra';

/** mounts 对照：身份 = container_path（与注入去重同键）。 */
export function mountEffectiveTag(
  eff: MountConfig,
  cfg: MountConfig[],
): EffectiveTag {
  const m = cfg.find((x) => x.container_path === eff.container_path);
  if (!m) return 'extra';
  return m.host_path === eff.host_path && m.read_only === eff.read_only
    ? 'same'
    : 'changed';
}

/** volumes 对照：身份 = 卷名::容器路径（与创建期去重同键）。 */
export function volumeEffectiveTag(
  eff: VolumeMount,
  cfg: VolumeMount[],
): EffectiveTag {
  const v = cfg.find(
    (x) => x.name === eff.name && x.container_path === eff.container_path,
  );
  if (!v) return 'extra';
  return v.read_only === eff.read_only ? 'same' : 'changed';
}

/** 归一化：深拷贝可变字段，便于受控编辑与比较（entry/entry_args 已拔除；
 *  volumes 可缺省 → 归一为空数组） */
export function normalizeConfig(cfg: ContainerConfig): ContainerConfig {
  return {
    ...cfg,
    mounts: cfg.mounts.map((m) => ({ ...m })),
    volumes: (cfg.volumes ?? []).map((v) => ({ ...v })),
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
    volumes: (v.volumes ?? []).map((vol) => ({ ...vol })),
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
