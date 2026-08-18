// 文件浏览器：目录浏览 + 右键菜单（打开/复制/粘贴/选中）+ 选中态 +
// 宿主文件拖入上传（双向传输的"宿主→容器"方向；容器→宿主导出预留）。

import { useEffect, useState } from 'react';
import { invoke, Channel } from '@tauri-apps/api/core';
import { errMsg } from '../lib/errors';
import { getCurrentWebview } from '@tauri-apps/api/webview';
import type { UnlistenFn } from '@tauri-apps/api/event';
import { App as AntApp, Breadcrumb, Spin, Empty, Button, Tooltip, Dropdown, Modal, Progress } from 'antd';
import type { MenuProps } from 'antd';
import {
  AimOutlined,
  FolderOutlined,
  FileOutlined,
  ArrowLeftOutlined,
} from '@ant-design/icons';
import { useFileBrowserStore } from '../stores/fileBrowserStore';
import type { FsEntry } from '../types';
import './FileBrowser.css'

interface FileBrowserProps {
  /** 右键"打开"回调（文本编辑器面板） */
  onOpenFile?: (path: string) => void;
  /** 右键"预览图片"回调（图片预览面板） */
  onPreviewImage?: (path: string) => void;
  /** 跟随终端开关（PerContainer 订阅 cwd 事件后导航） */
  followTerminal?: boolean;
  onToggleFollow?: () => void;
}

/** 图片扩展名（右键"预览图片"） */
const IMAGE_EXTS = new Set(['png', 'jpg', 'jpeg', 'gif', 'webp', 'svg', 'bmp', 'ico']);

function isImageFile(name: string): boolean {
  const dot = name.toLowerCase().lastIndexOf('.');
  if (dot <= 0) return false;
  return IMAGE_EXTS.has(name.toLowerCase().slice(dot + 1));
}

/** 路径 → 面包屑层级（每级可点击跳转） */
function pathToCrumbs(path: string): { label: string; key: string }[] {
  if (path === '/' || path === '') return [{ label: '(root)', key: '/' }];
  const parts = path.split('/').filter(Boolean);
  return [
    { label: '(root)', key: '/' },
    ...parts.map((p, i) => ({
      label: p,
      key: `/${parts.slice(0, i + 1).join('/')}`,
    })),
  ];
}

