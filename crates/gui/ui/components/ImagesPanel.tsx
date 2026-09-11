// 镜像管理面板（主 GUI）：本地镜像列表 / 拉取 / 删除。
//
// 拉取是显式动作（创建容器不自动拉取）。删除带「防呆」：删除前先查询镜像占用
// （images_used_by），被容器引用的镜像不可删除——单个删除直接提示「无法删除」
// 并列出占用容器；批量删除只删未被占用的，跳过被占用的并汇总提示。

import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { errMsg } from '../lib/errors';
import { App as AntApp, Button, Input, Modal, Table, Tag } from 'antd';
import { CloudDownloadOutlined, ClearOutlined, DeleteOutlined, ReloadOutlined } from '@ant-design/icons';
import type { ImageSummary, RebuildCleanupResult, RebuildScanResult } from '../types';

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
  // 智能清理（rebuild 冗余镜像）：扫描中 / 预览结果 / 确认弹窗 / 执行中
  const [scanning, setScanning] = useState(false);
  const [scanResult, setScanResult] = useState<RebuildScanResult | null>(null);
  const [cleanupOpen, setCleanupOpen] = useState(false);
  const [cleanupRunning, setCleanupRunning] = useState(false);

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

  /** 查询一批镜像的占用情况：image 名 -> 占用容器名列表（空 = 可删除）。
   *  查询失败时返回空表（不阻断删除——podman 自身仍会保护被引用的镜像）。 */
  const queryUsage = async (names: string[]): Promise<Record<string, string[]>> => {
    try {
      return await invoke<Record<string, string[]>>('images_used_by', { images: names });
    } catch (err: any) {
      console.error('images_used_by failed:', err);
      return {};
    }
  };

  const handleRemove = async (img: ImageSummary) => {
    const name = img.repo_tags[0] ?? img.id;
    const usage = await queryUsage([name]);
    const usedBy = usage[name] ?? [];
    if (usedBy.length > 0) {
      modal.error({
        title: `无法删除镜像 ${name}`,
        width: 520,
        content: (
          <div>
            <p>该镜像正被以下容器使用，请先删除或迁移这些容器后再试：</p>
            <ul style={{ paddingLeft: 20, margin: '8px 0' }}>
              {usedBy.map((n) => (
                <li key={n}>{n}</li>
              ))}
            </ul>
          </div>
        ),
        okText: '知道了',
      });
      return;
    }
    modal.confirm({
      title: `删除镜像 ${name}?`,
      content: '删除后不可恢复。',
      okText: '删除',
      okButtonProps: { danger: true },
      cancelText: '取消',
      onOk: async () => {
        try {
          await invoke('image_remove', { image: name, force: false });
          message.success(`已删除：${name}`);
          await load();
        } catch (err: any) {
          message.error(errMsg(err, '删除失败'));
          console.error('image_remove failed:', err);
        }
      },
    });
  };

  /** 批量删除选中镜像：先查占用，只删未被占用的，跳过被占用的并汇总提示 */
  const handleBatchRemove = async () => {
    const selected = images.filter((img) => selectedKeys.includes(img.id));
    if (selected.length === 0) return;
    const names = selected.map((img) => img.repo_tags[0] ?? img.id);
    const usage = await queryUsage(names);

    const deletable = selected.filter((img) => (usage[img.repo_tags[0] ?? img.id] ?? []).length === 0);
    const inUse = selected.filter((img) => (usage[img.repo_tags[0] ?? img.id] ?? []).length > 0);

    // 全部被占用 → 直接提示，不进入删除确认
    if (deletable.length === 0) {
      modal.error({
        title: `无法删除选中的 ${selected.length} 个镜像`,
        width: 560,
        content: (
          <div>
            <p>这些镜像均被容器使用，无法删除：</p>
            <ul style={{ paddingLeft: 20, margin: '8px 0', maxHeight: 200, overflow: 'auto' }}>
              {inUse.map((img) => {
                const n = img.repo_tags[0] ?? img.id;
                return (
                  <li key={img.id}>
                    {n}（{usage[n]?.join('、')}）
                  </li>
                );
              })}
            </ul>
          </div>
        ),
        okText: '知道了',
      });
      return;
    }

    const deletableSize = deletable.reduce((acc, img) => acc + img.size, 0);
    modal.confirm({
      title: `删除选中的 ${deletable.length} 个镜像？`,
      content: (
        <div>
          {inUse.length > 0 && (
            <p>
              <b>{inUse.length} 个</b> 镜像被容器使用，将跳过不删除：
              {inUse.map((img) => img.repo_tags[0] ?? img.id).join('、')}
            </p>
          )}
          <p>可删除（共约 {fmtSize(deletableSize)}）：</p>
          <p style={{ paddingLeft: 12, maxHeight: 180, overflow: 'auto' }}>
            {deletable.map((img) => (
              <div key={img.id}>
                {img.repo_tags[0] ?? `<none>（悬空 ${img.id}）`}
              </div>
            ))}
          </p>
        </div>
      ),
      okText: `删除 ${deletable.length} 个`,
      okButtonProps: { danger: true },
      cancelText: '取消',
      onOk: async () => {
        setBatchRemoving(true);
        const failures: string[] = [];
        let done = 0;
        try {
          for (const img of deletable) {
            const name = img.repo_tags[0] ?? img.id;
            try {
              await invoke('image_remove', { image: name, force: false });
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
          } else if (inUse.length > 0) {
            message.success(`已删除 ${done} 个镜像；跳过 ${inUse.length} 个（被容器使用）`);
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

  /** 智能清理：扫描 rebuild 冗余镜像（每容器名留最新、跳过在用）→ 预览 Modal */
  const handleSmartCleanup = async () => {
    setScanning(true);
    try {
      const result = await invoke<RebuildScanResult>('rebuild_images_scan');
      setScanResult(result);
      setCleanupOpen(true);
    } catch (err: any) {
      message.error(errMsg(err, '智能清理扫描失败'));
      console.error('rebuild_images_scan failed:', err);
    } finally {
      setScanning(false);
    }
  };

  /** 确认删除预览中的冗余 rebuild 镜像（后端删除前会复查冗余 + 占用） */
  const handleConfirmCleanup = async () => {
    if (!scanResult) return;
    const tags = scanResult.candidates.map((c) => c.tag);
    if (tags.length === 0) {
      setCleanupOpen(false);
      return;
    }
    setCleanupRunning(true);
    try {
      const res = await invoke<RebuildCleanupResult>('rebuild_images_cleanup', { images: tags });
      setCleanupOpen(false);
      if (res.failures.length > 0) {
        modal.error({
          title: `智能清理完成：删除 ${res.deleted.length}，跳过 ${res.skipped.length}，失败 ${res.failures.length}`,
          width: 620,
          content: <pre className="error-detail">{res.failures.join('\n\n')}</pre>,
          okText: '知道了',
        });
      } else {
        const skipMsg = res.skipped.length > 0 ? `；跳过 ${res.skipped.length} 个（复查后已不冗余/在用）` : '';
        message.success(`已删除 ${res.deleted.length} 个冗余 rebuild 镜像，释放约 ${fmtSize(res.freed)}${skipMsg}`);
      }
      await load();
    } catch (err: any) {
      message.error(errMsg(err, '智能清理失败'));
      console.error('rebuild_images_cleanup failed:', err);
    } finally {
      setCleanupRunning(false);
    }
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
          <Button
            icon={<ClearOutlined />}
            loading={scanning}
            onClick={handleSmartCleanup}
            title="扫描并清理冗余 rebuild 镜像（每个容器名只留时间戳最新的一个）"
          >
            智能清理
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

      <Modal
        title="智能清理 rebuild 镜像"
        open={cleanupOpen}
        onCancel={() => !cleanupRunning && setCleanupOpen(false)}
        onOk={handleConfirmCleanup}
        okText={scanResult && scanResult.candidates.length > 0 ? `删除 ${scanResult.candidates.length} 个` : '关闭'}
        okButtonProps={{ danger: true, disabled: !scanResult || scanResult.candidates.length === 0 }}
        cancelText="取消"
        confirmLoading={cleanupRunning}
        width={640}
      >
        {scanResult && scanResult.candidates.length === 0 && (
          <p className="dialog-hint">
            没有可清理的冗余 rebuild 镜像（每个容器名都只保留了时间戳最新的一个）。
          </p>
        )}
        {scanResult && scanResult.candidates.length > 0 && (
          <div>
            <p>
              将删除 <b>{scanResult.candidates.length}</b> 个冗余 rebuild 镜像，共约{' '}
              <b>{fmtSize(scanResult.total_candidate_size)}</b>（每个容器名只保留时间戳最新的一个；正被容器使用的会自动跳过）。
            </p>
            <div style={{ maxHeight: 220, overflow: 'auto', margin: '8px 0' }}>
              {scanResult.candidates.map((c) => (
                <div
                  key={c.id}
                  style={{ display: 'flex', justifyContent: 'space-between', padding: '2px 0', gap: 12 }}
                >
                  <span style={{ wordBreak: 'break-all' }}>{c.tag}</span>
                  <span style={{ color: '#999', flexShrink: 0 }}>{fmtSize(c.size)}</span>
                </div>
              ))}
            </div>
            {scanResult.skipped_in_use.length > 0 && (
              <p style={{ color: '#999', marginBottom: 0 }}>
                {scanResult.skipped_in_use.length} 个冗余镜像正被容器使用，将跳过：
                {scanResult.skipped_in_use.map((c) => c.tag).join('、')}
              </p>
            )}
          </div>
        )}
      </Modal>
    </div>
  );
}
