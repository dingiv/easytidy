import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Breadcrumb, Spin, Empty, Button } from 'antd';
import { FolderOutlined, FileOutlined, ArrowLeftOutlined } from '@ant-design/icons';
import { useFileBrowserStore } from '../stores/fileBrowserStore';
import type { FsEntry } from '../types';
import './FileBrowser.css'

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

export function FileBrowser() {
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
    }
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
