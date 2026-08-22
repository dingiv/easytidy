// 新建容器页内表单 —— Master GUI「新建容器」图标入口的主面板。
//
// 单一职责:输入容器名 + 选 flavor(可选)+ 填剩余字段 → 提交 env_new。
// 镜像字段就是 ContainerConfigEditor "容器" section 里的镜像输入框。
//
// flavor 处理逻辑:
// - 挂载时拉一次 flavor_list;有 flavor 时在表单顶部显示 Select(默认禁用,
//   等用户先输入名称后启用)用于把模板声明预填进表单
// - 没 flavor 时直接用空白表单,等同于旧的"image"模式(用户填镜像即可)
// - 选了 flavor 后任何字段都可继续改——flavor 仅作预填,不是约束

import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { errMsg } from '../lib/errors';
import { App as AntApp, Alert, Button, Select, Typography } from 'antd';
import { PlayCircleOutlined } from '@ant-design/icons';
import type { ContainerConfig } from '../types';
import { BLANK_CONTAINER_CONFIG, ContainerConfigEditor } from './config/ContainerConfigEditor';
import './ContainerCreateForm.css';

interface ContainerCreateFormProps {
  /** 创建成功后回调（刷新列表并退出表单） */
  onCreated(): void;
  /** 预选模板（flavor 卡片「启动」进入）：名称就绪后自动展开预填 */
  initialFlavor?: string;
}

function ContainerCreateFormInner({ onCreated, initialFlavor }: ContainerCreateFormProps) {
  const { message, modal } = AntApp.useApp();

  const [flavors, setFlavors] = useState<string[]>([]);
  const [selectedFlavor, setSelectedFlavor] = useState<string | undefined>(initialFlavor);
  const [config, setConfig] = useState<ContainerConfig>(BLANK_CONTAINER_CONFIG);
  const [creating, setCreating] = useState(false);
  // 预选模板的自动展开只做一次（名称就绪后）；此后切换/手选均为手动
  const [autoExpanded, setAutoExpanded] = useState(false);

  // 可用 flavor 模板（挂载即取；空数组表示"无模板,直接走镜像方式"）
  useEffect(() => {
    invoke<string[]>('flavor_list')
      .then(setFlavors)
      .catch((err) => {
        console.error('flavor_list failed:', err);
        setFlavors([]);
      });
  }, []);

  /** 选定 flavor → 宿主侧展开为完整 ContainerConfig 预填表单（可继续改） */
  const handleFlavorSelect = async (flavor: string) => {
    const name = config.name.trim();
    if (!name) return; // Select 已按名称非空启用，防御
    try {
      const expanded = await invoke<ContainerConfig>('flavor_expand', { name, flavor });
      setConfig(expanded); // expanded.name = name，保留用户输入
      setSelectedFlavor(flavor);
    } catch (err: any) {
      message.error(errMsg(err, `展开模板 ${flavor} 失败`));
    }
  };

  /** 切换 flavor 选中 → 清掉模板预填,保留已输入名称 + 镜像 */
  const handleFlavorClear = () => {
    setSelectedFlavor(undefined);
    setConfig((prev) => ({
      ...BLANK_CONTAINER_CONFIG,
      name: prev.name,
      image: prev.image,
    }));
  };

  const handleCreate = async () => {
    const name = config.name.trim();
    if (!name) {
      message.error('请输入容器名称');
      return;
    }
    if (!config.image.trim()) {
      message.error('请输入镜像（需已拉取，未拉取请到「镜像」面板）');
      return;
    }
    setCreating(true);
    try {
      await invoke('env_new', { config: { ...config, name } });
      message.success(`「${name}」已创建并运行`);
      onCreated();
    } catch (err: any) {
      // 完整报错展示：创建/启动链路多步易错，toast 会消失且截断，
      // 用 Modal 展示全文（可选中复制）
      const msg = errMsg(err);
      console.error('env_new failed:', err);
      modal.error({
        title: `创建容器「${name}」失败`,
        width: 620,
        content: <pre className="error-detail">{msg}</pre>,
        okText: '知道了',
      });
    } finally {
      setCreating(false);
    }
  };

  const nameReady = config.name.trim().length > 0;
  const hasFlavors = flavors.length > 0;

  // 预选模板（flavor 卡片「启动」进入）：名称就绪后自动展开预填一次
  useEffect(() => {
    if (!initialFlavor || autoExpanded || !nameReady) return;
    setAutoExpanded(true);
    handleFlavorSelect(initialFlavor);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [initialFlavor, autoExpanded, nameReady]);

  return (
    <div className="container-create-form">
      {/* flavor 入口：仅当本地存在 flavor 时显示（空 flavor_list 时直接走镜像方式） */}
      {hasFlavors && (
        <div className="container-create-flavor">
          <Select
            style={{ minWidth: 260 }}
            placeholder={
              nameReady
                ? '选择预设配置预填表单（可继续调整任何字段）'
                : '请先在下方「容器」section 输入名称'
            }
            value={selectedFlavor}
            onChange={handleFlavorSelect}
            onClear={handleFlavorClear}
            allowClear
            disabled={!nameReady}
            options={flavors.map((f) => ({ value: f, label: f }))}
            notFoundContent="暂无可用模板"
          />
          {!nameReady && (
            <Alert
              type="info"
              showIcon
              message="模板按容器名称展开（挂载/环境变量与其绑定），请先在下方「容器」section 输入名称"
            />
          )}
        </div>
      )}

      <ContainerConfigEditor mode="create" value={config} onChange={setConfig} />

      <div className="container-create-foot">
        <Typography.Text type="secondary" className="container-create-hint">
          {selectedFlavor
            ? `由模板「${selectedFlavor}」预填，镜像需已拉取。修改字段后再次保存将以当前表单内容为准。`
            : '镜像需已在「镜像」面板拉取；创建后自动注册配置并生成桌面图标。'}
        </Typography.Text>
        <Button
          type="primary"
          icon={<PlayCircleOutlined />}
          loading={creating}
          onClick={handleCreate}
        >
          创建并启动
        </Button>
      </div>
    </div>
  );
}

/** antd App 包裹由 MasterView 提供 */
export function ContainerCreateForm(props: ContainerCreateFormProps) {
  return <ContainerCreateFormInner {...props} />;
}