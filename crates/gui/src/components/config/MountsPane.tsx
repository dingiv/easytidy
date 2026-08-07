// 挂载（路径映射）面板：表格 + 添加行（宿主路径/容器路径/只读）。

import { useState } from 'react';
import { App as AntApp, Button, Empty, Input, Switch, Table } from 'antd';
import type { TableProps } from 'antd';
import { DeleteOutlined, PlusOutlined } from '@ant-design/icons';
import type { MountConfig } from '../../types';

interface MountsPaneProps {
  mounts: MountConfig[];
  onAdd(m: MountConfig): void;
  onRemove(idx: number): void;
}

export function MountsPane({ mounts, onAdd, onRemove }: MountsPaneProps) {
  const { message } = AntApp.useApp();
  const [newMount, setNewMount] = useState({
    host_path: '',
    container_path: '',
    read_only: false,
  });

  const addMount = () => {
    const host = newMount.host_path.trim();
    const target = newMount.container_path.trim();
    if (!host) {
      message.error('宿主路径不能为空');
      return;
    }
    if (!target) {
      message.error('容器路径不能为空');
      return;
    }
    onAdd({ host_path: host, container_path: target, read_only: newMount.read_only });
    setNewMount({ host_path: '', container_path: '', read_only: false });
  };

  const mountColumns: TableProps<MountConfig>['columns'] = [
    {
      title: '宿主路径',
      dataIndex: 'host_path',
      key: 'host_path',
      ellipsis: true,
      render: (v: string) => <span className="path-cell">{v}</span>,
    },
    {
      title: '容器路径',
      dataIndex: 'container_path',
      key: 'container_path',
      ellipsis: true,
      render: (v: string) => <span className="path-cell">{v}</span>,
    },
    {
      title: '只读',
      dataIndex: 'read_only',
      key: 'read_only',
      width: 90,
      render: (ro: boolean) => <Switch size="small" checked={ro} disabled />,
    },
    {
      title: '操作',
      key: 'actions',
      width: 70,
      render: (_: unknown, _rec: MountConfig, idx: number) => (
        <Button
          type="text"
          danger
          size="small"
          icon={<DeleteOutlined />}
          onClick={() => onRemove(idx)}
        />
      ),
    },
  ];

  return (
    <div className="config-pane">
      <Table
        size="small"
        rowKey={(_rec: MountConfig, i) => `mount-${i}`}
        columns={mountColumns}
        dataSource={mounts}
        pagination={false}
        locale={{ emptyText: <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="暂无路径映射" /> }}
      />
      <div className="add-row">
        <Input
          placeholder="宿主路径，如 /home/user/data"
          value={newMount.host_path}
          onChange={(e) => setNewMount((m) => ({ ...m, host_path: e.target.value }))}
          onPressEnter={addMount}
        />
        <Input
          placeholder="容器路径，如 /data"
          value={newMount.container_path}
          onChange={(e) => setNewMount((m) => ({ ...m, container_path: e.target.value }))}
          onPressEnter={addMount}
        />
        <Switch
          checked={newMount.read_only}
          onChange={(v) => setNewMount((m) => ({ ...m, read_only: v }))}
          checkedChildren="只读"
          unCheckedChildren="读写"
        />
        <Button type="primary" icon={<PlusOutlined />} onClick={addMount}>
          添加
        </Button>
      </div>
    </div>
  );
}
