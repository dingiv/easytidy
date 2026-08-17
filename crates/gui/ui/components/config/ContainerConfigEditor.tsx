// ContainerConfig 统一编辑器 —— 主 GUI 创建（ContainerCreateForm）与
// 单实例 GUI 配置管理（ConfigManager）共用的页内表单。
//
// 受控组件（value/onChange），Tabs 分区复用配置管理器的既有 Pane：
// 创建与改配是同一套编辑语义，无第二份字段逻辑。
//
// - mode='create'：名称/镜像可编辑（容器页基本信息区）——flavor 展开
//   预填后任何字段可继续改
// - mode='edit'：名称恒定（改名 = 另一个容器）、镜像只读展示 + 血缘标签；
//   effective（podman inspect 投影）与 hostUser 提供对照视图，创建时缺省

import { Tabs } from 'antd';
import {
  ApiOutlined,
  CodeOutlined,
  FolderOpenOutlined,
  SettingOutlined,
  UserOutlined,
} from '@ant-design/icons';
import type {
  ContainerConfig,
  ContainerConfigView,
  HostUser,
  MountConfig,
  NetworkMode,
  PortMapping,
} from '../../types';
import { MountsPane } from './MountsPane';
import { NetworkPane } from './NetworkPane';
import { EnvPane } from './EnvPane';
import { UserPane } from './UserPane';
import { ContainerPane } from './ContainerPane';
import '../ConfigManager.css'; // 自带样式依赖（编辑器独立于 ConfigManager 使用）

/** 新建默认值（与 core ContainerConfig::default + 产品语义对齐：
 * Host 网络 + 用户一致性映射 + 常驻） */
export const BLANK_CONTAINER_CONFIG: ContainerConfig = {
  name: '',
  image: '',
  entry: null,
  entry_args: [],
  silent_boot: false,
  persistent: true,
  mounts: [],
  network: { mode: 'host', ports: [] },
  env: [],
  user_home: true,
  flavor: null,
};

interface ContainerConfigEditorProps {
  /** 受控值（完整 ContainerConfig） */
  value: ContainerConfig;
  onChange(next: ContainerConfig): void;
  /** create = 名称/镜像可编辑；edit = 身份只读 + 对照视图 */
  mode: 'create' | 'edit';
  /** podman inspect 当前生效投影（edit 模式对照；创建时 null） */
  effective?: ContainerConfigView | null;
  /** 宿主用户（用户页 uid 映射语义对照） */
  hostUser?: HostUser | null;
}

/** ContainerConfig 页内编辑器（Tabs：容器/挂载/网络/环境变量/用户） */
export function ContainerConfigEditor({
  value, onChange, mode, effective = null, hostUser = null,
}: ContainerConfigEditorProps) {
  const update = (patch: Partial<ContainerConfig>) => onChange({ ...value, ...patch });
  const updateNetwork = (patch: Partial<ContainerConfig['network']>) =>
    onChange({ ...value, network: { ...value.network, ...patch } });

  return (
    <Tabs
      className="config-manager-tabs"
      items={[
        {
          key: 'container',
          label: (
            <span>
              <SettingOutlined /> 容器
            </span>
          ),
          children: (
            <ContainerPane
              edit={value}
              mode={mode}
              onNameChange={(name) => update({ name })}
              onImageChange={(image) => update({ image })}
              onEntryChange={(entry) => update({ entry: entry.trim() || null })}
              onEntryArgsChange={(entry_args) => update({ entry_args })}
              onSilentBootChange={(silent_boot) => update({ silent_boot })}
              onPersistentChange={(persistent) => update({ persistent })}
            />
          ),
        },
        {
          key: 'mounts',
          label: (
            <span>
              <FolderOpenOutlined /> 挂载
            </span>
          ),
          children: (
            <MountsPane
              mounts={value.mounts}
              onAdd={(m: MountConfig) => update({ mounts: [...value.mounts, m] })}
              onRemove={(idx: number) =>
                update({ mounts: value.mounts.filter((_, i) => i !== idx) })
              }
            />
          ),
        },
        {
          key: 'network',
          label: (
            <span>
              <ApiOutlined /> 网络
            </span>
          ),
          children: (
            <NetworkPane
              network={value.network}
              onModeChange={(mode: NetworkMode) => updateNetwork({ mode })}
              onAddPort={(p: PortMapping) =>
                updateNetwork({ ports: [...value.network.ports, p] })
              }
              onRemovePort={(idx: number) =>
                updateNetwork({ ports: value.network.ports.filter((_, i) => i !== idx) })
              }
            />
          ),
        },
        {
          key: 'env',
          label: (
            <span>
              <CodeOutlined /> 环境变量
            </span>
          ),
          children: (
            <EnvPane
              env={value.env}
              effectiveEnv={effective?.env ?? null}
              onAdd={(key, v) => update({ env: [...value.env, `${key}=${v}`] })}
              onRemove={(idx: number) =>
                update({ env: value.env.filter((_, i) => i !== idx) })
              }
            />
          ),
        },
        {
          key: 'user',
          label: (
            <span>
              <UserOutlined /> 用户
            </span>
          ),
          children: (
            <UserPane
              userHome={value.user_home}
              onUserHomeChange={(user_home) => update({ user_home })}
              hostUser={hostUser}
              effective={effective}
            />
          ),
        },
      ]}
    />
  );
}
