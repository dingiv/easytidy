// 数据卷管理面板（主 GUI）：宿主全量卷列表 / 创建 / 删除。
//
// 全量列出（接管策略定案）：不分谁建的——外部工具建的卷（如 devcontainer 的
// vscode）也列出，`labels.manager === 'easytidy'` 打标为 easytidy 创建。
// 删除挂载中的卷 podman 返回 409（is in use）→ 错误透传展示，不静默。

import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { errMsg } from '../lib/errors';
import { App as AntApp, Button, Input, Popconfirm, Space, Table, Tag, Tooltip } from 'antd';
import { DeleteOutlined, PlusOutlined, ReloadOutlined } from '@ant-design/icons';
import type { VolumeInfo } from '../types';

/** RFC3339 → 本地日期时间（解析失败 → '-'） */
function fmtTime(s: string): string {
  if (!s) return '-';
  const d = new Date(s);
  return Number.isNaN(d.getTime()) ? s : d.toLocaleString();
}

export function VolumesPanel() {
  const { message } = AntApp.useApp();
  const [volumes, setVolumes] = useState<VolumeInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [newName, setNewName] = useState('');
  const [creating, setCreating] = useState(false);
  /** 删除中的卷名（行内 loading） */
  const [removing, setRemoving] = useState<string | null>(null);

  const load = async () => {
    setLoading(true);
    try {
      setVolumes(await invoke<VolumeInfo[]>('volume_list'));
    } catch (err: any) {
      message.error(errMsg(err, '读取数据卷列表失败'));
      console.error('volume_list failed:', err);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    load();
  }, []);

  const handleCreate = async () => {
    const name = newName.trim();
    if (!name) {
      message.warning('请输入卷名');
      return;
    }
    setCreating(true);
    try {
      await invoke('volume_create', { name });
      message.success(`数据卷已创建：${name}`);
      setNewName('');
      await load();
    } catch (err: any) {
      // 同名已存在 → podman 409 原文透传；卷名非法同理
      message.error(errMsg(err, `创建数据卷 ${name} 失败`));
      console.error('volume_create failed:', err);
    } finally {
      setCreating(false);
    }
  };

  const handleRemove = async (name: string) => {
    setRemoving(name);
    try {
      await invoke('volume_rm', { name });
      message.success(`数据卷已删除：${name}`);
      await load();
    } catch (err: any) {
      // 挂载中 → podman 409 "is in use"（错误原文展示，用户先卸载/删容器）
      message.error(errMsg(err, `删除数据卷 ${name} 失败`));
      console.error('volume_rm failed:', err);
    } finally {
      setRemoving(null);
    }
  };

  const columns = [
    {
      title: '卷名',
      dataIndex: 'name',
      key: 'name',
      ellipsis: true,
      render: (v: string, rec: VolumeInfo) => (
        <span>
          {v}
          {rec.labels?.manager === 'easytidy' ? (
            <Tag color="blue" style={{ marginLeft: 6 }}>
              easytidy
            </Tag>
          ) : (
            <Tag style={{ marginLeft: 6 }}>外部</Tag>
          )}
        </span>
      ),
    },
    {
      title: '状态',
      key: 'status',
      width: 130,
      render: (_: unknown, rec: VolumeInfo) =>
        rec.mount_count > 0 ? (
          <Tag color="blue">使用中 ×{rec.mount_count}</Tag>
        ) : (
          <Tag color="default">空闲</Tag>
        ),
    },
    {
      title: '创建时间',
      dataIndex: 'created_at',
      key: 'created_at',
      width: 170,
      render: (v: string) => fmtTime(v),
    },
    {
      title: '宿主路径',
      dataIndex: 'mountpoint',
      key: 'mountpoint',
      ellipsis: true,
      render: (v: string) => (
        <Tooltip title={v}>
          <span className="path-cell">{v}</span>
        </Tooltip>
      ),
    },
    {
      title: '操作',
      key: 'actions',
      width: 90,
      render: (_: unknown, rec: VolumeInfo) => (
        <Popconfirm
          title={`删除数据卷 ${rec.name}？`}
          description={
            rec.mount_count > 0
              ? '该卷正在被容器使用——删除会被 podman 拒绝（409）'
              : '卷内数据将一并删除，不可恢复'
          }
          okText="删除"
          okButtonProps={{ danger: true }}
          onConfirm={() => handleRemove(rec.name)}
        >
          <Button
            type="text"
            danger
            size="small"
            icon={<DeleteOutlined />}
            loading={removing === rec.name}
          />
        </Popconfirm>
      ),
    },
  ];

  return (
    <div className="config-pane">
      <div className="add-row">
        <Input
          placeholder="新卷名，如 app-data"
          value={newName}
          onChange={(e) => setNewName(e.target.value)}
          onPressEnter={handleCreate}
          style={{ flex: 1, maxWidth: 320 }}
        />
        <Button
          type="primary"
          icon={<PlusOutlined />}
          loading={creating}
          onClick={handleCreate}
        >
          创建
        </Button>
        <Button icon={<ReloadOutlined />} onClick={load}>
          刷新
        </Button>
        <Space size={4} wrap>
          <Tag color="blue">easytidy</Tag>
          <span className="section-hint">easytidy 创建</span>
          <Tag>外部</Tag>
          <span className="section-hint">其他工具创建（全量列出，也可挂/删）</span>
        </Space>
      </div>

      <Table
        size="small"
        rowKey="name"
        loading={loading}
        columns={columns}
        dataSource={volumes}
        pagination={false}
        locale={{
          emptyText: '暂无数据卷（在容器配置的「数据卷」区挂载，创建容器时自动建）',
        }}
        footer={() => (
          <span className="section-hint">
            数据卷独立于容器：删容器/重建/快照都不丢数据。挂载中的卷无法删除（podman
            409）——先移除容器挂载或删除容器。
          </span>
        )}
      />
    </div>
  );
}
