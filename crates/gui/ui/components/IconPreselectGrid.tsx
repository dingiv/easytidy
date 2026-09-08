// 预选图标：从扫描到的容器应用（.desktop 快捷方式）里挑一个图标，回填到
// 图标输入框。数据源 = apps_list（容器固定目录的 .desktop 应用），
// 每个 .desktop 的 Icon= 即容器内图标路径；图标经 server 拉取显示。
//
// 两种用法：
// - 点选：回填到当前绑定的输入框（自定义应用表单）
// - 拖拽：拖到任意图标输入框（自定义应用 / 容器导出），drop 时把容器内
//   路径写入该输入框（dataTransfer text/plain）

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
      <div className="icon-preselect-title">从容器应用预选用（{icons.length}，可拖到图标输入框）</div>
      <div className="icon-preselect-grid">
        {icons.map(({ path, name }) => (
          <button
            key={path}
            type="button"
            draggable
            className={`icon-preselect-item ${selected === path ? 'selected' : ''}`}
            title={`${name}\n${path}（点选或拖到图标输入框）`}
            onClick={() => onSelect(path, name)}
            onDragStart={(e) => {
              // 自定义 MIME 优先（drop 端先读它），text/plain 兜底；
              // img 已禁原生拖拽，drag 只会从本按钮发起，不会被图片 data URI 覆盖
              e.dataTransfer.setData('application/x-easytidy-icon', path);
              e.dataTransfer.setData('text/plain', path);
              e.dataTransfer.effectAllowed = 'copy';
            }}
          >
            <AppIcon path={path} size={32} />
            <span className="icon-preselect-name">{name}</span>
          </button>
        ))}
      </div>
    </div>
  );
}
