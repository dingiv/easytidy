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
//
// 顶部工具条（两种模式共用）：配置 YAML 桥 ——
//   「示例模板」下拉：conf_examples() 内置模板 → conf_parse 预填
//   「加载 YAML」：conf_load_dialog()（rfd 选取宿主 YAML → 解析 → 整表预填）
//   「导出 YAML」：conf_save_dialog()（表单 → YAML → rfd 保存对话框落盘）
// edit 模式下加载只替换"可编辑载荷"（容器身份 name / 血缘 flavor 保留——
// apply_container_config 会强制当前容器名，血缘断开是不可逆的身份变更）。

import {
  ApiOutlined,
  CodeOutlined,
  DatabaseOutlined,
  ExportOutlined,
  FolderOpenOutlined,
  ImportOutlined,
  UserOutlined,
} from '@ant-design/icons';
import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { App as AntApp, Button, Select, Tooltip } from 'antd';
import { errMsg } from '../../lib/errors';
import type {
  ContainerConfig,
  ContainerConfigView,
  HostUser,
  MountConfig,
  NetworkMode,
  PassthroughPreview,
  PortMapping,
  ServerEnvItem,
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
  keep_id: true,
  user_uid: null,
  user_gid: null,
  user_name: null,
  gui: false,
  gpu_nvidia: false,
  gpu_amd: false,
};

export interface ContainerConfigEditorProps {
  /** 受控值（完整 ContainerConfig） */
  value: ContainerConfig;
  onChange(next: ContainerConfig): void;
  /** create = 名称/镜像可编辑；edit = 身份只读 + 对照视图 */
  mode: 'create' | 'edit';
  /** create 模式下锁死名称（模板编辑既有模板用；模板名 = 文件名） */
  nameLocked?: boolean;
  /** podman inspect 当前生效投影（edit 模式对照；创建时 null） */
  effective?: ContainerConfigView | null;
  /** 宿主用户（用户页 uid 映射语义对照） */
  hostUser?: HostUser | null;
}

/** 后端 LoadConfResp（加载 YAML 对话框返回） */
interface LoadConfResp {
  path: string;
  config: ContainerConfig;
}

/** 后端 ExampleConf（内置示例模板条目） */
interface ExampleConf {
  name: string;
  yaml: string;
}

