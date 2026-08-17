// 容器面板：基本信息（名称/镜像，按模式可编辑）+ entry/参数 + 静默启动 +
// 持久化 + 血缘。统一编辑器（ContainerConfigEditor）的「容器」页。

import { Input, Space, Switch, Tag, Typography } from 'antd';
import type { ContainerConfig } from '../../types';

interface ContainerPaneProps {
  edit: ContainerConfig;
  /** create = 名称/镜像可编辑；edit = 身份只读（改名/换镜像 = 另一个容器） */
  mode: 'create' | 'edit';
  onNameChange(v: string): void;
  onImageChange(v: string): void;
  onEntryChange(v: string): void;
  onEntryArgsChange(v: string[]): void;
  onSilentBootChange(v: boolean): void;
  onPersistentChange(v: boolean): void;
}

export function ContainerPane({
  edit, mode, onNameChange, onImageChange, onEntryChange, onEntryArgsChange,
  onSilentBootChange, onPersistentChange,
}: ContainerPaneProps) {
  return (
    <div className="config-fields">
      {mode === 'create' && (
        <div className="config-field">
          <label>名称</label>
          <Input
            placeholder="如：my-env（模板按名称展开，请先输入）"
            value={edit.name}
            onChange={(e) => onNameChange(e.target.value)}
          />
        </div>
      )}
      <div className="config-field">
        <label>镜像{mode === 'create' ? '（需已拉取）' : ''}</label>
        {mode === 'create' ? (
          <Input
            placeholder="如：docker.io/library/ubuntu:24.04"
            value={edit.image}
            onChange={(e) => onImageChange(e.target.value)}
          />
        ) : (
          <Typography.Text code>{edit.image}</Typography.Text>
        )}
      </div>
      <div className="config-field">
        <label>入口应用（entry）</label>
        <Input
          placeholder="容器启动时运行的命令，留空则不启动应用"
          value={edit.entry ?? ''}
          onChange={(e) => onEntryChange(e.target.value)}
        />
        <span className="section-hint">随「保存并重启」一起生效：容器重建后由 server 链式拉起。</span>
      </div>
      <div className="config-field">
        <label>入口应用参数</label>
        <Input
          placeholder="空格分隔，拼接在 entry 后执行（含空格参数需引号）"
          value={edit.entry_args.join(' ')}
          onChange={(e) => onEntryArgsChange(e.target.value.split(/\s+/).filter(Boolean))}
        />
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
            宿主开机时无头启动容器并拉起 entry 应用（不弹 GUI）
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
