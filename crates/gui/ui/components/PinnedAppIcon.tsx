// 收藏应用图标（工具栏 / passthrough 行内通用）。
//
// 图标引用存容器内配置（容器自包含）：扫描应用与自定义应用均为**容器内**
// 图标路径，统一经 AppIcon（server 拉取）显示，加载失败自带占位。

import { AppIcon } from './AppIcon';

interface PinnedAppIconProps {
  /** 应用 id（保留字段：custom: 前缀 = 自定义应用） */
  id: string;
  /** 容器内图标路径 */
  icon?: string | null;
  size?: number;
}

export function PinnedAppIcon({ id: _id, icon, size = 22 }: PinnedAppIconProps) {
  if (!icon) {
    return <span className="favorite-fallback">⚙️</span>;
  }
  return <AppIcon path={icon} size={size} />;
}