/** 单页容器配置编辑器：自上而下 section（容器 / 挂载 / 网络 / 环境变量 / 用户） */
export function ContainerConfigEditor({
  value, onChange, mode, nameLocked = false, effective = null, hostUser = null,
}: ContainerConfigEditorProps) {
  const { message } = AntApp.useApp();
  const [examples, setExamples] = useState<ExampleConf[]>([]);
  const [exampleSel, setExampleSel] = useState<string | undefined>(undefined);
  const [busy, setBusy] = useState(false);
  // GUI + GPU 透传注入预览（gui / gpu_nvidia / gpu_amd 任一开启时由
  // passthrough_preview 计算；null = 未开启/未算出）
  const [preview, setPreview] = useState<PassthroughPreview | null>(null);
  // 服务器运行时注入的 env（server.env：XAUTHORITY 探测 / XDG_DATA_DIRS 修正）
  const [serverEnv, setServerEnv] = useState<ServerEnvItem[]>([]);

  // 挂载即拉内置示例模板（conf/*.yaml 编译期打进二进制的三件套）
  useEffect(() => {
    invoke<ExampleConf[]>('conf_examples')
      .then(setExamples)
      .catch((err) => console.error('conf_examples failed:', err));
  }, []);

  // 透传预览：value.gui / value.gpu_nvidia / value.gpu_amd 任一开启时按当前
  // mounts/env 计算引擎将隐式注入的增量（防抖 200ms 合并连击；mounts/env/gui/gpu
  // 变化 → 增量随之变化，需重算）。gui/gpu_* 是 ContainerConfig 一等字段，随
  // value 传入，故预览入参只传 config。
  useEffect(() => {
    if (!value.gui && !value.gpu_nvidia && !value.gpu_amd) {
      setPreview(null);
      return;
    }
    let cancelled = false;
    const t = setTimeout(async () => {
      try {
        const p = await invoke<PassthroughPreview>('passthrough_preview', {
          config: value,
        });
        if (!cancelled) setPreview(p);
      } catch (err) {
        console.error('passthrough_preview failed:', err);
      }
    }, 200);
    return () => {
      cancelled = true;
      clearTimeout(t);
    };
    // 增量取决于 gui/gpu_* 开关与用户声明的 mounts/env
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [value.gui, value.gpu_nvidia, value.gpu_amd, value.mounts, value.env]);

  // 服务器运行时注入的 env（server.env）：仅 edit 模式（单容器 GUI、容器在运行）
  // 可查——create/模板模式无容器。effective 变化（容器启动/重启/刷新）触发重查。
  // 失败（非单容器模式 / 容器未运行）→ 空，不显示此类行。
  useEffect(() => {
    if (mode !== 'edit') {
      setServerEnv([]);
      return;
    }
    let cancelled = false;
    invoke<ServerEnvItem[]>('server_injected_env')
      .then((res) => {
        if (!cancelled) setServerEnv(res ?? []);
      })
      .catch((err) => {
        console.error('server_injected_env failed:', err);
        if (!cancelled) setServerEnv([]);
      });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [mode, effective]);

  const update = (patch: Partial<ContainerConfig>) => onChange({ ...value, ...patch });
  const updateNetwork = (patch: Partial<ContainerConfig['network']>) =>
    onChange({ ...value, network: { ...value.network, ...patch } });

  /**
   * 应用外部加载的配置（YAML 文件 / 内置示例共用）。
   * edit 模式保留容器身份（name）——改名 = 另一个容器；加载只替换可编辑载荷。
   */
  const applyLoaded = (loaded: ContainerConfig) => {
    if (mode === 'edit') {
      onChange({
        ...loaded,
        name: value.name,
      });
    } else {
      onChange(loaded);
    }
  };

  /** 「加载 YAML」：rfd 选文件 → 后端解析 → 整表预填 */
  const handleLoad = async () => {
    setBusy(true);
    try {
      const resp = await invoke<LoadConfResp>('conf_load_dialog');
      applyLoaded(resp.config);
      message.success(`已加载配置：${resp.path}`);
    } catch (err) {
      const msg = errMsg(err);
      if (msg !== '已取消') message.error(msg);
    } finally {
      setBusy(false);
    }
  };

  /** 「示例模板」下拉：选中 → conf_parse → 预填 */
  const handleExampleSelect = async (name: string | undefined) => {
    if (!name) return;
    const ex = examples.find((e) => e.name === name);
    if (!ex) return;
    setBusy(true);
    setExampleSel(name);
    try {
      const config = await invoke<ContainerConfig>('conf_parse', { text: ex.yaml });
      applyLoaded(config);
      message.success(`已按示例「${name}」预填表单`);
    } catch (err) {
      message.error(errMsg(err, '解析示例模板失败'));
    } finally {
      setBusy(false);
    }
  };

  /** 「导出 YAML」：表单 → YAML → rfd 保存对话框落盘 */
  const handleExport = async () => {
    setBusy(true);
    try {
      const dest = await invoke<string>('conf_save_dialog', { config: value });
      message.success(`已导出：${dest}`);
    } catch (err) {
      const msg = errMsg(err);
      if (msg !== '已取消') message.error(msg);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="config-editor">
      <div className="conf-toolbar">
        <Select
          size="small"
          style={{ minWidth: 150 }}
          placeholder="示例模板"
          value={exampleSel}
          onChange={handleExampleSelect}
          loading={examples.length === 0}
          disabled={busy}
          options={examples.map((e) => ({ value: e.name, label: e.name }))}
          notFoundContent="无内置示例"
        />
        <Tooltip title="从宿主选择 .yaml 配置载入并预填表单">
          <Button
            size="small"
            icon={<ImportOutlined />}
            disabled={busy}
            loading={busy}
            onClick={handleLoad}
          >
            加载 YAML
          </Button>
        </Tooltip>
        <Tooltip title="将当前表单导出为 .yaml（可分享/复用）">
          <Button
            size="small"
            icon={<ExportOutlined />}
            disabled={busy}
            loading={busy}
            onClick={handleExport}
          >
            导出 YAML
          </Button>
        </Tooltip>
      </div>

      <section className="config-section">
        <h3 className="config-section-title">
          <DatabaseOutlined /> 容器
        </h3>
        <ContainerPane
          edit={value}
          mode={mode}
          nameLocked={nameLocked}
          onNameChange={(name) => update({ name })}
          onImageChange={(image) => update({ image })}
          onSilentBootChange={(silent_boot) => update({ silent_boot })}
          onPersistentChange={(persistent) => update({ persistent })}
          onGuiChange={(gui) => update({ gui })}
          onGpuNvidiaChange={(gpu_nvidia) => update({ gpu_nvidia })}
          onGpuAmdChange={(gpu_amd) => update({ gpu_amd })}
        />
      </section>

      <section className="config-section">
        <h3 className="config-section-title">
          <FolderOpenOutlined /> 挂载
        </h3>
        <MountsPane
          mounts={value.mounts}
          readonlyMounts={preview?.mounts ?? []}
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
          readonlyEnv={preview?.env ?? []}
          serverEnv={serverEnv}
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
          keepId={value.keep_id}
          userUid={value.user_uid ?? null}
          userGid={value.user_gid ?? null}
          userName={value.user_name ?? null}
          onKeepIdChange={(keep_id) => update({ keep_id })}
          onUserUidChange={(user_uid) => update({ user_uid })}
          onUserGidChange={(user_gid) => update({ user_gid })}
          onUserNameChange={(user_name) => update({ user_name })}
          hostUser={hostUser}
          effective={effective}
        />
      </section>
    </div>
  );
}