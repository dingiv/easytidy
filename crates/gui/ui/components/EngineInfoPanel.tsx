// 环境信息面板（Master GUI）：下层引擎（podman）只读快照。
//
// 数据源：`engine_info`（podman `/info` 只读端点，一次调用）。
// 三段：引擎（版本 / rootless / 默认运行时 / cgroup）、
// 存储（驱动 / 驱动细节 / 根目录）、宿主（系统 / 内核 / 架构 / CPU / 内存）。
// 纯只读，无操作按钮——修复类入口（fuse-overlayfs 一键配置等）属于
// 道路二 doctor 设计稿（docs/18），届时在本面板上生长。

import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { errMsg } from '../lib/errors';
import { App as AntApp, Button, Descriptions, Spin, Tag } from 'antd';
import { ReloadOutlined } from '@ant-design/icons';
import type { EngineInfo } from '../types';

/** 字节 → 人类可读 */
function fmtBytes(n: number): string {
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let v = n;
  let u = 0;
  while (v >= 1024 && u < units.length - 1) {
    v /= 1024;
    u += 1;
  }
  return `${v.toFixed(v >= 100 || u === 0 ? 0 : 1)} ${units[u]}`;
}

/** 缺字段显示「-」（不同 podman 版本 /info 填充程度不一） */
function fmt(s?: string | null): string {
  return s || '-';
}

export function EngineInfoPanel() {
  const { message } = AntApp.useApp();
  const [info, setInfo] = useState<EngineInfo | null>(null);
  const [loading, setLoading] = useState(true);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      setInfo(await invoke<EngineInfo>('engine_info'));
    } catch (err: any) {
      message.error(errMsg(err, '获取环境信息失败'));
      console.error('engine_info failed:', err);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  if (loading && !info) {
    return (
      <div
        className="engine-info-panel"
        style={{ textAlign: 'center', paddingTop: '3rem' }}
      >
        <Spin />
      </div>
    );
  }

  if (!info) return <div className="engine-info-panel" />;

  return (
    <div className="engine-info-panel">
      <div className="panel-header">
        <h2>环境信息</h2>
        <div className="header-actions">
          <Button icon={<ReloadOutlined />} loading={loading} onClick={load}>
            刷新
          </Button>
        </div>
      </div>

      <Descriptions
        title="引擎"
        bordered
        size="small"
        column={2}
        style={{ marginBottom: 16 }}
      >
        <Descriptions.Item label="引擎">podman {fmt(info.version)}</Descriptions.Item>
        <Descriptions.Item label="rootless">
          <Tag color={info.rootless ? 'blue' : 'default'}>
            {info.rootless ? '是' : '否'}
          </Tag>
        </Descriptions.Item>
        <Descriptions.Item label="默认运行时">
          {fmt(info.default_runtime)}
        </Descriptions.Item>
        <Descriptions.Item label="cgroup">
          {fmt(info.cgroup_driver)}
          {info.cgroup_version ? `（v${info.cgroup_version}）` : ''}
        </Descriptions.Item>
      </Descriptions>

      <Descriptions
        title="存储"
        bordered
        size="small"
        column={2}
        style={{ marginBottom: 16 }}
      >
        <Descriptions.Item label="存储驱动">
          <Tag color="green">{fmt(info.storage_driver)}</Tag>
        </Descriptions.Item>
        <Descriptions.Item label="存储根目录">
          <span style={{ fontFamily: 'monospace', fontSize: '0.8em' }}>
            {fmt(info.storage_root)}
          </span>
        </Descriptions.Item>
        {info.storage_driver_status.map(([k, v]) => (
          <Descriptions.Item key={k} label={k}>
            {v}
          </Descriptions.Item>
        ))}
      </Descriptions>

      <Descriptions title="宿主" bordered size="small" column={2}>
        <Descriptions.Item label="操作系统">{fmt(info.os)}</Descriptions.Item>
        <Descriptions.Item label="内核版本">{fmt(info.kernel_version)}</Descriptions.Item>
        <Descriptions.Item label="架构">{fmt(info.arch)}</Descriptions.Item>
        <Descriptions.Item label="CPU / 内存">
          {info.ncpu != null ? `${info.ncpu} 核` : '-'}
          {' / '}
          {info.mem_total != null ? fmtBytes(info.mem_total) : '-'}
        </Descriptions.Item>
      </Descriptions>
    </div>
  );
}
