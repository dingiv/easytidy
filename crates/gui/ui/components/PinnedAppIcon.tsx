// 收藏应用图标（工具栏 / passthrough 行内通用）。
//
// 图标引用存容器内配置（容器自包含）：
// - 自定义应用（id 以 custom: 开头）：图标 = 宿主 ~/.easytidy/icons 路径。
//   展示时经 fs_host_exists 探测该宿主路径——存在则正常显示，缺失（容器迁移到
//   其它宿主 / 图标被清理）则显示「图标异常」。
// - 扫描应用：图标 = 容器内路径，经 AppIcon（server 拉取）显示，加载失败自带占位。

import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { AppIcon } from './AppIcon';

interface PinnedAppIconProps {
  /** 应用 id（custom: 前缀 = 自定义应用） */
  id: string;
  /** 图标路径：自定义 = 宿主路径；扫描 = 容器内路径 */
  icon?: string | null;
  size?: number;
}

export function PinnedAppIcon({ id, icon, size = 22 }: PinnedAppIconProps) {
  const isCustom = id.startsWith('custom:');
  // 自定义应用宿主图标是否缺失（null = 未知/不适用，false = 存在）
  const [hostMissing, setHostMissing] = useState(false);

  useEffect(() => {
    let cancelled = false;
    setHostMissing(false);
    if (isCustom && icon) {
      invoke<boolean>('fs_host_exists', { path: icon })
        .then((ok) => {
          if (!cancelled) setHostMissing(!ok);
        })
        .catch(() => {
          if (!cancelled) setHostMissing(true);
        });
    }
    return () => {
      cancelled = true;
    };
  }, [isCustom, icon]);

  if (isCustom) {
    if (!icon) {
      return <span className="favorite-fallback">⚙️</span>;
    }
    if (hostMissing) {
      return (
        <span className="favorite-fallback favorite-abnormal" title="图标异常">
          ⚠️
        </span>
      );
    }
    return (
      <img
        src={`file://${icon}`}
        alt=""
        style={{ width: size, height: size, objectFit: 'contain', borderRadius: 4 }}
      />
    );
  }

  return <AppIcon path={icon ?? null} size={size} />;
}
