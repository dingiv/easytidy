// 图片预览面板：经 server socket 分块读取（fetch_file_b64）→ data: URI 显示。
// rootless + pasta 网络下容器 HTTP 端口宿主不可达，故不走 HTTP 静态托管。

import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Spin, Typography } from 'antd';

interface ImageViewerProps {
  path: string;
}

/** 按扩展名推断 mime（data: URI 需要） */
function mimeForPath(path: string): string {
  const ext = path.split('.').pop()?.toLowerCase() ?? '';
  switch (ext) {
    case 'png': return 'image/png';
    case 'jpg':
    case 'jpeg': return 'image/jpeg';
    case 'gif': return 'image/gif';
    case 'webp': return 'image/webp';
    case 'svg': return 'image/svg+xml';
    case 'bmp': return 'image/bmp';
    case 'ico': return 'image/x-icon';
    default: return 'application/octet-stream';
  }
}

export function ImageViewer({ path }: ImageViewerProps) {
  const [src, setSrc] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setSrc(null);
    setError(null);
    invoke<string>('fetch_file_b64', { path })
      .then((b64) => {
        if (!cancelled) setSrc(`data:${mimeForPath(path)};base64,${b64}`);
      })
      .catch((err: any) => {
        if (!cancelled) {
          setError(err?.message || '加载图片失败');
          console.error('fetch_file_b64 failed:', err);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [path]);

  return (
    <div className="image-viewer">
      {error ? (
        <Typography.Text type="danger">{error}</Typography.Text>
      ) : !src ? (
        <Spin tip="加载图片…" size="large">
          <div className="spin-block" />
        </Spin>
      ) : (
        <img src={src} alt={path} />
      )}
    </div>
  );
}
