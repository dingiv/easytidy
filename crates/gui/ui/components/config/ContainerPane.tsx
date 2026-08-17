// 容器面板：镜像/入口应用/静默启动/持久化/血缘。

import { Input, Space, Switch, Tag, Typography } from 'antd';
import type { ContainerConfig } from '../../types';

interface ContainerPaneProps {
  edit: ContainerConfig;
  onEntryChange(v: string): void;
  onEntryArgsChange(v: string[]): void;
  onSilentBootChange(v: boolean): void;
  onPersistentChange(v: boolean): void;
}

export function ContainerPane({
  edit, onEntryChange, onEntryArgsChange, onSilentBootChange, onPersistentChange,
}: ContainerPaneProps) {
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
