// 容器面板：基本信息（名称/镜像，按模式可编辑）+ 静默启动 + 图标 + GUI/GPU 直通。
// 统一编辑器（ContainerConfigEditor）的「容器」页。
//
// entry/entry_args（入口应用）与 persistent（常驻开关）是已拔除的死字段
// （2026-10-02）：应用启动走 passthrough auto-start，生命周期由 catatonit +
// server 托管——不再作为宿主侧容器配置暴露。

import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { App as AntApp, Button, Input, Segmented, Space, Switch, Typography } from 'antd';
import { PictureOutlined } from '@ant-design/icons';
import type { ContainerConfig, GuiMode, ImageSummary } from '../../types';
import { ImageSelect } from './ImageSelect';

interface ContainerPaneProps {
  edit: ContainerConfig;
  /** create = 名称/镜像可编辑；edit = 身份只读（改名/换镜像 = 另一个容器） */
  mode: 'create' | 'edit';
  /** create 模式下锁死名称（如模板编辑既有模板：模板名 = 文件名，改名应复制/新建） */
  nameLocked?: boolean;
  onNameChange(v: string): void;
  onImageChange(v: string): void;
  /** 容器入口图标（宿主图片路径；null = 清除，回退内置品牌图标） */
  onIconChange(v: string | null): void;
  onSilentBootChange(v: boolean): void;
  /** GUI 直通 X11 半（三态）：none 不注入 / direct 宿主直通 / bypass 旁路 */
  onGuiX11Change(v: GuiMode): void;
  /** GUI 直通 Wayland 半（三态，同上） */
  onGuiWaylandChange(v: GuiMode): void;
  /** NVIDIA GPU 透传开关（注入 NVIDIA_* env + nvidia.com/gpu=all CDI） */
  onGpuNvidiaChange(v: boolean): void;
  /** AMD GPU 透传开关（探测注入 /dev/kfd + AMD render 节点；ROCm 容器内自检） */
  onGpuAmdChange(v: boolean): void;
  /** tmpfs 快捷开关：/tmp 挂 tmpfs（mode=1777，开机即净） */
  onTmpfsTmpChange(v: boolean): void;
  /** tmpfs 快捷开关：/run 挂 tmpfs（noexec mode=0755，开机即净） */
  onTmpfsRunChange(v: boolean): void;
}

export function ContainerPane({
  edit, mode, nameLocked = false, onNameChange, onImageChange,
  onIconChange, onSilentBootChange,
  onGuiX11Change, onGuiWaylandChange,
  onGpuNvidiaChange, onGpuAmdChange,
  onTmpfsTmpChange, onTmpfsRunChange,
}: ContainerPaneProps) {
  const { message } = AntApp.useApp();
  // 镜像下拉数据：仅 create 模式需要拉（edit 模式镜像只读，渲染 Typography.Text）。
  const [images, setImages] = useState<ImageSummary[]>([]);
  useEffect(() => {
    if (mode !== 'create') return;
    invoke<ImageSummary[]>('images_list')
      .then((list) => setImages(list ?? []))
      .catch((err) => console.error('images_list failed:', err));
  }, [mode]);

  /** 容器入口图标：原生文件选择 → 后端拷入 icons 目录 → 路径写入配置 */
  const handlePickIcon = async () => {
    try {
      const path = await invoke<string>('container_pick_icon', { name: edit.name.trim() });
      onIconChange(path);
    } catch (err: any) {
      if (String(err) !== '已取消') {
        message.error(String(err));
      }
    }
  };

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
        <label>容器图标（可选）</label>
        <Space wrap>
          {edit.icon ? (
            <Typography.Text code ellipsis style={{ maxWidth: 300 }}>
              {edit.icon}
            </Typography.Text>
          ) : (
            <Typography.Text type="secondary">未设置（使用内置品牌图标）</Typography.Text>
          )}
          <Button
            size="small"
            icon={<PictureOutlined />}
            disabled={!edit.name.trim()}
            onClick={handlePickIcon}
            title={edit.name.trim() ? '选择宿主图片作为容器入口图标' : '请先填写名称'}
          >
            选择图片
          </Button>
          {edit.icon && (
            <Button size="small" type="text" danger onClick={() => onIconChange(null)}>
              清除
            </Button>
          )}
        </Space>
        <span className="section-hint">
          容器入口桌面快捷方式图标：导出时经内置品牌工具加工（渐变边框 + 水印，256×256）
        </span>
      </div>
      <div className="config-field">
        <label>GUI 直通 · X11</label>
        <Space>
          <Segmented
            value={edit.gui_x11 ?? 'none'}
            onChange={(v) => onGuiX11Change(v as GuiMode)}
            options={[
              { label: '关', value: 'none' },
              { label: '直通', value: 'direct' },
              { label: '旁路', value: 'bypass' },
            ]}
          />
          <Typography.Text type="secondary">
            直通=注入宿主 DISPLAY + /tmp/.X11-unix + XAUTHORITY（画到宿主屏）；
            旁路=容器自有 X（:0）+ 宿主 X0→X100 单文件旁路（HOST_DISPLAY/HOST_XAUTHORITY）
          </Typography.Text>
        </Space>
      </div>
      <div className="config-field">
        <label>GUI 直通 · Wayland</label>
        <Space>
          <Segmented
            value={edit.gui_wayland ?? 'none'}
            onChange={(v) => onGuiWaylandChange(v as GuiMode)}
            options={[
              { label: '关', value: 'none' },
              { label: '直通', value: 'direct' },
              { label: '旁路', value: 'bypass' },
            ]}
          />
          <Typography.Text type="secondary">
            直通=注入宿主 WAYLAND_DISPLAY；旁路=容器 wayland-0 悬空（GTK/Qt 回落 X）+ HOST_WAYLAND_DISPLAY 旁路
          </Typography.Text>
        </Space>
      </div>
      <div className="config-field">
        <Typography.Text type="secondary">
          任一半非 none 即注入该模型的共享基建：keep_id + 字体/图标挂载 + XDG_DATA_DIRS +
          runtime dir（直通=宿主目录直挂；旁路=容器私有目录 + HOST_XDG_RUNTIME_DIR 改名挂载）
        </Typography.Text>
      </div>
      <div className="config-field">
        <label>tmpfs（开机即净，不攒垃圾）</label>
        <Space>
          <Switch
            checked={!!edit.tmpfs_tmp}
            onChange={onTmpfsTmpChange}
            checkedChildren="/tmp"
            unCheckedChildren="/tmp"
          />
          <Typography.Text type="secondary">mode=1777，容器用户可写</Typography.Text>
          <Switch
            checked={!!edit.tmpfs_run}
            onChange={onTmpfsRunChange}
            checkedChildren="/run"
            unCheckedChildren="/run"
          />
          <Typography.Text type="secondary">noexec mode=0755，私有 XDG_RUNTIME_DIR 在其上</Typography.Text>
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
            注入 NVIDIA_DRIVER_CAPABILITIES env + nvidia.com/gpu=all CDI 设备节点（见「环境变量」页只读项）
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