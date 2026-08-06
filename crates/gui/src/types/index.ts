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
}

/// Passthrough state from passthrough_state
export interface PassthroughState {
  exported_apps: string[];
  auto_start_enabled: boolean;
}

/// Container configuration from config_get
export interface ContainerConfig {
  mounts: Array<{ source: string; target: string }>;
  network: {
    mode: 'bridge' | 'host' | 'none';
    port_mappings?: Array<{ container_port: number; host_port: number }>;
  };
  entry_app?: string;
  silent_boot: boolean;
}

/// PTY event from pty_open
export interface PtyEvent {
  kind: 'data' | 'exited';
  data?: number[];
  code?: number;
}
