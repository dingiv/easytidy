// 容器面板：镜像/入口应用/静默启动/持久化。

import { Input, Space, Switch, Typography } from 'antd';
import type { ContainerConfig } from '../../types';

interface ContainerPaneProps {
  edit: ContainerConfig;
  onEntryChange(v: string): void;
  onSilentBootChange(v: boolean): void;
}

export function ContainerPane({ edit, onEntryChange, onSilentBootChange }: ContainerPaneProps) {
  return (
    <div className="config-fields">
      <div className="config-field">
        <label>镜像</label>
        <Typography.Text code>{edit.image}</Typography.Text>
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
        <Typography.Text>{edit.persistent ? '是' : '否'}</Typography.Text>
      </div>
    </div>
  );
}