export function FileBrowser({ onOpenFile, onPreviewImage, followTerminal, onToggleFollow }: FileBrowserProps) {
  const { message } = AntApp.useApp();
  const { currentPath, navigate, navigateToParent, selectedPaths, copiedPath, toggleSelect, clearSelection, setCopied } =
    useFileBrowserStore();
  const [entries, setEntries] = useState<FsEntry[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [importing, setImporting] = useState(false);
  // 拖入进度（Channel 事件）
  const [importProgress, setImportProgress] = useState<{
    done: number;
    total: number;
    file: string;
  } | null>(null);

  const reload = () => {
    setLoading(true);
    setError(null);
    invoke<FsEntry[]>('fs_list', { path: currentPath })
      .then(setEntries)
      .catch((err: any) => {
        setError(errMsg(err, 'Failed to load directory'));
        console.error('fs_list failed:', err);
      })
      .finally(() => setLoading(false));
  };

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    invoke<FsEntry[]>('fs_list', { path: currentPath })
      .then((result) => {
        if (!cancelled) setEntries(result);
      })
      .catch((err: any) => {
        if (!cancelled) {
          setError(errMsg(err, 'Failed to load directory'));
          console.error('fs_list failed:', err);
        }
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [currentPath]);

  /** 执行导入（分块上传 + Channel 进度） */
  const doImport = async (paths: string[]) => {
    const channel = new Channel<{ done: number; total: number; file: string }>();
    channel.onmessage = (p) => setImportProgress(p);
    setImporting(true);
    setImportProgress({ done: 0, total: 1, file: '准备中…' });
    try {
      await invoke('import_files', { paths, destDir: currentPath, onProgress: channel });
      message.success(`已导入 ${paths.length} 个项目`);
      reload();
    } catch (err: any) {
      message.error(errMsg(err, '导入失败'));
      console.error('import_files failed:', err);
    } finally {
      setImporting(false);
      setImportProgress(null);
    }
  };

  /** 宿主文件/文件夹拖入：先探测（含文件夹 → 确认弹窗），再上传 */
  const handleDrop = async (paths: string[]) => {
    if (!paths.length) return;
    try {
      const info = await invoke<{ has_dir: boolean; total_bytes: number; file_count: number }>(
        'import_inspect',
        { paths },
      );
      if (info.has_dir) {
        Modal.confirm({
          title: '确认导入文件夹',
          content: `拖动内容包含文件夹（共 ${info.file_count} 个文件，约 ${formatBytes(
            info.total_bytes,
          )}），确认复制到容器目录 ${currentPath}？`,
          okText: '确认导入',
          cancelText: '取消',
          onOk: () => doImport(paths),
        });
      } else {
        doImport(paths);
      }
    } catch (err: any) {
      message.error(errMsg(err, '导入失败'));
      console.error('import_inspect failed:', err);
    }
  };

  /** 宿主文件拖入 → 分块上传到当前容器目录（Tauri GTK 层,拿宿主绝对路径） */
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    let cancelled = false;
    getCurrentWebview()
      .onDragDropEvent((event) => {
        const e = event.payload;
        if (e.type !== 'drop' || cancelled) return;
        handleDrop(e.paths);
      })
      .then((fn) => {
        unlisten = fn;
      })
      .catch((err) => console.error('onDragDropEvent failed:', err));
    return () => {
      cancelled = true;
      unlisten?.();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [currentPath]);

  const fullPath = (name: string) =>
    currentPath === '/' ? `/${name}` : `${currentPath}/${name}`;

  /** 打开文件（右键菜单）：目录进目录；文件（不限后缀）打开编辑器 */
  const openEntry = (entry: FsEntry) => {
    const path = fullPath(entry.name);
    if (entry.is_dir) {
      navigate(path);
      return;
    }
    if (entry.size && entry.size > 1_048_576) {
      message.info('文件超过 1MB，请用终端编辑');
      return;
    }
    onOpenFile?.(path);
  };

  /** 粘贴：容器内复制源 → 当前目录（server fs.copy） */
  const pasteCopied = async () => {
    if (!copiedPath) return;
    const name = copiedPath.split('/').filter(Boolean).pop() ?? 'file';
    const dst = fullPath(name);
    if (dst === copiedPath) {
      message.info('源与目标相同');
      return;
    }
    try {
      await invoke('fs_copy', { src: copiedPath, dst });
      message.success(`已复制到 ${dst}`);
      reload();
    } catch (err: any) {
      message.error(errMsg(err, '复制失败'));
      console.error('fs_copy failed:', err);
    }
  };

  /** 导出容器文件到宿主（rfd 保存对话框;fs_read 分块下载） */
  const exportToHost = async (path: string) => {
    try {
      const dest = await invoke<string>('export_file_dialog', { path });
      message.success(`已导出到 ${dest}`);
    } catch (err: any) {
      if (err?.message !== '已取消') {
        message.error(errMsg(err, '导出失败'));
        console.error('export_file_dialog failed:', err);
      }
    }
  };

  /** 条目右键菜单 */
  const entryMenu = (entry: FsEntry): MenuProps => ({
    items: [
      { key: 'open', label: entry.is_dir ? '打开目录' : '使用文本编辑器打开' },
      ...(!entry.is_dir && isImageFile(entry.name)
        ? [{ key: 'preview', label: '预览图片' }]
        : []),
      { key: 'export', label: '导出到宿主…', disabled: entry.is_dir },
      { type: 'divider' },
      { key: 'copy', label: '复制' },
      { key: 'paste', label: '粘贴', disabled: !copiedPath },
      { key: 'select', label: selectedPaths.includes(fullPath(entry.name)) ? '取消选中' : '选中' },
    ],
    onClick: ({ key }) => {
      const path = fullPath(entry.name);
      switch (key) {
        case 'open':
          openEntry(entry);
          break;
        case 'preview':
          onPreviewImage?.(path);
          break;
        case 'export':
          exportToHost(path);
          break;
        case 'copy':
          setCopied(path);
          message.success(`已标记复制：${entry.name}`);
          break;
        case 'paste':
          pasteCopied();
          break;
        case 'select':
          toggleSelect(path);
          break;
      }
    },
  });

  /** 空白区右键菜单（粘贴 + 刷新） */
  const blankMenu: MenuProps = {
    items: [
      { key: 'paste', label: '粘贴', disabled: !copiedPath },
      { key: 'refresh', label: '刷新' },
      { key: 'clear', label: '清除选中', disabled: selectedPaths.length === 0 },
    ],
    onClick: ({ key }) => {
      if (key === 'paste') pasteCopied();
      else if (key === 'refresh') reload();
      else if (key === 'clear') clearSelection();
    },
  };

  const crumbs = pathToCrumbs(currentPath);

  return (
    <div
      className="file-browser"
      // 取消鼠标选中文字与默认右键菜单
      onContextMenu={(e) => e.preventDefault()}
    >
      {/* 导航行：跟随 + 返回 + 面包屑 */}
      <header className="file-browser-head">
        <Tooltip
          title={followTerminal ? '关闭跟随终端' : '跟随终端（目录自动同步到终端 pwd）'}
          mouseEnterDelay={4}
        >
          <Button
            size="small"
            type={followTerminal ? 'primary' : 'text'}
            icon={<AimOutlined />}
            onClick={onToggleFollow}
            title="跟随终端"
          />
        </Tooltip>
        <div className="file-browser-breadcrumb">
          <Breadcrumb
            items={crumbs.map((c) => ({
              title: (
                <a
                  href="#"
                  onClick={(e) => {
                    e.preventDefault();
                    if (c.key === currentPath) return;
                    navigate(c.key);
                  }}
                  style={{ fontWeight: c.key === currentPath ? 600 : 'normal' }}
                >
                  {c.label}
                </a>
              ),
            }))}
          />
          <Button
            size="small"
            type="text"
            icon={<ArrowLeftOutlined />}
            disabled={currentPath === '/'}
            onClick={navigateToParent}
            title="返回上一级"
          />
        </div>
      </header>

      {error && <div className="error-message">{error}</div>}

      {/* 拖入进度条（header 下方） */}
      {importProgress && (
        <div className="import-progress">
          <Progress
            percent={
              importProgress.total > 0
                ? Math.min(100, Math.round((importProgress.done / importProgress.total) * 100))
                : 0
            }
            size="small"
            status="active"
          />
          <span className="import-progress-file" title={importProgress.file}>
            {importing ? '导入中' : '导入完成'}：{importProgress.file}
          </span>
        </div>
      )}

      {loading ? (
        <div style={{ display: 'flex', justifyContent: 'center', padding: '2rem 0' }}>
          <Spin />
        </div>
      ) : entries.length === 0 ? (
        <Dropdown menu={blankMenu} trigger={['contextMenu']}>
          <Empty description="目录为空（右键粘贴/刷新）" style={{ marginTop: '2rem' }} />
        </Dropdown>
      ) : (
        <Dropdown menu={blankMenu} trigger={['contextMenu']}>
          <div className="file-list">
            {entries.map((entry) => {
              const path = fullPath(entry.name);
              const isSelected = selectedPaths.includes(path);
              return (
                <Dropdown key={entry.name} menu={entryMenu(entry)} trigger={['contextMenu']}>
                  <div
                    className={`file-entry ${entry.is_dir ? 'directory' : 'file'} ${
                      isSelected ? 'selected' : ''
                    }`}
                    onClick={() => toggleSelect(path)}
                    // 双击智能打开：目录进目录；图片 → 预览面板；其他(≤1MB) → 编辑器
                    onDoubleClick={() => {
                      if (entry.is_dir) {
                        navigate(path);
                        return;
                      }
                      if (isImageFile(entry.name)) {
                        onPreviewImage?.(path);
                      } else if (!(entry.size && entry.size > 1_048_576)) {
                        onOpenFile?.(path);
                      }
                    }}
                  >
                    <span className="file-icon">
                      {entry.is_dir ? <FolderOutlined /> : <FileOutlined />}
                    </span>
                    <span className="file-name">{entry.name}</span>
                    <span className="file-size">{entry.is_dir ? '' : formatSize(entry.size)}</span>
                  </div>
                </Dropdown>
              );
            })}
          </div>
        </Dropdown>
      )}
    </div>
  );
}

function formatSize(bytes?: number) {
  if (!bytes) return '-';
  const units = ['B', 'KB', 'MB', 'GB'];
  let size = bytes;
  let unitIndex = 0;
  while (size >= 1024 && unitIndex < units.length - 1) {
    size /= 1024;
    unitIndex++;
  }
  return `${size.toFixed(1)} ${units[unitIndex]}`;
}

function formatBytes(bytes: number): string {
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let size = bytes;
  let unitIndex = 0;
  while (size >= 1024 && unitIndex < units.length - 1) {
    size /= 1024;
    unitIndex++;
  }
  return `${size.toFixed(1)} ${units[unitIndex]}`;
}
