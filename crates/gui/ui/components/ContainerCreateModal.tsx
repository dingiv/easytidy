// 新建容器 Modal——主 GUI 两个入口（容器 tab / 环境 tab）共用的创建对话框。
//
// 创建方式二选一：
// - flavor 模板：选定后经 flavor_expand 在宿主侧展开为完整 ContainerConfig
//   （注入 GUI 透传 env / X11 / 字体挂载等），预填表单，任何字段可继续改
// - 镜像：默认值起步
// 提交统一走 env_new(config)——与单实例 GUI 配置管理依赖同一 ContainerConfig
// 结构体，容器启动参数自此单一定义（flavor 只是预填，不再是第二套创建路径）。

import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { App as AntApp, Alert, Modal, Radio, Select } from 'antd';
import type { ContainerConfig } from '../types';
import { BLANK_CONTAINER_CONFIG, ContainerConfigForm } from './ContainerConfigForm';

interface ContainerCreateModalProps {
  open: boolean;
  onClose(): void;
  /** 创建成功后回调（刷新列表） */
  onCreated(): void;
}

function ContainerCreateModalInner({ open, onClose, onCreated }: ContainerCreateModalProps) {
  const { message } = AntApp.useApp();

  const [mode, setMode] = useState<'flavor' | 'image'>('flavor');
  const [flavors, setFlavors] = useState<string[]>([]);
  const [selectedFlavor, setSelectedFlavor] = useState<string | undefined>(undefined);
  const [config, setConfig] = useState<ContainerConfig>(BLANK_CONTAINER_CONFIG);
  const [creating, setCreating] = useState(false);

  // 可用 flavor 模板（挂载即取；Modal 常驻渲染，open 时重置状态）
  useEffect(() => {
    invoke<string[]>('flavor_list')
      .then((list) => setFlavors(list))
      .catch((err) => {
        console.error('flavor_list failed:', err);
        setFlavors([]);
      });
  }, []);

  useEffect(() => {
    if (!open) return;
    setMode(flavors.length > 0 ? 'flavor' : 'image');
    setSelectedFlavor(undefined);
    setConfig(BLANK_CONTAINER_CONFIG);
    // flavors 可能晚于 open 到达：首帧为空时默认 image，到货后在下拉里可选
  }, [open]);

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
      message.error('请输入名称');
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
      onClose();
      onCreated();
    } catch (err: any) {
      message.error(err?.message || '创建失败');
      console.error('env_new failed:', err);
    } finally {
      setCreating(false);
    }
  };

  const nameReady = config.name.trim().length > 0;

  return (
    <Modal
      title="新建容器"
      open={open}
      onCancel={onClose}
      onOk={handleCreate}
      okText="创建"
      cancelText="取消"
      confirmLoading={creating}
      width={640}
    >
      <div className="env-modal-form">
        <div className="form-field">
          <label>创建方式</label>
          <Radio.Group
            value={mode}
            onChange={(e) => {
              setMode(e.target.value);
              setSelectedFlavor(undefined);
              // 切到镜像方式回到默认值（flavor 预填的挂载/env 不残留）；
              // 已输入的名称/镜像保留
              setConfig((prev) => ({
                ...BLANK_CONTAINER_CONFIG,
                name: prev.name,
                image: mode === 'flavor' ? '' : prev.image,
              }));
            }}
          >
            <Radio.Button value="flavor" disabled={flavors.length === 0}>
              模板（flavor）
            </Radio.Button>
            <Radio.Button value="image">镜像</Radio.Button>
          </Radio.Group>
        </div>

        {mode === 'flavor' && (
          <div className="form-field">
            <label>模板（选定后展开预填，可继续调整任何字段）</label>
            <Select
              style={{ width: '100%' }}
              placeholder={nameReady ? '选择预配置模板' : '请先在下方输入名称'}
              value={selectedFlavor}
              onChange={handleFlavorSelect}
              disabled={!nameReady}
              options={flavors.map((f) => ({ value: f, label: f }))}
              notFoundContent="暂无可用模板（~/.config/easytidy/flavors/）"
            />
            {!nameReady && (
              <Alert
                type="info"
                showIcon
                style={{ marginTop: 8 }}
                message="模板按容器名称展开（挂载/环境变量与其绑定），请先在下方输入名称"
              />
            )}
          </div>
        )}

        <ContainerConfigForm value={config} onChange={setConfig} />
      </div>
    </Modal>
  );
}

/** antd App 包裹（message 可用） */
export function ContainerCreateModal(props: ContainerCreateModalProps) {
  return (
    <AntApp>
      <ContainerCreateModalInner {...props} />
    </AntApp>
  );
}
