// 数据卷（named volume）面板：表格 + 添加行（卷名下拉=宿主已有卷 + 容器路径 + 只读）。
//
// 与 MountsPane 同构（表格 + 行内删除 + 底部添加行），差异：
// - 卷名 AutoComplete 数据源 = volume_list（全量：easytidy 建的带
//   manager label、外部卷也列出）；**未列出的名字 = 新卷**，创建容器时由
//   引擎幂等创建（volume_ensure：IgnoreIfExists + label）
// - 宿主路径这里不存在（卷是 podman 管的独立存储），只需容器路径
// - 挂载点与 bind 冲突在添加时即拦截（podman 同容器路径不允许 bind+volume 共存）

import { useEffect, useState } from 'react';
import { App as AntApp, AutoComplete, Button, Empty, Input, Switch, Table, Tag, Tooltip } from 'antd';
import type { TableProps } from 'antd';
import { DeleteOutlined, PlusOutlined, ReloadOutlined } from '@ant-design/icons';
import { invoke } from '@tauri-apps/api/core';
import type { VolumeInfo, VolumeMount } from '../../types';
import { volumeEffectiveTag, type EffectiveTag } from './utils';

interface VolumesPaneProps {
  volumes: VolumeMount[];
  /** 已占用的容器挂载点（bind 的 container_path——同路径不能重复挂） */
  mountTargets?: string[];
  /** 当前生效数据卷（podman inspect 实时投影；edit 模式才有，null=容器未创建）。
   *  与配置对照：一致 / 已变更·未应用 / 配置外·未应用。 */
  effectiveVolumes?: VolumeMount[] | null;
  onAdd(v: VolumeMount): void;
  onRemove(idx: number): void;
}

