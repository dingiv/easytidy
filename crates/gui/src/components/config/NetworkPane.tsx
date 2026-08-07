// 网络面板：host ↔ mapped 模式切换 + 端口映射表/添加行。

import { useState } from 'react';
import { App as AntApp, Alert, Button, Empty, InputNumber, Radio, Select, Table } from 'antd';
import type { TableProps } from 'antd';
import { DeleteOutlined, PlusOutlined } from '@ant-design/icons';
import type {
  ContainerNetworkConfig,
  NetworkMode,
  PortMapping,
} from '../../types';

interface NetworkPaneProps {
  network: ContainerNetworkConfig;
  onModeChange(mode: NetworkMode): void;
  onAddPort(p: PortMapping): void;
  onRemovePort(idx: number): void;
}

const EMPTY_PORT: PortMapping = { host_port: 0, container_port: 0, protocol: 'tcp' };
const PROTOCOL_OPTIONS = [
  { value: 'tcp', label: 'TCP' },
  { value: 'udp', label: 'UDP' },
];

export function NetworkPane({ network, onModeChange, onAddPort, onRemovePort }: NetworkPaneProps) {
  const { message } = AntApp.useApp();
  const [newPort, setNewPort] = useState<PortMapping>({ ...EMPTY_PORT });

  const addPort = () => {
    if (
      !Number.isInteger(newPort.host_port) ||
      newPort.host_port < 1 ||
      newPort.host_port > 65535
    ) {
      message.error('宿主端口需为 1-65535 的整数');
      return;
    }
    if (
      !Number.isInteger(newPort.container_port) ||
      newPort.container_port < 1 ||
      newPort.container_port > 65535
    ) {
      message.error('容器端口需为 1-65535 的整数');
      return;
    }
    onAddPort({ ...newPort });
    setNewPort({ ...EMPTY_PORT });
  };

  const portColumns: TableProps<PortMapping>['columns'] = [
    {
      title: '宿主端口',
      dataIndex: 'host_port',
      key: 'host_port',
      width: 120,
      render: (v: number) => <span className="port-cell">{v}</span>,
    },
    {
      title: '容器端口',
      dataIndex: 'container_port',
      key: 'container_port',
      width: 120,
      render: (v: number) => <span className="port-cell">{v}</span>,
    },
    {
      title: '协议',
      dataIndex: 'protocol',
      key: 'protocol',
      width: 90,
      render: (p: string) => <span className="proto-cell">{p.toUpperCase()}</span>,
    },
    {
      title: '操作',
      key: 'actions',
      width: 70,
      render: (_: unknown, _rec: PortMapping, idx: number) => (
        <Button
          type="text"
          danger
          size="small"
          icon={<DeleteOutlined />}
          onClick={() => onRemovePort(idx)}
        />
      ),
    },
  ];

  return (
    <div className="config-pane">
      <Radio.Group value={network.mode} onChange={(e) => onModeChange(e.target.value as NetworkMode)}>
        <Radio.Button value="host">host 模式</Radio.Button>
        <Radio.Button value="mapped">映射模式</Radio.Button>
      </Radio.Group>

      {network.mode === 'host' ? (
        <Alert
          type="info"
          showIcon
          message="host 模式下端口映射不可用"
          description="切换为映射模式后可配置端口转发；映射模式需要容器使用 bridge 网络。"
          className="mode-hint"
        />
      ) : (
        <>
          <Table
            size="small"
            rowKey={(_rec: PortMapping, i) => `port-${i}`}
            columns={portColumns}
            dataSource={network.ports}
            pagination={false}
            locale={{ emptyText: <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="暂无端口映射" /> }}
          />
          <div className="add-row">
            <InputNumber
              min={1}
              max={65535}
              precision={0}
              placeholder="宿主端口"
              value={newPort.host_port || null}
              onChange={(v) => setNewPort((p) => ({ ...p, host_port: v ?? 0 }))}
              style={{ width: 140 }}
            />
            <InputNumber
              min={1}
              max={65535}
              precision={0}
              placeholder="容器端口"
              value={newPort.container_port || null}
              onChange={(v) => setNewPort((p) => ({ ...p, container_port: v ?? 0 }))}
              style={{ width: 140 }}
            />
            <Select
              value={newPort.protocol}
              onChange={(v) => setNewPort((p) => ({ ...p, protocol: v as PortMapping['protocol'] }))}
              options={PROTOCOL_OPTIONS}
              style={{ width: 100 }}
            />
            <Button type="primary" icon={<PlusOutlined />} onClick={addPort}>
              添加
            </Button>
          </div>
        </>
      )}
    </div>
  );
}
