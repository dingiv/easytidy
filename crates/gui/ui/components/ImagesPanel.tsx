// 镜像管理面板（主 GUI）：本地镜像列表 / 拉取 / 删除。
//
// 拉取是显式动作（创建容器不自动拉取）；删除支持 force（被容器引用时）。
// 批量管理：行首多选框（含表头全选）+「删除选中」——逐个删除、被引用时
// 自动 force，失败逐个汇总展示（不中断批次）。

import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { errMsg } from '../lib/errors';
import { App as AntApp, Button, Input, Modal, Table, Tag } from 'antd';
import { CloudDownloadOutlined, DeleteOutlined, ReloadOutlined } from '@ant-design/icons';
import type { ImageSummary } from '../types';

/** 字节 → 人类可读 */
function fmtSize(bytes: number): string {
  const units = ['B', 'KB', 'MB', 'GB'];
  let v = bytes;
  let u = 0;
  while (v >= 1024 && v < 1e12) {
    v /= 1024;
    u += 1;
  }
  return `${v.toFixed(v >= 100 || u === 0 ? 0 : 1)} ${units[u] ?? 'TB'}`;
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
  // 批量管理：选中镜像的 rowKey（短 ID）集合；刷新后失效，清空
  const [selectedKeys, setSelectedKeys] = useState<React.Key[]>([]);
  const [batchRemoving, setBatchRemoving] = useState(false);

  const load = async () => {
    setLoading(true);
    try {
      setImages(await invoke<ImageSummary[]>('images_list'));
      setSelectedKeys([]);
    } catch (err: any) {
      message.error(errMsg(err, '读取镜像列表失败'));
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
      const msg = errMsg(err);
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
          message.error(errMsg(err, '删除失败'));
          console.error('image_remove failed:', err);
        }
      },
    });
  };

  /** 批量删除选中镜像：逐个删除（被引用自动 force），失败汇总不中断批次 */
  const handleBatchRemove = () => {
    const selected = images.filter((img) => selectedKeys.includes(img.id));
    if (selected.length === 0) return;
    const totalSize = selected.reduce((acc, img) => acc + img.size, 0);
    modal.confirm({
      title: `删除选中的 ${selected.length} 个镜像？`,
      content: (
        <div>
          <p>共约 {fmtSize(totalSize)}。被容器引用的将强制删除（force）。</p>
          <p style={{ paddingLeft: 12, maxHeight: 180, overflow: 'auto' }}>
            {selected.map((img) => (
              <div key={img.id}>
                {img.repo_tags[0] ?? `<none>（悬空 ${img.id}）`}
              </div>
            ))}
          </p>
        </div>
      ),
      okText: '全部删除',
      okButtonProps: { danger: true },
      cancelText: '取消',
      onOk: async () => {
        setBatchRemoving(true);
        const failures: string[] = [];
        let done = 0;
        try {
          for (const img of selected) {
            const name = img.repo_tags[0] ?? img.id;
            try {
              try {
                await invoke('image_remove', { image: name, force: false });
              } catch {
                await invoke('image_remove', { image: name, force: true });
              }
              done += 1;
            } catch (err: any) {
              failures.push(`${name}：${errMsg(err)}`);
            }
          }
          if (failures.length > 0) {
            modal.error({
              title: `批量删除完成：成功 ${done}，失败 ${failures.length}`,
              width: 620,
              content: <pre className="error-detail">{failures.join('\n\n')}</pre>,
              okText: '知道了',
            });
          } else {
            message.success(`已删除 ${done} 个镜像`);
          }
          await load();
        } finally {
          setBatchRemoving(false);
        }
      },
    });
  };

  return (
    <div className="images-panel">
      <div className="panel-header">
        <h2>镜像管理</h2>
        <div className="header-actions">
          {selectedKeys.length > 0 && (
            <Button danger icon={<DeleteOutlined />} loading={batchRemoving} onClick={handleBatchRemove}>
              删除选中（{selectedKeys.length}）
            </Button>
          )}
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
        rowSelection={{
          selectedRowKeys: selectedKeys,
          onChange: (keys) => setSelectedKeys(keys),
        }}
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