export function VolumesPane({
  volumes,
  mountTargets = [],
  effectiveVolumes = null,
  onAdd,
  onRemove,
}: VolumesPaneProps) {
  const { message } = AntApp.useApp();
  const [newVolume, setNewVolume] = useState({
    name: '',
    container_path: '',
    read_only: false,
  });
  /** 宿主卷列表（下拉 + 「新卷」提示判定） */
  const [existing, setExisting] = useState<VolumeInfo[]>([]);
  const [opts, setOpts] = useState<{ value: string; label: React.ReactNode }[]>([]);

  const loadVolumes = () => {
    invoke<VolumeInfo[]>('volume_list')
      .then(setExisting)
      .catch((err) => console.error('volume_list failed:', err));
  };
  useEffect(loadVolumes, []);

  /** 卷名输入 → 下拉：已有卷（带使用中/来源标）+ 当前输入若不存在则补一条「将创建」 */
  useEffect(() => {
    const q = newVolume.name.trim().toLowerCase();
    const matched = existing.filter((v) => !q || v.name.toLowerCase().includes(q));
    const items = matched.slice(0, 20).map((v) => ({
      value: v.name,
      label: (
        <span>
          {v.name}
          {v.mount_count > 0 && (
            <Tag color="blue" style={{ marginLeft: 6 }}>
              使用中 ×{v.mount_count}
            </Tag>
          )}
          {v.labels?.manager === 'easytidy' ? (
            <Tag style={{ marginLeft: 6 }}>easytidy</Tag>
          ) : (
            <Tag color="default" style={{ marginLeft: 6 }}>
              外部
            </Tag>
          )}
        </span>
      ),
    }));
    if (
      newVolume.name.trim() &&
      !existing.some((v) => v.name === newVolume.name.trim())
    ) {
      items.push({
        value: newVolume.name.trim(),
        label: (
          <span>
            {newVolume.name.trim()}
            <Tag color="green" style={{ marginLeft: 6 }}>
              新卷（创建容器时自动创建）
            </Tag>
          </span>
        ),
      });
    }
    setOpts(items);
  }, [newVolume.name, existing]);

  const addVolume = () => {
    const name = newVolume.name.trim();
    const target = newVolume.container_path.trim();
    if (!name) {
      message.error('卷名不能为空');
      return;
    }
    if (!target) {
      message.error('容器路径不能为空');
      return;
    }
    if (!target.startsWith('/')) {
      message.error('容器路径须为绝对路径（以 / 开头）');
      return;
    }
    if (mountTargets.includes(target)) {
      message.error(`挂载点 ${target} 已被现有挂载占用（bind 与卷不能共存）`);
      return;
    }
    if (volumes.some((v) => v.name === name && v.container_path === target)) {
      message.warning('该卷已挂到此路径');
      return;
    }
    onAdd({ name, container_path: target, read_only: newVolume.read_only });
    setNewVolume({ name: '', container_path: '', read_only: false });
  };

  type Row = VolumeMount & {
    idx: number;
    kind?: 'user' | 'eff';
    effTag?: EffectiveTag;
  };
  const columns: TableProps<Row>['columns'] = [
    {
      title: '卷名',
      dataIndex: 'name',
      key: 'name',
      ellipsis: true,
      render: (v: string, rec: Row) => {
        if (rec.kind === 'eff') {
          return (
            <span>
              {v}{' '}
              <Tooltip title="当前生效（podman inspect 实时投影；与上方配置对照）">
                <span>
                  {rec.effTag === 'same' && <Tag color="success">当前生效·一致</Tag>}
                  {rec.effTag === 'changed' && (
                    <Tag color="warning">当前生效·已变更未应用</Tag>
                  )}
                  {rec.effTag === 'extra' && (
                    <Tag color="warning">当前生效·配置外</Tag>
                  )}
                </span>
              </Tooltip>
            </span>
          );
        }
        const info = existing.find((x) => x.name === v);
        return (
          <span>
            {v}
            {info && info.mount_count > 0 && (
              <Tag color="blue" style={{ marginLeft: 6 }}>
                使用中
              </Tag>
            )}
            {info && info.labels?.manager !== 'easytidy' && (
              <Tag style={{ marginLeft: 6 }}>外部</Tag>
            )}
            {!info && (
              <Tag color="green" style={{ marginLeft: 6 }}>
                新卷
              </Tag>
            )}
          </span>
        );
      },
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
      render: (_: unknown, rec: Row) =>
        rec.kind === 'user' ? (
          <Button
            type="text"
            danger
            size="small"
            icon={<DeleteOutlined />}
            onClick={() => onRemove(rec.idx)}
          />
        ) : null,
    },
  ];

  const rows: Row[] = [
    ...volumes.map((v, idx) => ({ ...v, idx, kind: 'user' as const })),
    ...(effectiveVolumes ?? []).map((v) => ({
      ...v,
      idx: -1,
      kind: 'eff' as const,
      effTag: volumeEffectiveTag(v, volumes),
    })),
  ];

  return (
    <div className="config-pane">
      <Table
        size="small"
        rowKey={(rec: Row) => `${rec.kind ?? 'user'}-${rec.name}-${rec.container_path}`}
        columns={columns}
        dataSource={rows}
        pagination={false}
        locale={{
          emptyText: (
            <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="暂无数据卷" />
          ),
        }}
        footer={() => (
          <span className="section-hint">
            卷是 podman 独立存储：删容器/重建/快照都不丢数据（bind 则依赖宿主路径）。
            下拉里没有的卷名 = 新卷，创建容器时自动创建（打 manager=easytidy 标签）；
            删除卷到「数据卷」面板操作（挂载中会被 podman 拒绝）。
            {effectiveVolumes &&
              ' 「当前生效」= podman inspect 实时投影；标「未应用」= 配置已修改，重建应用后才生效。'}
          </span>
        )}
      />

      <div className="add-row">
        <AutoComplete
          value={newVolume.name}
          onChange={(v) => setNewVolume((s) => ({ ...s, name: String(v ?? '') }))}
          options={opts}
          style={{ flex: 1, minWidth: 200 }}
          popupMatchSelectWidth={false}
          allowClear
          placeholder="卷名，如 app-data（不存在则创建）"
        >
          <Input
            onPressEnter={addVolume}
            onBlur={() => loadVolumes()}
          />
        </AutoComplete>
        <Input
          placeholder="容器路径，如 /srv/data"
          value={newVolume.container_path}
          onChange={(e) =>
            setNewVolume((s) => ({ ...s, container_path: e.target.value }))
          }
          onPressEnter={addVolume}
        />
        <Switch
          checked={newVolume.read_only}
          onChange={(v) => setNewVolume((s) => ({ ...s, read_only: v }))}
          checkedChildren="只读"
          unCheckedChildren="读写"
        />
        <Button type="primary" icon={<PlusOutlined />} onClick={addVolume}>
          添加
        </Button>
        <Button
          icon={<ReloadOutlined />}
          onClick={loadVolumes}
          title="刷新宿主卷列表"
        />
      </div>
    </div>
  );
}
