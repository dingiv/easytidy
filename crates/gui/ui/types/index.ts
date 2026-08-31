/// Application mode types
export interface AppModeMaster {
  Master: null;
}

export interface AppModeWorker {
  Worker: { name: string };
}

export type AppMode = AppModeMaster | AppModeWorker;

/// Container summary from list_containers
export interface ContainerSummary {
  name: string;
  id: string;
  image: string;
  status: string;
  managed: boolean;
}

/// Env view from env_list（环境语义面板，docs/13-mutable-env-paradigm.md）
/// managed=true（easytidy 接管）status: 'running'（运行中）/ 'exited' | 'created'（已停止）
///   / 'missing'（仅配置保留，podman 容器已不存在 → 前端显示「容器丢失」）；
/// managed=false：他人创建的未接管容器，status 为 podman 真实状态（前端显示「未接管」）
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
  /** 符号链接（图标区分：链接目录/链接文件） */
  is_symlink?: boolean;
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
  /// 用户一致性映射开关（keep-id：宿主 uid ↔ 容器同 uid 锁死 1:1）
  keep_id: boolean;
  /// 容器默认用户 uid（null = 宿主登录用户 uid）
  user_uid?: number | null;
  /// 容器默认用户 gid（null = 宿主登录用户 gid）
  user_gid?: number | null;
  /// 容器内用户名（可选；有值时首次创建经 root exec useradd 建号）
  user_name?: string | null;
  /// GUI 透传（意图）：开启时引擎展开/重建注入宿主显示 env + X11/Wayland/字体图标挂载
  gui?: boolean;
  /// GPU 透传（意图）：值为 "all" / 设备名 / "device=<uuid>"；null/缺省 = 不透传
  gpu?: string | null;
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

/// 宿主用户信息（uid 映射语义对照表数据源；null = 探测失败，
/// 容器创建报错而非静默降级 root）
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
  /// 当前生效环境变量（含 EASYTIDY_USER_NAME 系统注入与
  /// podman 默认 PATH/HOSTNAME 等）
  env: string[];
  /// 容器进程用户（"<uid>:<gid>"，即容器默认用户）
  user: string | null;
  /// userns 模式（keep-id 容器可能回显 "private"/null，语义以 keep_id + docs/12 为准）
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

/// 活跃终端会话（get_terminals / server pty.list；均为容器默认用户会话）
export interface TerminalInfo {
  stream_id: number;
  /// 显示命令（pty.open 的 cmd；空 = 默认登录 shell）
  cmd: string;
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

/// GUI + GPU 透传预览（passthrough_preview）：`gui`/`gpu` 开启时引擎会隐式注入的
/// 增量 env / mounts（模板里已声明的同 destination / 同 key 项不重复）。
/// 模板编辑器开启对应开关时以只读行展示，让用户看见引擎将注入什么。
export interface PassthroughPreview {
  /// 展开时注入的环境变量（"KEY=VALUE"）
  env: string[];
  /// 展开时注入的挂载（仅 gui 产生）
  mounts: MountConfig[];
}

/// 服务器运行时注入的环境变量（server.env）：容器内 server 启动时探测/修正的
/// session 耦合值（XAUTHORITY 自动探测 / XDG_DATA_DIRS 系统默认修正）——配置里
/// 定义不了、宿主侧也无法预知最终值。配置管理器展示为「easytidy 注入」只读行。
export interface ServerEnvItem {
  /** 环境变量名 */
  key: string;
  /** 注入的值（当前生效） */
  value: string;
  /** 注入原因（展示用） */
  note: string;
}

/// conf 启动配置模板（conf_templates / conf_template_get / conf_save_template；
/// GUI 全面切 YAML 后取代 flavor TOML 模板——见 `commands::config::conf_templates`）。
///
/// 后端 `ConfTemplate` 用 `#[serde(flatten)]` 平铺 `ContainerConfig` 字段,
/// YAML 序列化形状 = 容器关键参数 + `setup`。`gui` / `gpu` 透传意图在
/// `ContainerConfig` 共享基座内（模板与实例共用），故前端直接继承、不再重复声明。
export interface ConfTemplate extends ContainerConfig {
  /// 创建后按序执行的安装命令（本轮只存不执行；执行链路下一步接入）
  setup: string[];
}

/// PTY event from pty_open
export interface PtyEvent {
  kind: 'data' | 'exited' | 'cwdChanged';
  data?: number[];
  code?: number;
  /// 工作目录（kind="cwdChanged" 时;server 主动推送）
  cwd?: string;
}
