// ContainerConfig 统一编辑器 —— 主 GUI 创建（ContainerCreateForm）与
// 单实例 GUI 配置管理（ConfigManager）共用的页内表单。
//
// 受控组件（value/onChange）。单页布局：从上到下分为 容器 / 挂载 / 网络 /
// 环境变量 / 用户 五个 section,各自带标题 + 内边距卡片,不再嵌套 antd Tabs。
//
// - mode='create'：名称/镜像可编辑（容器 section）——flavor 展开
//   预填后任何字段可继续改
// - mode='edit'：名称恒定（改名 = 另一个容器）、镜像只读展示 + 血缘标签；
//   effective（podman inspect 投影）与 hostUser 提供对照视图，创建时缺省

import {
  ApiOutlined,
  CodeOutlined,
  DatabaseOutlined,
  FolderOpenOutlined,
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
import { ContainerPane } from './ContainerPane';
import { EnvPane } from './EnvPane';
import { MountsPane } from './MountsPane';
import { NetworkPane } from './NetworkPane';
import { UserPane } from './UserPane';

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

export interface ContainerConfigEditorProps {
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

/** 单页容器配置编辑器：自上而下 section（容器 / 挂载 / 网络 / 环境变量 / 用户） */
export function ContainerConfigEditor({
  value, onChange, mode, effective = null, hostUser = null,
}: ContainerConfigEditorProps) {
  const update = (patch: Partial<ContainerConfig>) => onChange({ ...value, ...patch });
  const updateNetwork = (patch: Partial<ContainerConfig['network']>) =>
    onChange({ ...value, network: { ...value.network, ...patch } });

  return (
    <div className="config-editor">
      <section className="config-section">
        <h3 className="config-section-title">
          <DatabaseOutlined /> 容器
        </h3>
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
      </section>

      <section className="config-section">
        <h3 className="config-section-title">
          <FolderOpenOutlined /> 挂载
        </h3>
        <MountsPane
          mounts={value.mounts}
          onAdd={(m: MountConfig) => update({ mounts: [...value.mounts, m] })}
          onRemove={(idx: number) =>
            update({ mounts: value.mounts.filter((_, i) => i !== idx) })
          }
        />
      </section>

      <section className="config-section">
        <h3 className="config-section-title">
          <ApiOutlined /> 网络
        </h3>
        <NetworkPane
          network={value.network}
          onModeChange={(networkMode: NetworkMode) => updateNetwork({ mode: networkMode })}
          onAddPort={(p: PortMapping) =>
            updateNetwork({ ports: [...value.network.ports, p] })
          }
          onRemovePort={(idx: number) =>
            updateNetwork({ ports: value.network.ports.filter((_, i) => i !== idx) })
          }
        />
      </section>

      <section className="config-section">
        <h3 className="config-section-title">
          <CodeOutlined /> 环境变量
        </h3>
        <EnvPane
          env={value.env}
          effectiveEnv={effective?.env ?? null}
          onAdd={(key, v) => update({ env: [...value.env, `${key}=${v}`] })}
          onRemove={(idx: number) =>
            update({ env: value.env.filter((_, i) => i !== idx) })
          }
        />
      </section>

      <section className="config-section">
        <h3 className="config-section-title">
          <UserOutlined /> 用户
        </h3>
        <UserPane
          userHome={value.user_home}
          onUserHomeChange={(user_home) => update({ user_home })}
          hostUser={hostUser}
          effective={effective}
        />
      </section>
    </div>
  );
}