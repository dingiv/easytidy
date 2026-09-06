// 预选图标：从扫描到的容器应用（.desktop 快捷方式）里挑一个图标，回填到
// 自定义应用表单。数据源 = apps_list（容器固定目录的 .desktop 应用），
// 每个 .desktop 的 Icon= 即容器内图标路径；图标经 server 拉取显示。
//
// 用途：自定义应用（容器固定目录之外）的图标，除了「键入路径」「从宿主机
// 选用」外，可从这里直接点选一个容器内已有图标，免去手敲路径。

import { useMemo } from 'react';
import { AppIcon } from './AppIcon';
import type { AppInfo } from '../types';

interface IconPreselectGridProps {
  /** 扫描到的容器应用（提供 icon_path + name） */
  apps: AppInfo[];
  /** 当前选中的图标路径（用于高亮；null = 未选） */
  selected: string | null;
  /** 点选某图标（回填容器内图标路径） */
  onSelect: (iconPath: string, name: string) => void;
}

/** 容器应用图标预选网格（去重、可滚动、点选回填路径） */
export function IconPreselectGrid({ apps, selected, onSelect }: IconPreselectGridProps) {
  // 按 icon_path 去重 + 过滤空路径（一个图标可能被多个应用引用）
  const icons = useMemo(() => {
    const seen = new Set<string>();
    const list: { path: string; name: string }[] = [];
    for (const app of apps) {
      const p = app.icon_path;
      if (!p || seen.has(p)) continue;
      seen.add(p);
      list.push({ path: p, name: app.name });
    }
    return list;
  }, [apps]);

  if (icons.length === 0) return null;

  return (
    <div className="icon-preselect">
      <div className="icon-preselect-title">从容器应用预选用（{icons.length}）</div>
      <div className="icon-preselect-grid">
        {icons.map(({ path, name }) => (
          <button
            key={path}
            type="button"
            className={`icon-preselect-item ${selected === path ? 'selected' : ''}`}
            title={`${name}\n${path}`}
            onClick={() => onSelect(path, name)}
          >
            <AppIcon path={path} size={32} />
            <span className="icon-preselect-name">{name}</span>
          </button>
        ))}
      </div>
    </div>
  );
}
