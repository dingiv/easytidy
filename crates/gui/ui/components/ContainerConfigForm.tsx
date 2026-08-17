// ContainerConfig 受控编辑表单——主 GUI 创建容器与单实例 GUI 配置管理
// 依赖同一结构体（core::models::ContainerConfig 是容器启动参数的唯一事实
// 定义，TS 侧 types/index.ts 为其镜像）。高级分区复用配置管理器的
// Mounts/Network/Env Pane：创建与改配是同一套编辑语义，无第二份字段逻辑。

import { Collapse, Input, Switch } from 'antd';
import type { ContainerConfig, ContainerNetworkConfig, MountConfig, NetworkMode, PortMapping } from '../types';
import { MountsPane } from './config/MountsPane';
import { NetworkPane } from './config/NetworkPane';
import { EnvPane } from './config/EnvPane';

interface ContainerConfigFormProps {
  /** 受控值（完整 ContainerConfig） */
  value: ContainerConfig;
  onChange(next: ContainerConfig): void;
}

/** 新建默认值（与 core ContainerConfig::default + 产品语义对齐：
 * Host 网络 + 用户一致性映射 + 常驻；旧 create_container 的默认行为） */
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

/** ContainerConfig 编辑表单（名称/镜像 + 可折叠高级配置） */
export function ContainerConfigForm({ value, onChange }: ContainerConfigFormProps) {
  const update = (patch: Partial<ContainerConfig>) => onChange({ ...value, ...patch });
  const updateNetwork = (patch: Partial<ContainerNetworkConfig>) =>
    onChange({ ...value, network: { ...value.network, ...patch } });

  return (
    <div className="container-config-form">
      <div className="form-field">
        <label>名称</label>
        <Input
          placeholder="如：my-env"
          value={value.name}
          onChange={(e) => update({ name: e.target.value })}
        />
      </div>
      <div className="form-field">
        <label>镜像（需已拉取）</label>
        <Input
          placeholder="如：docker.io/library/ubuntu:latest"
          value={value.image}
          onChange={(e) => update({ image: e.target.value })}
        />
      </div>

      <Collapse
        ghost
        items={[
          {
            key: 'advanced',
            label: '高级配置（网络 / 挂载 / 环境变量 / 行为）',
            children: (
              <>
                <NetworkPane
                  network={value.network}
                  onModeChange={(mode: NetworkMode) => updateNetwork({ mode })}
                  onAddPort={(p: PortMapping) =>
                    updateNetwork({ ports: [...value.network.ports, p] })
                  }
                  onRemovePort={(idx: number) =>
                    updateNetwork({
                      ports: value.network.ports.filter((_, i) => i !== idx),
                    })
                  }
                />
                <MountsPane
                  mounts={value.mounts}
                  onAdd={(m: MountConfig) => update({ mounts: [...value.mounts, m] })}
                  onRemove={(idx: number) =>
                    update({ mounts: value.mounts.filter((_, i) => i !== idx) })
                  }
                />
                <EnvPane
                  env={value.env}
                  effectiveEnv={null}
                  onAdd={(key, v) => update({ env: [...value.env, `${key}=${v}`] })}
                  onRemove={(idx: number) =>
                    update({ env: value.env.filter((_, i) => i !== idx) })
                  }
                />
                <div className="config-fields">
                  <div className="config-field">
                    <label>用户一致性映射（keep-id）</label>
                    <Switch
                      checked={value.user_home}
                      onChange={(v) => update({ user_home: v })}
                    />
                  </div>
                  <div className="config-field">
                    <label>常驻容器</label>
                    <Switch
                      checked={value.persistent}
                      onChange={(v) => update({ persistent: v })}
                    />
                  </div>
                  <div className="config-field">
                    <label>开机静默自启</label>
                    <Switch
                      checked={value.silent_boot}
                      onChange={(v) => update({ silent_boot: v })}
                    />
                  </div>
                  <div className="config-field">
                    <label>entry 应用（启动时链式拉起；类 ENTRYPOINT）</label>
                    <Input
                      placeholder="如：firefox（留空 = 无）"
                      value={value.entry ?? ''}
                      onChange={(e) => update({ entry: e.target.value.trim() || null })}
                    />
                  </div>
                  <div className="config-field">
                    <label>entry 参数（空格分隔，随 entry 一起执行）</label>
                    <Input
                      placeholder="如：--new-window https://example.com"
                      value={value.entry_args.join(' ')}
                      onChange={(e) =>
                        update({ entry_args: e.target.value.split(/\s+/).filter(Boolean) })
                      }
                    />
                  </div>
                </div>
              </>
            ),
          },
        ]}
      />
    </div>
  );
}
