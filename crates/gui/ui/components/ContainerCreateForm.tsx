// 新建容器页内表单 —— 主 GUI 两个入口（容器 tab / 环境 tab）共用。
//
// 页内呈现（用户偏好，替代模态）：创建方式（flavor 模板展开 / 镜像默认值）
// + 统一编辑器（ContainerConfigEditor，与单实例 GUI 配置管理同一套表单）
// + 提交栏。提交走 env_new(config)（统一创建入口）。
//
// flavor 展开在宿主侧执行（GUI 透传注入宿主 DISPLAY/探测字体目录），
// 预填表单后任何字段可继续改——模板只是预填。

import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { App as AntApp, Alert, Button, Radio, Select, Space, Typography } from 'antd';
import { PlayCircleOutlined } from '@ant-design/icons';
import type { ContainerConfig } from '../types';
import { BLANK_CONTAINER_CONFIG, ContainerConfigEditor } from './config/ContainerConfigEditor';
import './ContainerCreateForm.css';

interface ContainerCreateFormProps {
  /** 放弃创建，返回列表 */
  onCancel(): void;
  /** 创建成功后回调（刷新列表并退出表单） */
  onCreated(): void;
  /** 预选模板（flavor 卡片「启动」进入）：名称就绪后自动展开预填 */
  initialFlavor?: string;
}

function ContainerCreateFormInner({ onCancel, onCreated, initialFlavor }: ContainerCreateFormProps) {
  const { message, modal } = AntApp.useApp();

  const [mode, setMode] = useState<'flavor' | 'image'>('flavor');
  const [flavors, setFlavors] = useState<string[]>([]);
  const [selectedFlavor, setSelectedFlavor] = useState<string | undefined>(initialFlavor);
  const [config, setConfig] = useState<ContainerConfig>(BLANK_CONTAINER_CONFIG);
  const [creating, setCreating] = useState(false);
  // 预选模板的自动展开只做一次（名称就绪后）；此后切换/手选均为手动
  const [autoExpanded, setAutoExpanded] = useState(false);

  // 可用 flavor 模板（挂载即取；无模板时回退镜像方式）
  useEffect(() => {
    invoke<string[]>('flavor_list')
      .then((list) => {
        setFlavors(list);
        if (list.length === 0) setMode('image');
      })
      .catch((err) => {
        console.error('flavor_list failed:', err);
        setFlavors([]);
        setMode('image');
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
      message.error(err?.message || `展开模板 ${flavor} 失败`);
    }
  };

  const handleCreate = async () => {
    const name = config.name.trim();
    if (!name) {
      message.error('请在「容器」页输入名称');
      return;
    }
    if (!config.image.trim()) {
      message.error('请输入镜像（需已拉取）');
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
      const msg = typeof err === 'string' ? err : err?.message || JSON.stringify(err);
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

  const switchMode = (next: 'flavor' | 'image') => {
    setMode(next);
    setSelectedFlavor(undefined);
    // 切到镜像方式回到默认值（flavor 预填的挂载/env 不残留）；
    // 已输入的名称保留（展开按名称绑定）
    setConfig((prev) => ({
      ...BLANK_CONTAINER_CONFIG,
      name: prev.name,
      image: mode === 'flavor' ? '' : prev.image,
    }));
  };

  const nameReady = config.name.trim().length > 0;

  // 预选模板（flavor 卡片「启动」进入）：名称就绪后自动展开预填一次
  useEffect(() => {
    if (!initialFlavor || mode !== 'flavor' || autoExpanded || !nameReady) return;
    setAutoExpanded(true);
    handleFlavorSelect(initialFlavor);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [initialFlavor, mode, autoExpanded, nameReady]);

  return (
    <div className="container-create-form">
      <div className="container-create-head">
        <Typography.Title level={4} className="container-create-title">
          新建容器
        </Typography.Title>
        <Radio.Group value={mode} onChange={(e) => switchMode(e.target.value)}>
          <Radio.Button value="flavor" disabled={flavors.length === 0}>
            模板（flavor）
          </Radio.Button>
          <Radio.Button value="image">镜像</Radio.Button>
        </Radio.Group>
      </div>

      {mode === 'flavor' && (
        <div className="container-create-flavor">
          <Select
            style={{ minWidth: 260 }}
            placeholder={nameReady ? '选择预配置模板（展开预填表单，可继续调整任何字段）' : '请先在「容器」页输入名称'}
            value={selectedFlavor}
            onChange={handleFlavorSelect}
            disabled={!nameReady}
            options={flavors.map((f) => ({ value: f, label: f }))}
            notFoundContent="暂无可用模板（Flavor 面板可创建）"
          />
          {!nameReady && (
            <Alert
              type="info"
              showIcon
              message="模板按容器名称展开（挂载/环境变量与其绑定），请先在下方「容器」页输入名称"
            />
          )}
        </div>
      )}

      <ContainerConfigEditor mode="create" value={config} onChange={setConfig} />

      <div className="container-create-foot">
        <Space>
          <Button onClick={onCancel} disabled={creating}>
            取消
          </Button>
          <Button
            type="primary"
            icon={<PlayCircleOutlined />}
            loading={creating}
            onClick={handleCreate}
          >
            创建并启动
          </Button>
        </Space>
        <Typography.Text type="secondary" className="container-create-hint">
          镜像需已在「镜像」面板拉取；创建后自动注册配置并生成桌面图标。
        </Typography.Text>
      </div>
    </div>
  );
}

/** antd App 包裹（message/modal 可用） */
export function ContainerCreateForm(props: ContainerCreateFormProps) {
  return (
    <AntApp>
      <ContainerCreateFormInner {...props} />
    </AntApp>
  );
}
