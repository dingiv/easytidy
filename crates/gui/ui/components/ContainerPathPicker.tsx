// 容器内路径选择器（图标选择的「浏览」入口）：导航容器内目录、选定一个
// 图片文件，把完整路径交还调用方（路径输入框）。
//
// 从 IconPickerModal 原内嵌文件浏览移植（fs_list 列目录）；只负责「选一个
// 图片文件路径」。非图片文件置灰不可选（图标必须是图片）。

import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { errMsg } from '../lib/errors';
import { Modal, Button, Input, Spin, Empty } from 'antd';

interface FsEntry {
  name: string;
  is_dir: boolean;
  size?: number;
}

const IMAGE_EXT = new Set(['png', 'jpg', 'jpeg', 'svg', 'ico', 'webp', 'gif', 'bmp']);

function isImageName(name: string): boolean {
  return IMAGE_EXT.has(name.split('.').pop()?.toLowerCase() ?? '');
}

/** 路径的父目录（顶层 → `/`） */
export function parentDir(path: string): string {
  const trimmed = path.replace(/\/+$/, '');
  const i = trimmed.lastIndexOf('/');
  if (i <= 0) return '/';
  return trimmed.slice(0, i);
}

interface ContainerPathPickerProps {
  open: boolean;
  /** 初始目录（一般 = 输入框当前值的父目录） */
  initialPath: string;
  /** 选定文件（图片）的完整路径交还调用方 */
  onPick: (path: string) => void;
  onClose: () => void;
}

export function ContainerPathPicker({ open, initialPath, onPick, onClose }: ContainerPathPickerProps) {
  const [cwd, setCwd] = useState('/');
  const [entries, setEntries] = useState<FsEntry[]>([]);
  const [browsing, setBrowsing] = useState(false);
  const [selected, setSelected] = useState<string | null>(null);
  const [navInput, setNavInput] = useState('/');
  const [error, setError] = useState<string | null>(null);

  // 打开时重置并加载初始目录（若初始路径是文件，回退到父目录）
  useEffect(() => {
    if (!open) return;
    setError(null);
    setSelected(null);
    const start = initialPath;
    const fallback = parentDir(start);
    void loadDir(start, fallback !== start ? fallback : undefined);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  const loadDir = async (dir: string, fallback?: string) => {
    setBrowsing(true);
    try {
      const list = await invoke<FsEntry[]>('fs_list', { path: dir });
      setCwd(dir);
      setNavInput(dir);
      setEntries(list);
      setSelected(null);
    } catch (err: any) {
      if (fallback && fallback !== dir) {
        await loadDir(fallback); // 初始路径是文件 → 回退父目录
        return;
      }
      setError(errMsg(err, '读取容器目录失败'));
      setCwd(dir);
      setNavInput(dir);
      setEntries([]);
    } finally {
      setBrowsing(false);
    }
  };

  const goUp = () => {
    loadDir(parentDir(cwd));
  };

  const confirm = () => {
    if (!selected) return;
    onPick(selected);
    onClose();
  };

  return (
    <Modal
      title="浏览容器内图片文件"
      open={open}
      onCancel={onClose}
      width={560}
      footer={[
        <Button key="cancel" onClick={onClose}>
          取消
        </Button>,
        <Button key="ok" type="primary" disabled={!selected} onClick={confirm}>
          选择
        </Button>,
      ]}
    >
      <div className="icon-picker-nav" style={{ marginBottom: 8 }}>
        <Input
          value={navInput}
          onChange={(e) => setNavInput(e.target.value)}
          onPressEnter={() => loadDir(navInput)}
          placeholder="容器内路径（回车跳转）"
        />
        <Button onClick={() => loadDir(navInput)}>跳转</Button>
        <Button onClick={goUp} disabled={cwd === '/'}>
          上级
        </Button>
      </div>
      {error && (
        <div className="error-message" style={{ marginBottom: 8 }}>
          {error}
        </div>
      )}
      {browsing ? (
        <div className="icon-picker-list icon-picker-loading" style={{ minHeight: 220 }}>
          <Spin />
        </div>
      ) : (
        <div className="icon-picker-list" style={{ minHeight: 220, maxHeight: 320 }}>
          {entries.map((e) => {
            const isImage = !e.is_dir && isImageName(e.name);
            const full = cwd === '/' ? `/${e.name}` : `${cwd}/${e.name}`;
            const disabled = !e.is_dir && !isImage;
            return (
              <div
                key={full}
                className={`icon-picker-entry ${selected === full ? 'selected' : ''}`}
                style={disabled ? { opacity: 0.45, cursor: 'default' } : undefined}
                onClick={() => (e.is_dir ? loadDir(full) : isImage && setSelected(full))}
                title={
                  e.is_dir
                    ? full
                    : `${full}${e.size != null ? `（${e.size} B）` : ''}${disabled ? '（非图片文件）' : ''}`
                }
              >
                <span className="icon-picker-entry-icon">
                  {e.is_dir ? '📁' : isImage ? '🖼️' : '📄'}
                </span>
                <span className="icon-picker-name">{e.name}</span>
              </div>
            );
          })}
          {entries.length === 0 && (
            <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="空目录" />
          )}
        </div>
      )}
    </Modal>
  );
}
