// 桌面图标（宿主侧 .desktop 快捷方式）管理：扫描全部 easytidy 生成的
// 桌面图标（容器入口 + passthrough 应用），支持图标重编与移除。
//
// **纯宿主侧**：只触碰宿主文件（applications 目录 / 桌面目录 / icons 目录），
// 不涉及容器内任何处理。扫描/移除/重编逻辑在 core::desktop（与导出链路共用）。

import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { errMsg } from '../lib/errors';
import { App as AntApp, Alert, Button, Empty, Popconfirm, Space, Spin, Table, Tag, Typography } from 'antd';
import { DeleteOutlined, PictureOutlined, ReloadOutlined } from '@ant-design/icons';
import { mimeForPath } from './mime';
import './DesktopIconsPanel.css';

/** 后端 desktop_icons_scan 行（core::desktop::DesktopIconEntry） */
interface DesktopIconEntry {
  /** entry = 容器名；app = `容器名/app_id` */
  identity: string;
  kind: 'entry' | 'app';
  container: string;
  title: string;
  icon: string;
  app_id: string | null;
  app_file: string | null;
  menu_path: string | null;
  desktop_path: string | null;
}

/** 宿主图片预览缓存（path → data: URI；刷新不重复读盘） */
const iconCache = new Map<string, string>();

/** 宿主图标预览（经 host_file_b64 读盘 → data: URI；失败/主题名回退占位） */
function HostIcon({ path, size = 28 }: { path: string; size?: number }) {
  const [src, setSrc] = useState<string | null>(() =>
    path ? iconCache.get(path) ?? null : null,
  );
  useEffect(() => {
    let cancelled = false;
    if (!path || !path.startsWith('/')) {
      setSrc(null);
      return;
    }
    const cached = iconCache.get(path);
    if (cached) {
      setSrc(cached);
      return;
    }
    invoke<string>('host_file_b64', { path })
      .then((b64) => {
        if (cancelled) return;
        const uri = `data:${mimeForPath(path)};base64,${b64}`;
        iconCache.set(path, uri);
        setSrc(uri);
      })
      .catch(() => {
        if (!cancelled) setSrc(null);
      });
    return () => {
      cancelled = true;
    };
  }, [path]);
  if (!src) {
    return <span className="app-icon-fallback">🖼️</span>;
  }
  return (
    <img
      className="app-icon-img"
      src={src}
      alt=""
      style={{ width: size, height: size, objectFit: 'contain' }}
    />
  );
}

