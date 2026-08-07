/// Application mode types
export interface AppModeCentralized {
  Centralized: null;
}

export interface AppModePerContainer {
  PerContainer: { name: string };
}

export type AppMode = AppModeCentralized | AppModePerContainer;

/// Container summary from list_containers
export interface ContainerSummary {
  name: string;
  id: string;
  image: string;
  status: string;
  managed: boolean;
}

/// Env view from env_list（环境语义面板，docs/13-mutable-env-paradigm.md）
/// status: 'running'（运行中）/ 'exited' | 'created'（已停止）/ 'missing'（仅配置保留）
export interface EnvView {
  name: string;
  image: string;
  status: string;
  managed: boolean;
}

/// File system entry from fs_list
export interface FsEntry {
  name: string;
  is_dir: boolean;
  size?: number;
  mtime?: string;
}

/// Application info from apps_list
export interface AppInfo {
  name: string;
  icon_path: string;
  exec: string;
  comment?: string;
  desktop_file: string;
  /// Categories（.desktop，逗号分隔；passthrough 导出时原样带出）
  categories?: string;
  /// StartupNotify（.desktop）
  startup_notify: boolean;
  /// StartupWMClass（.desktop；Wayland 窗口匹配用）
  startup_wm_class?: string;
}

/// Passthrough state from passthrough_state
export interface PassthroughState {
  exported_apps: string[];
  auto_start_enabled: boolean;
}

/// Mount config for the container config manager
export interface MountConfig {
  host_path: string;
  container_path: string;
  read_only: boolean;
}

/// Network mode: host networking, or bridge with port mappings
export type NetworkMode = 'host' | 'mapped';

export interface PortMapping {
  host_port: number;
  container_port: number;
  protocol: 'tcp' | 'udp';
}

export interface ContainerNetworkConfig {
  mode: NetworkMode;
  ports: PortMapping[];
}

/// Container configuration from get_container_config / apply_container_config
export interface ContainerConfig {
  name: string;
  image: string;
  entry: string | null;
  silent_boot: boolean;
  persistent: boolean;
  mounts: MountConfig[];
  network: ContainerNetworkConfig;
  /// 容器环境变量（"KEY=VALUE" 列表）
  env: string[];
  /// 用户一致性映射开关（keep-id：容器内 uid 与宿主对齐）
  user_home: boolean;
}

/// 宿主用户信息（uid 映射语义对照表数据源；null = 探测失败，容器降级 root 运行）
export interface HostUser {
  name: string;
  uid: number;
  gid: number;
  home: string;
}

/// Podman inspect 投影：当前生效状态（不复用 ContainerConfig——view 本无
/// entry/silent_boot/persistent；env 含系统注入，对比需过滤后子集比较）
export interface ContainerConfigView {
  mounts: MountConfig[];
  network: ContainerNetworkConfig;
  /// 当前生效环境变量（含 EASYTIDY_USER_* 系统注入与 podman 默认 PATH/HOSTNAME 等）
  env: string[];
  /// 容器进程用户（keep-id 下为 "0:0"）
  user: string | null;
  /// userns 模式（keep-id 容器可能回显 "private"/null，语义以 user_home + docs/12 为准）
  userns_mode: string | null;
}

/// Result of get_container_config: saved config vs config actually effective in podman
export interface ContainerConfigResult {
  config: ContainerConfig;
  effective: ContainerConfigView | null;
  host_user: HostUser | null;
}

/// PTY event from pty_open
export interface PtyEvent {
  kind: 'data' | 'exited';
  data?: number[];
  code?: number;
}
