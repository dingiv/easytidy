// 环境变量面板：编辑表（增删）+ 只读生效环境（来源分栏）。

import { useState } from 'react';
import { App as AntApp, Button, Empty, Input, Table, Typography } from 'antd';
import type { TableProps } from 'antd';
import { DeleteOutlined, PlusOutlined } from '@ant-design/icons';
import {
  EASYTIDY_SYSTEM_ENV_PREFIX,
  PODMAN_DEFAULT_ENV_KEYS,
  parseEnv,
  validateEnv,
} from './utils';

interface EnvPaneProps {
  env: string[];
  effectiveEnv: string[] | null; // null = 容器未创建
  onAdd(key: string, value: string): void;
  onRemove(idx: number): void;
}

export function EnvPane({ env, effectiveEnv, onAdd, onRemove }: EnvPaneProps) {
  const { message } = AntApp.useApp();
  const [newEnv, setNewEnv] = useState({ key: '', value: '' });

  const addEnv = () => {
    const key = newEnv.key.trim();
    const err = validateEnv(key, env);
    if (err) {
      message.error(err);
      return;
    }
    onAdd(key, newEnv.value);
    setNewEnv({ key: '', value: '' });
  };

  const envColumns: TableProps<{ key: string; value: string }>['columns'] = [
    {
      title: '变量名',
      dataIndex: 'key',
      key: 'key',
      ellipsis: true,
      render: (v: string) => <span className="env-key-cell">{v}</span>,
    },
    {
      title: '值',
      dataIndex: 'value',
      key: 'value',
      ellipsis: true,
      render: (v: string) => <Typography.Text>{v}</Typography.Text>,
    },
    {
      title: '操作',
      key: 'actions',
      width: 70,
      render: (_: unknown, _rec: { key: string; value: string }, idx: number) => (
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
        rowKey={(_rec: { key: string; value: string }, i) => `env-${i}`}
        columns={envColumns}
        dataSource={env.map(parseEnv)}
        pagination={false}
        locale={{ emptyText: <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="暂无自定义环境变量" /> }}
      />
      <div className="add-row">
        <Input
          placeholder="变量名，如 TEST_KEY"
          value={newEnv.key}
          onChange={(e) => setNewEnv((en) => ({ ...en, key: e.target.value }))}
          onPressEnter={addEnv}
          style={{ width: 220 }}
        />
        <Input
          placeholder="值（可留空）"
          value={newEnv.value}
          onChange={(e) => setNewEnv((en) => ({ ...en, value: e.target.value }))}
          onPressEnter={addEnv}
        />
        <Button type="primary" icon={<PlusOutlined />} onClick={addEnv}>
          添加
        </Button>
      </div>
      <span className="section-hint">
        环境变量随「保存并重启」在容器重建后生效；EASYTIDY_USER_* 由引擎保留注入，不可手动修改。
      </span>

      <div className="config-subsection">
        <Typography.Text strong>生效环境（含系统注入，只读）</Typography.Text>
        {effectiveEnv ? (
          <Table
            size="small"
            rowKey={(_rec: { key: string; value: string; src: string }, i) => `eff-env-${i}`}
            dataSource={effectiveEnv
              .map((kv) => {
                const { key, value } = parseEnv(kv);
                let src = '用户配置';
                if (key.startsWith(EASYTIDY_SYSTEM_ENV_PREFIX)) src = 'easytidy 系统注入';
                else if (PODMAN_DEFAULT_ENV_KEYS.has(key)) src = 'podman 默认';
                return { key, value, src };
              })
              .sort((a, b) => (a.src === b.src ? 0 : a.src === '用户配置' ? -1 : 1))}
            pagination={false}
            locale={{ emptyText: <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="无" /> }}
            columns={[
              {
                title: '变量名',
                dataIndex: 'key',
                key: 'key',
                ellipsis: true,
                render: (v: string) => <span className="env-key-cell">{v}</span>,
              },
              {
                title: '值',
                dataIndex: 'value',
                key: 'value',
                ellipsis: true,
              },
              {
                title: '来源',
                dataIndex: 'src',
                key: 'src',
                width: 150,
                render: (v: string) => (
                  <span className={v === '用户配置' ? 'env-src-user' : 'env-src-system'}>{v}</span>
                ),
              },
            ]}
          />
        ) : (
          <Typography.Text type="secondary">
            容器未创建，无实际生效环境（可编辑下方配置后保存并重启）。
          </Typography.Text>
        )}
      </div>
    </div>
  );
}
