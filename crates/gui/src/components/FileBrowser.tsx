import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { App as AntApp, Breadcrumb, Spin, Empty, Button, Tooltip } from 'antd';
import { AimOutlined, FolderOutlined, FileOutlined, ArrowLeftOutlined } from '@ant-design/icons';
import { useFileBrowserStore } from '../stores/fileBrowserStore';
import type { FsEntry } from '../types';
import './FileBrowser.css'

interface FileBrowserProps {
  /** 双击文本文件回调（在右侧面板打开编辑器） */
  onOpenFile?: (path: string) => void;
  /** 跟随终端开关（PerContainer 轮询 pty.cwd 后导航） */
  followTerminal?: boolean;
  onToggleFollow?: () => void;
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

/** 纯文本扩展名/文件名白名单（双击可打开编辑） */
const TEXT_EXTS = new Set([
  'txt', 'md', 'json', 'json5', 'toml', 'yaml', 'yml',
  'sh', 'bash', 'py', 'js', 'jsx', 'ts', 'tsx', 'rs', 'c', 'h',
  'cpp', 'hpp', 'go', 'rb', 'java', 'sql', 'html', 'css', 'scss',
  'conf', 'cfg', 'ini', 'env', 'log', 'xml', 'csv', 'properties', 'vue',
]);
const TEXT_NAMES = new Set([
  'dockerfile', 'makefile', 'readme', 'license', 'gitignore',
  'bashrc', 'bash_profile', 'profile', 'zshrc', 'vimrc',
]);

/** 是否纯文本文件（扩展名/文件名白名单；编辑限制 1MB） */
function isTextFile(name: string, size?: number): boolean {
  if (size !== undefined && size > 1_048_576) return false; // >1MB 提示用终端
  const lower = name.toLowerCase();
  const dot = lower.lastIndexOf('.');
  if (dot > 0) return TEXT_EXTS.has(lower.slice(dot + 1));
  return TEXT_NAMES.has(lower);
}

export function FileBrowser({ onOpenFile, followTerminal, onToggleFollow }: FileBrowserProps) {
  const { message } = AntApp.useApp();
  const { currentPath, navigate, navigateToParent } = useFileBrowserStore();
  const [entries, setEntries] = useState<FsEntry[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

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
          setError(err.message || 'Failed to load directory');
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

  const handleEntryDoubleClick = (entry: FsEntry) => {
    if (entry.is_dir) {
      const newPath = currentPath === '/' ? `/${entry.name}` : `${currentPath}/${entry.name}`;
      navigate(newPath);
      return;
    }
    // 纯文本文件 → 回调 PerContainer 在右侧面板打开编辑器
    const filePath = currentPath === '/' ? `/${entry.name}` : `${currentPath}/${entry.name}`;
    if (!isTextFile(entry.name, entry.size)) {
      message.info(entry.size && entry.size > 1_048_576 ? '文件超过 1MB，请用终端编辑' : '二进制或非文本文件，请在终端中查看');
      return;
    }
    onOpenFile?.(filePath);
  };

  const formatSize = (bytes?: number) => {
    if (!bytes) return '-';
    const units = ['B', 'KB', 'MB', 'GB'];
    let size = bytes;
    let unitIndex = 0;
    while (size >= 1024 && unitIndex < units.length - 1) {
      size /= 1024;
      unitIndex++;
    }
    return `${size.toFixed(1)} ${units[unitIndex]}`;
  };

  const crumbs = pathToCrumbs(currentPath);

  return (
    <div className="file-browser">
      {/* 导航行：返回根目录 + 返回上一级 + 面包屑（每级可点击） */}
      <header className='file-browser-head'>
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

      {loading ? (
        <div style={{ display: 'flex', justifyContent: 'center', padding: '2rem 0' }}>
          <Spin />
        </div>
      ) : entries.length === 0 ? (
        <Empty description="目录为空" style={{ marginTop: '2rem' }} />
      ) : (
        <div className="file-list">
          {entries.map((entry) => (
            <div
              key={entry.name}
              className={`file-entry ${entry.is_dir ? 'directory' : 'file'}`}
              onDoubleClick={() => handleEntryDoubleClick(entry)}
            >
              <span className="file-icon">
                {entry.is_dir ? <FolderOutlined /> : <FileOutlined />}
              </span>
              <span className="file-name">{entry.name}</span>
              <span className="file-size">{entry.is_dir ? '' : formatSize(entry.size)}</span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
