// 容器面板：基本信息（名称/镜像，按模式可编辑）+ 静默启动 + 持久化 + 血缘。
// 统一编辑器（ContainerConfigEditor）的「容器」页。
//
// 入口应用（entry/entry_args）已移入容器内由 server 管理（见 docs），
// 不再作为宿主侧容器配置暴露，故此处不再渲染 entry 字段。

import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Input, Space, Switch, Typography } from 'antd';
import type { ContainerConfig, ImageSummary } from '../../types';
import { ImageSelect } from './ImageSelect';

interface ContainerPaneProps {
  edit: ContainerConfig;
  /** create = 名称/镜像可编辑；edit = 身份只读（改名/换镜像 = 另一个容器） */
  mode: 'create' | 'edit';
  /** create 模式下锁死名称（如模板编辑既有模板：模板名 = 文件名，改名应复制/新建） */
  nameLocked?: boolean;
  onNameChange(v: string): void;
  onImageChange(v: string): void;
  onSilentBootChange(v: boolean): void;
  onPersistentChange(v: boolean): void;
  /** GUI 透传开关（实例一等项；存意图，展开/重建时按宿主实时注入） */
  onGuiChange(v: boolean): void;
  /** NVIDIA GPU 透传开关（注入 NVIDIA_* env + nvidia.com/gpu=all CDI） */
  onGpuNvidiaChange(v: boolean): void;
  /** AMD GPU 透传开关（探测注入 /dev/kfd + AMD render 节点；ROCm 容器内自检） */
  onGpuAmdChange(v: boolean): void;
}

export function ContainerPane({
  edit, mode, nameLocked = false, onNameChange, onImageChange,
  onSilentBootChange, onPersistentChange, onGuiChange,
  onGpuNvidiaChange, onGpuAmdChange,
}: ContainerPaneProps) {
  // 镜像下拉数据：仅 create 模式需要拉（edit 模式镜像只读，渲染 Typography.Text）。
  const [images, setImages] = useState<ImageSummary[]>([]);
  useEffect(() => {
    if (mode !== 'create') return;
    invoke<ImageSummary[]>('images_list')
      .then((list) => setImages(list ?? []))
      .catch((err) => console.error('images_list failed:', err));
  }, [mode]);

  return (
    <div className="config-fields">
      {mode === 'create' && (
        <div className="config-field">
          <label>名称</label>
          <Input
            placeholder="如：my-env（模板按名称展开，请先输入）"
            value={edit.name}
            onChange={(e) => onNameChange(e.target.value)}
            disabled={nameLocked}
          />
        </div>
      )}
      <div className="config-field">
        <label>镜像{mode === 'create' ? '（需已拉取，可搜索本地已有）' : ''}</label>
        {mode === 'create' ? (
          // 自定义渲染下拉（不用 antd Select 的虚拟列表——WebKitGTK 下其自定义
          // wheel 处理滚动过慢）。原生可滚动 <div>，搜索 + 键盘导航见 ImageSelect。
          <ImageSelect
            images={images}
            value={edit.image}
            onChange={(v) => onImageChange(v)}
            placeholder="如：docker.io/library/ubuntu:24.04（输入关键字筛选本地镜像）"
          />
        ) : (
          <Typography.Text code>{edit.image}</Typography.Text>
        )}
      </div>
      <div className="config-field">
        <label>静默启动</label>
        <Space>
          <Switch
            checked={edit.silent_boot}
            onChange={onSilentBootChange}
            checkedChildren="开"
            unCheckedChildren="关"
          />
          <Typography.Text type="secondary">
            点击桌面快捷方式时仅静默启动容器（不弹 GUI 窗口）
          </Typography.Text>
        </Space>
      </div>
      <div className="config-field">
        <label>持久化</label>
        <Space>
          <Switch
            checked={edit.persistent}
            onChange={onPersistentChange}
            checkedChildren="常驻"
            unCheckedChildren="非常驻"
          />
          <Typography.Text type="secondary">catatonit + server 生命周期托管</Typography.Text>
        </Space>
      </div>
      <div className="config-field">
        <label>GUI 透传</label>
        <Space>
          <Switch
            checked={!!edit.gui}
            onChange={onGuiChange}
            checkedChildren="开"
            unCheckedChildren="关"
          />
          <Typography.Text type="secondary">
            展开/重建时按宿主实时 env 注入 DISPLAY/WAYLAND/XDG_RUNTIME_DIR + X11/Wayland/字体图标挂载
          </Typography.Text>
        </Space>
      </div>
      <div className="config-field">
        <label>NVIDIA GPU 透传</label>
        <Space>
          <Switch
            checked={!!edit.gpu_nvidia}
            onChange={onGpuNvidiaChange}
            checkedChildren="开"
            unCheckedChildren="关"
          />
          <Typography.Text type="secondary">
            注入 NVIDIA_VISIBLE_DEVICES / NVIDIA_DRIVER_CAPABILITIES env + nvidia.com/gpu=all CDI
            设备节点（见「环境变量」页只读项）
          </Typography.Text>
        </Space>
      </div>
      <div className="config-field">
        <label>AMD GPU 透传</label>
        <Space>
          <Switch
            checked={!!edit.gpu_amd}
            onChange={onGpuAmdChange}
            checkedChildren="开"
            unCheckedChildren="关"
          />
          <Typography.Text type="secondary">
            透传 /dev/kfd + AMD render 节点（PCI 0x1002；无 AMD CDI 依赖）
          </Typography.Text>
        </Space>
      </div>
    </div>
  );
}