// 镜像选择下拉（自定义渲染，不用 antd Select）。
//
// antd Select 内部用 @rc-component/virtual-list：它用自定义 wheel 处理滚动
// （把 deltaY 当像素、不做 deltaMode 换算），WebKitGTK（Tauri Linux webview）
// 下滚动手感异常缓慢。此处改为**原生可滚动 <div>**（webview 下滚动正常）+
// 搜索过滤 + 键盘导航，视觉对齐配置表单的暗色风格。

import { useEffect, useMemo, useRef, useState } from 'react';
import type { ImageSummary } from '../../types';

interface ImageSelectProps {
  images: ImageSummary[];
  /** 当前选中的镜像全名（如 docker.io/library/ubuntu:24.04） */
  value: string;
  /** 选中 / 清空时回调（清空 = 空字符串） */
  onChange: (v: string) => void;
  placeholder?: string;
  /** 无匹配时的提示 */
  emptyHint?: string;
}

/** 字节数 → 人读 ("1.4 GB") */
function formatSize(bytes: number): string {
  if (bytes <= 0) return '-';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let i = 0;
  let n = bytes;
  while (n >= 1024 && i < units.length - 1) {
    n /= 1024;
    i++;
  }
  return `${n.toFixed(n < 10 && i > 0 ? 1 : 0)} ${units[i]}`;
}

/** Unix epoch 秒 → "3d ago" / "2h ago" */
function formatAge(created: number): string {
  if (!created) return '-';
  const now = Math.floor(Date.now() / 1000);
  const diff = Math.max(0, now - created);
  if (diff < 60) return `${diff}s ago`;
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`;
  return `${Math.floor(diff / 86400)}d ago`;
}

export function ImageSelect({
  images, value, onChange, placeholder, emptyHint,
}: ImageSelectProps) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState('');
  const [active, setActive] = useState(0);
  const rootRef = useRef<HTMLDivElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);

  // 摊平 image → tag 选项（去重 + 排序）
  const options = useMemo(() => {
    const seen = new Set<string>();
    const opts: { value: string; meta: string }[] = [];
    for (const img of images) {
      for (const tag of img.repo_tags) {
        if (tag === '' || seen.has(tag)) continue;
        seen.add(tag);
        opts.push({ value: tag, meta: `${formatSize(img.size)} · ${formatAge(img.created)}` });
      }
    }
    opts.sort((a, b) => a.value.localeCompare(b.value));
    return opts;
  }, [images]);

  // 按 query 过滤（不区分大小写子串匹配 tag）
  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return options;
    return options.filter((o) => o.value.toLowerCase().includes(q));
  }, [options, query]);

  // 打开：重置搜索 + active，聚焦搜索框
  useEffect(() => {
    if (open) {
      setQuery('');
      setActive(0);
      requestAnimationFrame(() => searchRef.current?.focus());
    }
  }, [open]);

  // 点击外部关闭
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener('mousedown', onDown);
    return () => document.removeEventListener('mousedown', onDown);
  }, [open]);

  // 搜索变化 → active 归零
  useEffect(() => {
    setActive(0);
  }, [query]);

  // active 变更 → 滚入可见
  useEffect(() => {
    const el = listRef.current?.children[active] as HTMLElement | undefined;
    el?.scrollIntoView({ block: 'nearest' });
  }, [active]);

  const select = (v: string) => {
    onChange(v);
    setOpen(false);
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    switch (e.key) {
      case 'ArrowDown':
        e.preventDefault();
        setActive((a) => Math.min(a + 1, filtered.length - 1));
        break;
      case 'ArrowUp':
        e.preventDefault();
        setActive((a) => Math.max(a - 1, 0));
        break;
      case 'Enter':
        e.preventDefault();
        if (filtered[active]) select(filtered[active].value);
        break;
      case 'Escape':
        e.preventDefault();
        setOpen(false);
        break;
    }
  };

  const selectedOpt = options.find((o) => o.value === value);

  return (
    <div className="image-select" ref={rootRef}>
      <div
        className={`image-select-control${open ? ' open' : ''}`}
        onClick={() => setOpen((o) => !o)}
        role="combobox"
        aria-expanded={open}
      >
        <span className="image-select-value-wrap">
          <span className={`image-select-value${value ? '' : ' placeholder'}`}>
            {value || placeholder || ''}
          </span>
          {selectedOpt && <span className="image-select-meta">{selectedOpt.meta}</span>}
        </span>
        <span className="image-select-actions">
          {value && (
            <button
              type="button"
              className="image-select-clear"
              aria-label="清空"
              onClick={(e) => {
                e.stopPropagation();
                onChange('');
                setOpen(false);
              }}
            >
              ×
            </button>
          )}
          <span className="image-select-caret" />
        </span>
      </div>

      {open && (
        <div className="image-select-dropdown">
          <input
            ref={searchRef}
            className="image-select-search"
            placeholder="输入关键字筛选本地镜像"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={onKeyDown}
          />
          <div className="image-select-list" ref={listRef}>
            {filtered.map((o, i) => (
              <div
                key={o.value}
                className={`image-option${i === active ? ' active' : ''}`}
                onClick={() => select(o.value)}
                onMouseEnter={() => setActive(i)}
              >
                <div className="image-option-tag">{o.value}</div>
                <div className="image-option-meta">{o.meta}</div>
              </div>
            ))}
            {filtered.length === 0 && (
              <div className="image-select-empty">
                {emptyHint || '无匹配镜像（先到「镜像」面板拉取）'}
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
