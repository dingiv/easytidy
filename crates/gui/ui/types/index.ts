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

/// Passthrough 配置中的应用条目（有状态:auto_start=true 或 custom 应用）
export interface PassthroughApp {
  /// 应用标识:扫描应用=容器内 .desktop 路径;自定义应用="custom:<name>"
  id: string;
  name: string;
  /// 实际执行命令串(已去 %U 占位符;自定义应用原样)
  cmd: string;
  /// 仅扫描应用:容器内 .desktop 路径
  desktop_file?: string;
  /// 容器启动时自动拉起
  auto_start: boolean;
  /// 宿主本地图标路径（~/.easytidy/icons/；自定义应用用户选定后落盘）
  icon?: string;
}

/// 已导出的 .desktop 条目(含全文)
export interface ExportedPassthrough {
  desktop_file: string;
  content: string;
}

/// 收藏（pin 到工具栏）的应用条目
export interface PinnedApp {
  id: string;
  name: string;
  /// 实际执行命令串
  cmd: string;
  /// 图标：扫描应用 = 容器内路径（经 server 拉取显示）；
  /// 自定义应用 = 宿主 ~/.easytidy/icons 路径
  icon?: string;
}

/// Passthrough state from passthrough_state
export interface PassthroughState {
  exported: ExportedPassthrough[];
  configured_apps: PassthroughApp[];
  pinned: PinnedApp[];
  /// 容器自启动模式（systemd user unit）：off 关闭 / silent 静默 / gui 非静默
  boot_mode?: 'off' | 'silent' | 'gui';
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
  /// entry 应用参数（空格拼接到 entry 后，server 经 shell 执行）
  entry_args: string[];
  silent_boot: boolean;
  persistent: boolean;
  mounts: MountConfig[];
  network: ContainerNetworkConfig;
  /// 容器环境变量（"KEY=VALUE" 列表）
  env: string[];
  /// 用户一致性映射开关（keep-id：容器内 uid 与宿主对齐）
  user_home: boolean;
  /// 血缘：来源 flavor 模板名（展开时盖章；null = 自由创建，不参与模板同步）
  flavor?: string | null;
}

/// 模板血缘状态（get_container_config 返回；null = 无血缘）
export interface FlavorStatus {
  /// 来源模板名
  flavor: string;
  /// 模板文件是否存在（被删 = 无法同步，仅展示血缘）
  exists: boolean;
  /// 实例基座与模板当前声明不一致（提示「从模板同步」）
  drifted: boolean;
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
  /// 模板血缘状态（null = 自由创建）
  flavor_status?: FlavorStatus | null;
}

/// 活跃终端会话（get_terminals / server pty.list）
export interface TerminalInfo {
  stream_id: number;
  /// 显示命令（pty.open 的 cmd；空 = 默认登录 shell）
  cmd: string;
  /// 以 root 运行
  as_root: boolean;
  /// 常驻会话（server 持有，不随连接断开清理）
  persistent: boolean;
  /// 最近一次工作目录
  cwd?: string | null;
}

/// 镜像摘要（images_list；GUI 镜像管理）
export interface ImageSummary {
  /// 镜像 ID（短）
  id: string;
  /// 仓库标签（悬空镜像为空）
  repo_tags: string[];
  /// 展开后大小（字节）
  size: number;
  /// 创建时间（unix 秒）
  created: number;
}

/// flavor 启动配置模板（flavor_list_detailed / flavor_save；
/// 描述"将一个镜像 run 起来"需要向 easytidy 传递的完整配置清单）
export interface Flavor {
  name: string;
  /// 基础镜像
  image: string;
  /// GUI 透传（显示环境注入 + 字体/图标 + 用户映射）
  gui: boolean;
  /// 创建后按序执行的安装命令
  setup: string[];
  /// entry 应用（容器内可执行名）
  entry?: string | null;
  /// entry 应用参数
  entry_args: string[];
  /// 额外路径映射
  mounts: MountConfig[];
  /// 用户一致性映射（gui=true 时强制开启；Rust 侧为共享基座 bool，默认 true）
  user_home?: boolean;
  /// 网络配置
  network: ContainerNetworkConfig;
}

/// PTY event from pty_open
export interface PtyEvent {
  kind: 'data' | 'exited' | 'cwdChanged';
  data?: number[];
  code?: number;
  /// 工作目录（kind="cwdChanged" 时;server 主动推送）
  cwd?: string;
}
