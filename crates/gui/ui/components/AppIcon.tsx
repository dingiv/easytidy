// 容器内应用图标：<img src="icon://<路径>"> 经 Tauri 后端 icon:// 自定义协议中转。
// 后端先读宿主文件,宿主不存在再走容器 server socket 拉取;XPM/BMP/ICO/TIFF
// 后端转 PNG(to_browser_renderable)。替代早期 file://(宿主,容器路径宿主不存在
// 必挂)/ server HTTP 静态托管(rootless 下端口宿主不可达)/ base64 方案。

import { useState } from 'react';

interface AppIconProps {
  /** 容器内图标路径（apps_list.icon_path）；.desktop 的 Icon= 也可能是
   *  freedesktop 主题名（org.gnome.Screenshot / printer 等，无对应文件） */
  path?: string | null;
  size?: number;
  /** 是否允许 img 原生拖拽（默认 false——原生图片拖拽会把 data:image 写进
   *  transfer，干扰自定义拖拽逻辑，如预选图标拖到输入框） */
  draggable?: boolean;
}

/** icon:// URL（Tauri 自定义协议;浏览器按 scheme 请求 → 后端 icon:// 处理器中转）。
 *  Linux/WebKitGTK 要求 URL 带 host，否则 wry 构建请求失败（could not create request）；
 *  用 `localhost` 作 host（同 tauri://localhost 约定），后端 uri().path() 仍取到绝对路径。 */
const iconSrc = (p: string) => `icon://localhost${p}`;

/** 容器内应用图标（<img src="icon://<path>"> 经 Tauri 后端中转；加载失败回退占位符）。
 *  仅绝对路径会请求;主题图标名无对应文件（无 / 前缀）,不发请求直接占位
 *  （避免 404 / op_failed 告警风暴）。
 *  `failedFor` 记录失败的具体路径:换 path 后 `failedFor !== path` 自动重试。 */
export function AppIcon({ path, size = 28, draggable = false }: AppIconProps) {
  const isPath = !!path && path.startsWith('/');
  const [failedFor, setFailedFor] = useState<string | null>(null);
  const failed = isPath && failedFor === path;

  if (!isPath || failed) {
    return <span className="app-icon-fallback">📦</span>;
  }
  return (
    <img
      key={path}
      className="app-icon-img"
      src={iconSrc(path!)}
      alt=""
      draggable={draggable}
      onError={() => setFailedFor(path!)}
      style={{ width: size, height: size, objectFit: 'contain' }}
    />
  );
}
