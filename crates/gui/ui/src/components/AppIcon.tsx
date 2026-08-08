// 容器内应用图标：经 server（socket fetch_file_b64）拉取 → data: URI 显示。
// 数据源是容器内 server，而非宿主文件系统——container 路径在宿主上不存在，
// 旧实现 `<img src="file://<container path>">` 必挂（实测）。
// rootless + pasta 网络下容器 HTTP 端口宿主不可达，故不走 HTTP 静态托管。

import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { mimeForPath } from './mime';

interface AppIconProps {
  /** 容器内图标路径（apps_list.icon_path） */
  path?: string | null;
  size?: number;
}

/** 容器内应用图标（经 server fs.read 分块拉取；加载失败回退占位符） */
export function AppIcon({ path, size = 28 }: AppIconProps) {
  const [src, setSrc] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let cancelled = false;
    setSrc(null);
    setFailed(false);
    if (!path) {
      setFailed(true);
      return;
    }
    invoke<string>('fetch_file_b64', { path })
      .then((b64) => {
        if (!cancelled) setSrc(`data:${mimeForPath(path)};base64,${b64}`);
      })
      .catch((err: any) => {
        if (!cancelled) {
          console.error('AppIcon fetch failed:', path, err);
          setFailed(true);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [path]);

  if (!src) {
    return <span className="app-icon-fallback">{failed ? '📦' : '…'}</span>;
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