export function DesktopIconsPanel() {
  const { message } = AntApp.useApp();
  const [entries, setEntries] = useState<DesktopIconEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  // 正在执行重编/移除的 identity（按钮 loading）
  const [busy, setBusy] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      setEntries(await invoke<DesktopIconEntry[]>('desktop_icons_scan'));
    } catch (err: any) {
      setError(errMsg(err, '扫描桌面图标失败'));
      console.error('desktop_icons_scan failed:', err);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  /** 移除：删菜单 + 桌面副本（确认框用 Popconfirm） */
  const remove = async (e: DesktopIconEntry) => {
    setBusy(e.identity);
    try {
      const removed = await invoke<string[]>('desktop_icons_remove', {
        kind: e.kind,
        container: e.container,
        appId: e.app_id,
      });
      message.success(`已移除${removed.length ? `（${removed.length} 个文件）` : ''}`);
      await load();
    } catch (err: any) {
      setError(errMsg(err, `移除「${e.title}」失败`));
      console.error('desktop_icons_remove failed:', err);
    } finally {
      setBusy(null);
    }
  };

  /** 重编图标：选宿主图片 → 品牌加工 → 更新该 identity 全部 .desktop */
  const reeditIcon = async (e: DesktopIconEntry) => {
    setBusy(e.identity);
    try {
      const source = await invoke<string>('desktop_icons_pick');
      await invoke('desktop_icons_set_icon', {
        kind: e.kind,
        container: e.container,
        appId: e.app_id,
        source,
      });
      message.success('图标已重编（品牌边框 + 水印，256×256）');
      await load();
    } catch (err: any) {
      if (String(err) !== '已取消') {
        setError(errMsg(err, `重编「${e.title}」图标失败`));
        console.error('desktop_icons_set_icon failed:', err);
      }
    } finally {
      setBusy(null);
    }
  };

  return (
    <div className="desktop-icons-panel">
      <div className="desktop-icons-header">
        <Typography.Title level={4} className="desktop-icons-title">
          桌面图标
        </Typography.Title>
        <Typography.Text type="secondary" className="desktop-icons-sub">
          宿主侧 easytidy 生成的桌面快捷方式（容器入口 + 透传应用）；重编/移除仅涉及宿主文件
        </Typography.Text>
        <Button icon={<ReloadOutlined />} onClick={load} loading={loading}>
          刷新
        </Button>
      </div>

      {error && (
        <Alert
          type="error"
          showIcon
          message="操作失败"
          description={error}
          closable
          onClose={() => setError(null)}
        />
      )}

      {loading ? (
        <div className="desktop-icons-loading">
          <Spin tip="扫描…" size="large">
            <div className="spin-block" />
          </Spin>
        </div>
      ) : entries.length === 0 ? (
        <Empty
          image={Empty.PRESENTED_IMAGE_SIMPLE}
          description="未发现 easytidy 桌面图标（创建容器或导出应用后出现）"
        />
      ) : (
        <Table
          size="small"
          rowKey={(r) => r.identity}
          dataSource={entries}
          pagination={false}
          columns={[
            {
              title: '图标',
              key: 'icon',
              width: 56,
              render: (_: unknown, r: DesktopIconEntry) => <HostIcon path={r.icon} />,
            },
            {
              title: '名称',
              key: 'title',
              render: (_: unknown, r: DesktopIconEntry) => (
                <Space size={6}>
                  <Typography.Text strong>{r.title}</Typography.Text>
                  <Tag color={r.kind === 'entry' ? 'blue' : 'green'}>
                    {r.kind === 'entry' ? '容器入口' : '应用'}
                  </Tag>
                </Space>
              ),
            },
            {
              title: '容器',
              key: 'container',
              width: 160,
              render: (_: unknown, r: DesktopIconEntry) => (
                <Typography.Text code>{r.container}</Typography.Text>
              ),
            },
            {
              title: '应用',
              key: 'app_id',
              width: 180,
              render: (_: unknown, r: DesktopIconEntry) =>
                r.kind === 'app' && r.app_id ? (
                  <Typography.Text code>{r.app_id}</Typography.Text>
                ) : (
                  '-'
                ),
            },
            {
              title: '位置',
              key: 'where',
              width: 120,
              render: (_: unknown, r: DesktopIconEntry) => (
                <Space size={4}>
                  {r.menu_path && <Tag>菜单</Tag>}
                  {r.desktop_path && <Tag color="purple">桌面</Tag>}
                </Space>
              ),
            },
            {
              title: '操作',
              key: 'actions',
              width: 170,
              render: (_: unknown, r: DesktopIconEntry) => (
                <Space size={4}>
                  <Button
                    size="small"
                    icon={<PictureOutlined />}
                    loading={busy === r.identity}
                    onClick={() => reeditIcon(r)}
                    title="选择宿主图片重编图标（品牌边框 + 水印加工）"
                  >
                    重编图标
                  </Button>
                  <Popconfirm
                    title={`移除「${r.title}」?`}
                    description="仅删除宿主快捷方式文件（菜单 + 桌面副本），不动容器。"
                    okText="移除"
                    cancelText="取消"
                    okButtonProps={{ danger: true }}
                    onConfirm={() => remove(r)}
                  >
                    <Button
                      size="small"
                      danger
                      icon={<DeleteOutlined />}
                      disabled={busy === r.identity}
                    >
                      移除
                    </Button>
                  </Popconfirm>
                </Space>
              ),
            },
          ]}
        />
      )}
    </div>
  );
}
