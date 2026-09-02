// 容器状态面板：Worker 启动时检测容器未运行（启动失败 / 已停止 / 已丢失）时呈现。
//
// 信息来源：后端 `container_failure_info` 命令（podman inspect + bollard logs）。
// 展示：
// - 容器名 + 状态 badge（color-coded）
// - 退出码（exited 时）
// - podman 上报的 error（OCI hook / device 不可用 / 镜像损坏等）
// - 容器日志（stdout + stderr 合并；有损 UTF-8；按容器状态取最后 N 行）
// - 操作：刷新 / 复制日志 / 关闭 Worker
//
// 此面板**不依赖** server socket——容器未运行时也能展示（Worker 进不去终端
// 但仍能给用户看到为什么进不去）。

import { useCallback, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Alert, Button, Empty, Space, Spin, Tag, Typography, message } from 'antd';
import {
  CopyOutlined,
  ReloadOutlined,
  PoweroffOutlined,
} from '@ant-design/icons';
import type { ContainerFailureInfo } from '../types';

interface ContainerStatusPaneProps {
  containerName: string;
  initialInfo: ContainerFailureInfo | null;
}

const STATUS_COLOR: Record<string, string> = {
  running: 'green',
  exited: 'red',
  created: 'orange',
  configured: 'orange',
  stopped: 'default',
  missing: 'default',
  unknown: 'default',
};

const STATUS_LABEL: Record<string, string> = {
  running: '运行中',
  exited: '已退出',
  created: '已创建（未启动）',
  configured: '已配置（未启动）',
  stopped: '已停止',
  missing: '容器丢失',
  unknown: '未知',
};

export function ContainerStatusPane({ containerName, initialInfo }: ContainerStatusPaneProps) {
  const [info, setInfo] = useState<ContainerFailureInfo | null>(initialInfo);
  const [refreshing, setRefreshing] = useState(false);

  const refresh = useCallback(async () => {
    setRefreshing(true);
    try {
      const fresh = await invoke<ContainerFailureInfo>('container_failure_info', {
        name: containerName,
        tailLines: 200,
      });
      setInfo(fresh);
    } catch (err) {
      console.error('container_failure_info 刷新失败:', err);
      message.error(`刷新失败：${(err as any)?.message ?? err}`);
    } finally {
      setRefreshing(false);
    }
  }, [containerName]);

  const copyLogs = useCallback(async () => {
    if (!info?.logs) {
      message.info('暂无日志可复制');
      return;
    }
    try {
      await navigator.clipboard.writeText(info.logs);
      message.success('日志已复制');
    } catch (err) {
      console.error('clipboard.writeText 失败:', err);
      message.error(`复制失败：${(err as any)?.message ?? err}`);
    }
  }, [info?.logs]);

  const closeWorker = useCallback(async () => {
    try {
      const { getCurrentWindow } = await import('@tauri-apps/api/window');
      await getCurrentWindow().close();
    } catch (err) {
      console.error('关闭窗口失败:', err);
    }
  }, []);

  if (!info) {
    return (
      <div className="container-status-pane">
        <Spin tip="加载容器状态中…" />
      </div>
    );
  }

  const statusKey = info.status.toLowerCase();
  const statusColor = STATUS_COLOR[statusKey] ?? 'default';
  const statusLabel = STATUS_LABEL[statusKey] ?? info.status;

  return (
    <div className="container-status-pane">
      <div className="container-status-header">
        <Typography.Title level={4} style={{ margin: 0 }}>
          {containerName}
          <Tag color={statusColor} style={{ marginLeft: 12, fontSize: '0.75rem' }}>
            {statusLabel}
          </Tag>
        </Typography.Title>
        <Space>
          <Button
            icon={<ReloadOutlined />}
            onClick={refresh}
            loading={refreshing}
            size="small"
          >
            刷新
          </Button>
          <Button
            icon={<CopyOutlined />}
            onClick={copyLogs}
            size="small"
            disabled={!info.logs}
          >
            复制日志
          </Button>
          <Button
            icon={<PoweroffOutlined />}
            onClick={closeWorker}
            size="small"
          >
            关闭窗口
          </Button>
        </Space>
      </div>

      {!info.exists && (
        <Alert
          type="warning"
          showIcon
          message="容器不存在"
          description={info.error ?? `容器 ${containerName} 在 podman 中不存在（可能已被外部清理或创建失败后未保留）`}
          style={{ marginBottom: '1rem' }}
        />
      )}

      {info.running && (
        <Alert
          type="success"
          showIcon
          message="容器运行中"
          description="此面板为状态/日志查阅视图；可通过工具栏打开终端 / Passthrough / 容器配置面板进行交互。"
          style={{ marginBottom: '1rem' }}
        />
      )}

      {!info.running && info.error && (
        <Alert
          type="error"
          showIcon
          message="启动失败"
          description={info.error}
          style={{ marginBottom: '1rem' }}
        />
      )}

      {!info.running && !info.error && info.exists && (
        <Alert
          type="info"
          showIcon
          message="容器未运行"
          description={`当前状态：${statusLabel}${
            info.exitCode != null ? `（退出码 ${info.exitCode}）` : ''
          }。如需查看容器内应用启动细节，请查看下方日志。`}
          style={{ marginBottom: '1rem' }}
        />
      )}

      <Typography.Text strong style={{ display: 'block', marginBottom: '0.5rem' }}>
        容器日志
        {info.logs && (
          <Typography.Text type="secondary" style={{ marginLeft: 8, fontWeight: 'normal' }}>
            （{info.logs.split('\n').length} 行）
          </Typography.Text>
        )}
      </Typography.Text>
      {info.logs ? (
        <pre className="container-status-logs">{info.logs}</pre>
      ) : (
        <Empty
          description={info.exists ? '该容器暂无日志输出' : '容器不存在，无日志'}
          image={Empty.PRESENTED_IMAGE_SIMPLE}
        />
      )}
    </div>
  );
}
