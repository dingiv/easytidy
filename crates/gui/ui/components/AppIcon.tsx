// 容器内应用图标：经 server（socket fetch_file_b64）拉取 → data: URI 显示。
// 数据源是容器内 server，而非宿主文件系统——container 路径在宿主上不存在，
// 旧实现 `<img src="file://<container path>">` 必挂（实测）。
// rootless + pasta 网络下容器 HTTP 端口宿主不可达，故不走 HTTP 静态托管。

import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { mimeForPath } from './mime';

interface AppIconProps {
  /** 容器内图标路径（apps_list.icon_path）；.desktop 的 Icon= 也可能是
   *  freedesktop 主题名（org.gnome.Screenshot / printer 等，无对应文件） */
  path?: string | null;
  size?: number;
  /** 是否允许 img 原生拖拽（默认 false——原生图片拖拽会把 data:image 写进
   *  transfer，干扰自定义拖拽逻辑，如预选图标拖到输入框） */
  draggable?: boolean;
}

/** 路径→data: URI 缓存（列表刷新 / tab 切换重挂载不重复拉取） */
const iconCache = new Map<string, string>();

/** 容器内应用图标（经 server fs.read 分块拉取；加载失败回退占位符）。
 *  仅绝对路径会拉取；主题图标名无对应文件（server fs.read 必 ENOENT），
 *  不发请求直接占位（避免 op_failed 告警风暴）。 */
export function AppIcon({ path, size = 28, draggable = false }: AppIconProps) {
  const [src, setSrc] = useState<string | null>(() =>
    path && path.startsWith('/') ? iconCache.get(path) ?? null : null,
  );
  const [failed, setFailed] = useState(!path || !path.startsWith('/'));

  useEffect(() => {
    if (!path || !path.startsWith('/')) {
      // 主题图标名 / 空：无文件可拉，占位
      setSrc(null);
      setFailed(true);
      return;
    }
    let cancelled = false;
    const cached = iconCache.get(path);
    if (cached) {
      setSrc(cached);
      setFailed(false);
      return;
    }
    setSrc(null);
    setFailed(false);
    invoke<string>('fetch_file_b64', { path })
      .then((b64) => {
        if (cancelled) return;
        const uri = `data:${mimeForPath(path)};base64,${b64}`;
        iconCache.set(path, uri);
        setSrc(uri);
      })
      .catch(() => {
        // 图标缺失是常见情况（server 侧已记 op_failed 日志），不再重复 error
        if (!cancelled) setFailed(true);
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
      draggable={draggable}
      style={{ width: size, height: size, objectFit: 'contain' }}
    />
  );
}
