// 环境变量面板：单一合并表（按来源分类型）+ 添加行。
//
// 类型（来源）：
// - 用户自定义：env 配置（可增删，随「保存并重启」生效）
// - EasyTidy 注入：EASYTIDY_USER_* 系统注入 + GUI/GPU 透传注入（只读）
// - Podman 注入：podman 默认注入（PATH/HOSTNAME 等，只读）
// - 镜像默认：镜像 ENV 带入的其余变量（只读）
//
// 用户自定义行可删除（其余行带锁图标只读）；添加行固定在表下方。

import { useState } from 'react';
import { App as AntApp, Button, Empty, Input, Table, Typography } from 'antd';
import type { TableProps } from 'antd';
import { DeleteOutlined, LockOutlined, PlusOutlined } from '@ant-design/icons';
import {
  EASYTIDY_SYSTEM_ENV_PREFIX,
  PODMAN_DEFAULT_ENV_KEYS,
  parseEnv,
  validateEnv,
} from './utils';

interface EnvPaneProps {
  env: string[];
  /** GUI/GPU 透传开启时引擎将隐式注入的 env（只读展示；gui/gpu 开启时由
   *  passthrough_preview 计算） */
  readonlyEnv?: string[];
  effectiveEnv: string[] | null; // null = 容器未创建
  onAdd(key: string, value: string): void;
  onRemove(idx: number): void;
}

/** 环境变量来源类型 */
type EnvRowType = 'user' | 'easytidy' | 'podman' | 'image';

const ENV_TYPE_META: Record<EnvRowType, { label: string; cls: string; order: number }> = {
  user: { label: '用户自定义', cls: 'env-src-user', order: 0 },
  easytidy: { label: 'EasyTidy 注入', cls: 'env-src-easytidy', order: 1 },
  podman: { label: 'Podman 注入', cls: 'env-src-system', order: 2 },
  image: { label: '镜像默认', cls: 'env-src-system', order: 3 },
};

interface EnvRow {
  key: string;
  value: string;
  type: EnvRowType;
  /** 仅用户自定义行可删除 */
  removable: boolean;
  /** env 配置中的下标（仅用户自定义行，删除用） */
  idx?: number;
}

/** 合并三个来源（用户配置 / 引擎透传预览 / 生效环境）为单一表；
 *  同一 key 只出现一次（用户配置优先），其余按来源归类、只读 */
function buildEnvRows(env: string[], readonlyEnv: string[], effectiveEnv: string[] | null): EnvRow[] {
  const rows: EnvRow[] = [];
  const userKeys = new Set<string>();
  env.forEach((kv, i) => {
    const { key, value } = parseEnv(kv);
    userKeys.add(key);
    rows.push({ key, value, type: 'user', removable: true, idx: i });
  });

  const easyKeys = new Set<string>();
  for (const kv of readonlyEnv) {
    const { key, value } = parseEnv(kv);
    if (userKeys.has(key)) continue;
    easyKeys.add(key);
    rows.push({ key, value, type: 'easytidy', removable: false });
  }

  if (effectiveEnv) {
    for (const kv of effectiveEnv) {
      const { key, value } = parseEnv(kv);
      if (userKeys.has(key) || easyKeys.has(key)) continue;
      let type: EnvRowType;
      if (key.startsWith(EASYTIDY_SYSTEM_ENV_PREFIX)) type = 'easytidy';
      else if (PODMAN_DEFAULT_ENV_KEYS.has(key)) type = 'podman';
      else type = 'image';
      if (type === 'easytidy') easyKeys.add(key);
      rows.push({ key, value, type, removable: false });
    }
  }

  return rows.sort((a, b) => {
    const d = ENV_TYPE_META[a.type].order - ENV_TYPE_META[b.type].order;
    return d !== 0 ? d : a.key.localeCompare(b.key);
  });
}

export function EnvPane({ env, readonlyEnv = [], effectiveEnv, onAdd, onRemove }: EnvPaneProps) {
  const { message } = AntApp.useApp();
  const [newEnv, setNewEnv] = useState({ key: '', value: '' });

  const rows = buildEnvRows(env, readonlyEnv, effectiveEnv);

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

  const columns: TableProps<EnvRow>['columns'] = [
    {
      title: '变量名',
      dataIndex: 'key',
      key: 'key',
      ellipsis: true,
      render: (v: string, rec: EnvRow) => (
        <span className="env-key-cell">
          {rec.type !== 'user' && <LockOutlined className="readonly-badge-icon" />}
          {v}
        </span>
      ),
    },
    {
      title: '值',
      dataIndex: 'value',
      key: 'value',
      ellipsis: true,
      render: (v: string) => <Typography.Text>{v}</Typography.Text>,
    },
    {
      title: '类型',
      dataIndex: 'type',
      key: 'type',
      width: 130,
      render: (t: EnvRowType) => <span className={ENV_TYPE_META[t].cls}>{ENV_TYPE_META[t].label}</span>,
    },
    {
      title: '操作',
      key: 'actions',
      width: 70,
      render: (_: unknown, rec: EnvRow) =>
        rec.removable ? (
          <Button
            type="text"
            danger
            size="small"
            icon={<DeleteOutlined />}
            onClick={() => onRemove(rec.idx!)}
          />
        ) : null,
    },
  ];

  return (
    <div className="config-pane">
      <Table
        size="small"
        rowKey={(r) => r.key}
        columns={columns}
        dataSource={rows}
        pagination={false}
        locale={{ emptyText: <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="暂无环境变量" /> }}
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
        用户自定义行可增删，随「保存并重启」生效；EasyTidy 注入（EASYTIDY_USER_* 与 GUI/GPU 透传）、
        Podman 注入与镜像默认由引擎/运行时带入，只读。
      </span>
    </div>
  );
}
