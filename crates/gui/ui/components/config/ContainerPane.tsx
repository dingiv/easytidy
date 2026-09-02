// 容器面板：基本信息（名称/镜像，按模式可编辑）+ 静默启动 + 持久化 + 血缘。
// 统一编辑器（ContainerConfigEditor）的「容器」页。
//
// 入口应用（entry/entry_args）已移入容器内由 server 管理（见 docs），
// 不再作为宿主侧容器配置暴露，故此处不再渲染 entry 字段。

import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Input, Select, Space, Switch, Tag, Typography } from 'antd';
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
  /** GPU 透传值（null = 关；开时默认 "all"，可改为设备名 / device=<uuid>） */
  onGpuChange(v: string | null): void;
}

export function ContainerPane({
  edit, mode, nameLocked = false, onNameChange, onImageChange,
  onSilentBootChange, onPersistentChange, onGuiChange, onGpuChange,
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
        <label>GPU 透传</label>
        <Space>
          <Select
            style={{ width: 120 }}
            value={(() => {
              if (!edit.gpu) return '';
              const v = edit.gpu.split('=')[0];
              return v === 'nvidia' || v === 'amd' ? v : 'nvidia';
            })()}
            onChange={(vendor: string) => onGpuChange(vendor || null)}
            options={[
              { value: '', label: '关闭' },
              { value: 'nvidia', label: 'NVIDIA' },
              { value: 'amd', label: 'AMD' },
            ]}
          />
          {edit.gpu && (
            <Input
              style={{ width: 200 }}
              value={(() => {
                const idx = edit.gpu!.indexOf('=');
                return idx === -1 ? 'all' : edit.gpu!.slice(idx + 1);
              })()}
              onChange={(e) => {
                const spec = e.target.value.trim() || 'all';
                const vendor = edit.gpu!.split('=')[0];
                const v = vendor === 'nvidia' || vendor === 'amd' ? vendor : 'nvidia';
                onGpuChange(spec === 'all' ? v : `${v}=${spec}`);
              }}
              placeholder="all / 0 / device=<uuid>"
            />
          )}
        </Space>
        <Typography.Text type="secondary" className="section-hint">
          经 &lt;vendor&gt;.com/gpu=&lt;值&gt; 注入设备节点
          {edit.gpu && edit.gpu.startsWith('nvidia')
            ? ' + NVIDIA_* env'
            : ''}
          （见「环境变量」页只读项）；需宿主已装对应 Container Toolkit 并生成 CDI spec
        </Typography.Text>
      </div>
      {edit.flavor && (
        <div className="config-field">
          <label>来源模板</label>
          <Space>
            <Tag color="blue">{edit.flavor}</Tag>
            <Typography.Text type="secondary">
              配置由此模板展开；模板修改后可在顶部「从模板同步」重新对齐
            </Typography.Text>
          </Space>
        </div>
      )}
    </div>
  );
}