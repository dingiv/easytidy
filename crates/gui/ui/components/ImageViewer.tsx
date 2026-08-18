// 图片预览面板：经 server socket 分块读取（fetch_file_b64）→ data: URI 显示。
// rootless + pasta 网络下容器 HTTP 端口宿主不可达，故不走 HTTP 静态托管。

import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { errMsg } from '../lib/errors';
import { Spin, Typography } from 'antd';
import { mimeForPath } from './mime';

interface ImageViewerProps {
  path: string;
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
          setError(errMsg(err, '加载图片失败'));
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
