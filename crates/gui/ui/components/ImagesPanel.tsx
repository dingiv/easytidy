// 镜像管理面板（主 GUI）：本地镜像列表 / 拉取 / 删除。
//
// 拉取是显式动作（创建容器不自动拉取）；删除支持 force（被容器引用时）。

import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { App as AntApp, Button, Input, Modal, Table, Tag } from 'antd';
import { CloudDownloadOutlined, DeleteOutlined, ReloadOutlined } from '@ant-design/icons';
import type { ImageSummary } from '../types';

/** 字节 → 人类可读 */
function fmtSize(bytes: number): string {
  const units = ['B', 'KB', 'MB', 'GB'];
  let v = bytes;
  let u = 0;
  while (v >= 1024 && u < units.length - 1) {
    v /= 1024;
    u += 1;
  }
  return `${v.toFixed(v >= 100 || u === 0 ? 0 : 1)} ${units[u]}`;
}

/** unix 秒 → 本地日期 */
function fmtDate(secs: number): string {
  if (!secs) return '-';
  return new Date(secs * 1000).toLocaleDateString();
}

export function ImagesPanel() {
  const { message, modal } = AntApp.useApp();
  const [images, setImages] = useState<ImageSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [pullOpen, setPullOpen] = useState(false);
  const [pullName, setPullName] = useState('');
  const [pulling, setPulling] = useState(false);

  const load = async () => {
    setLoading(true);
    try {
      setImages(await invoke<ImageSummary[]>('images_list'));
    } catch (err: any) {
      message.error(err?.message || '读取镜像列表失败');
      console.error('images_list failed:', err);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    load();
  }, []);

  const handlePull = async () => {
    if (!pullName.trim()) {
      message.warning('请输入镜像名');
      return;
    }
    setPulling(true);
    try {
      await invoke('image_pull', { image: pullName.trim() });
      message.success(`镜像已拉取：${pullName.trim()}`);
      setPullOpen(false);
      setPullName('');
      await load();
    } catch (err: any) {
      // 拉取失败原因多样（tag 不存在/网络/权限），Modal 展示完整错误
      const msg = typeof err === 'string' ? err : err?.message || JSON.stringify(err);
      console.error('image_pull failed:', err);
      modal.error({
        title: `拉取镜像失败：${pullName.trim()}`,
        width: 620,
        content: <pre className="error-detail">{msg}</pre>,
        okText: '知道了',
      });
    } finally {
      setPulling(false);
    }
  };

  const handleRemove = (img: ImageSummary) => {
    const name = img.repo_tags[0] ?? img.id;
    modal.confirm({
      title: `删除镜像 ${name}?`,
      content: '被容器引用时需强制删除（force）。',
      okText: '删除',
      okButtonProps: { danger: true },
      cancelText: '取消',
      onOk: async () => {
        try {
          // 先常规删除；被引用时报错再 force（一次交互完成两种语义）
          try {
            await invoke('image_remove', { image: name, force: false });
          } catch {
            await invoke('image_remove', { image: name, force: true });
          }
          message.success(`已删除：${name}`);
          await load();
        } catch (err: any) {
          message.error(err?.message || '删除失败');
          console.error('image_remove failed:', err);
        }
      },
    });
  };

  return (
    <div className="images-panel">
      <div className="panel-header">
        <h2>镜像管理</h2>
        <div className="header-actions">
          <Button icon={<ReloadOutlined />} onClick={load} loading={loading}>
            刷新
          </Button>
          <Button type="primary" icon={<CloudDownloadOutlined />} onClick={() => setPullOpen(true)}>
            拉取镜像
          </Button>
        </div>
      </div>

      <Table<ImageSummary>
        rowKey={(r) => r.id}
        dataSource={images}
        loading={loading}
        size="small"
        pagination={false}
        columns={[
          {
            title: '镜像',
            dataIndex: 'repo_tags',
            render: (tags: string[], record) =>
              tags.length > 0 ? (
                tags.map((t) => <div key={t} className="image-tag">{t}</div>)
              ) : (
                <Tag color="orange">&lt;none&gt;（悬空 {record.id}）</Tag>
              ),
          },
          { title: 'ID', dataIndex: 'id', width: 110, render: (id: string) => <code>{id}</code> },
          { title: '大小', dataIndex: 'size', width: 100, render: (s: number) => fmtSize(s) },
          { title: '创建时间', dataIndex: 'created', width: 110, render: (c: number) => fmtDate(c) },
          {
            title: '操作',
            width: 80,
            render: (_, record) => (
              <Button
                size="small"
                danger
                icon={<DeleteOutlined />}
                onClick={() => handleRemove(record)}
                title="删除镜像"
              />
            ),
          },
        ]}
      />

      <Modal
        title="拉取镜像"
        open={pullOpen}
        onCancel={() => !pulling && setPullOpen(false)}
        onOk={handlePull}
        okText="拉取"
        cancelText="取消"
        confirmLoading={pulling}
        okButtonProps={{ disabled: !pullName.trim() }}
      >
        <p className="dialog-hint">拉取可能需要几分钟（取决于镜像大小与网络）。</p>
        <Input
          placeholder="docker.io/library/ubuntu:24.04"
          value={pullName}
          onChange={(e) => setPullName(e.target.value)}
          onPressEnter={handlePull}
          autoFocus
        />
      </Modal>
    </div>
  );
}
