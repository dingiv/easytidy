// 自定义应用图标选择弹窗（已有应用改图标）：
// - 图标 = **容器内路径**输入框（AutoComplete 列目录补全 +「浏览」进目录挑选）
// - 「从宿主机选用」：rfd 原生对话框选图片 → 后端复制进容器
//   {home}/.easytidy/icons/ → 容器内路径回填输入框
// - 确定：写入容器内配置（passthrough_set_custom_icon）；清除：去掉图标
// 图标统一存容器内（容器自包含）；导出 .desktop 时再从容器拷出到宿主缓存。

import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { errMsg } from '../lib/errors';
import { Modal, Button, Input, Spin, AutoComplete, Space } from 'antd';
import { FolderOpenOutlined, DownloadOutlined } from '@ant-design/icons';
import { ContainerPathPicker, parentDir } from './ContainerPathPicker';

interface IconPickerModalProps {
  open: boolean;
  /** 自定义应用 id（custom:<name>） */
  appId: string;
  /** 当前图标的容器内路径（输入框初值） */
  currentIcon?: string | null;
  onClose: () => void;
  /** 图标已设置/清除（调用方重新拉取列表） */
  onChanged: () => void;
}

interface FsEntry {
  name: string;
  is_dir: boolean;
}

const IMAGE_EXT = new Set([
  'png',
  'jpg',
  'jpeg',
  'svg',
  'ico',
  'webp',
  'gif',
  'bmp',
  'xpm',
  'tif',
  'tiff',
]);

function isImagePath(path: string): boolean {
  return IMAGE_EXT.has(path.split('.').pop()?.toLowerCase() ?? '');
}

export function IconPickerModal({ open, appId, currentIcon, onClose, onChanged }: IconPickerModalProps) {
  // 容器内图片路径输入框（AutoComplete 列目录补全 + 浏览）
  const [containerPath, setContainerPath] = useState('');
  // 补全选项（每键入一次从 fs_list 父目录拉取，前缀过滤）
  const [pathOpts, setPathOpts] = useState<{ value: string; label: React.ReactNode }[]>([]);
  // 选中目录后保持下拉打开 → 续接下一级
  const [acOpen, setAcOpen] = useState(false);
  // 「浏览」：容器内路径选择器弹窗
  const [pickerOpen, setPickerOpen] = useState(false);
  const [preview, setPreview] = useState<string | null>(null);
  const [previewing, setPreviewing] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // 打开时重置
  useEffect(() => {
    if (!open) return;
    setError(null);
    setContainerPath(currentIcon ?? '');
    setPathOpts([]);
    setPreview(null);
    setAcOpen(false);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, currentIcon]);

  /** 补全：fs_list 父目录 → 按已键入前缀过滤 */
  const searchPathOpts = async (value: string) => {
    if (!value) {
      setPathOpts([]);
      return;
    }
    const lastSlash = value.lastIndexOf('/');
    const parent = lastSlash > 0 ? value.slice(0, lastSlash) : '/';
    const prefix = value.slice(lastSlash + 1);
    try {
      const list = await invoke<FsEntry[]>('fs_list', { path: parent });
      setPathOpts(
        list
          .filter((e) => e.name.startsWith(prefix))
          .slice(0, 50)
          .map((e) => {
            const full = parent === '/' ? `/${e.name}` : `${parent}/${e.name}`;
            return {
              value: e.is_dir ? full + '/' : full, // 目录补 / 视觉提示
              label: (
                <div className="host-path-option">
                  <span>{e.is_dir ? '📁' : '📄'}</span>
                  <span className="host-path-name">{e.name}</span>
                  <span className="host-path-tail">{parent === '/' ? '/' : parent}</span>
                </div>
              ),
            };
          }),
      );
    } catch {
      // 静默失败：选项清空即可，不影响用户输入
      setPathOpts([]);
    }
  };

  useEffect(() => {
    void searchPathOpts(containerPath);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [containerPath]);

  /** 容器内图片预览（debounce：键入停顿 500ms 后设 icon:// src，仅图片路径；
   *  <img> 由浏览器经 Tauri 后端 icon:// 协议中转拉取，无需 async invoke） */
  useEffect(() => {
    if (!containerPath || !isImagePath(containerPath)) {
      setPreview(null);
      setPreviewing(false);
      return;
    }
    setPreviewing(true);
    const t = setTimeout(() => {
      setPreview(`icon://localhost${containerPath}`);
      setPreviewing(false);
    }, 500);
    return () => {
      clearTimeout(t);
      setPreviewing(false);
    };
  }, [containerPath]);

  /** AutoComplete 选中：目录 → 保持下拉钻进下一级 */
  const handleSelect = (v: string) => {
    if (v.endsWith('/')) {
      setAcOpen(true);
    }
  };

  /** 「从宿主机选用」：rfd 选图片 → 后端复制进容器 → 路径回填输入框 */
  const pickFromHost = async () => {
    setBusy(true);
    setError(null);
    try {
      const p = await invoke<string>('passthrough_pick_host_icon');
      setContainerPath(p);
    } catch (err: any) {
      const msg = errMsg(err, '选择宿主图标失败');
      if (!msg.includes('已取消')) setError(msg); // 取消不算错误
    } finally {
      setBusy(false);
    }
  };

  /** 确定：写入容器内配置 */
  const confirm = async () => {
    const p = containerPath.replace(/\/+$/, '');
    if (!p) return;
    setBusy(true);
    setError(null);
    try {
      await invoke('passthrough_set_custom_icon', { id: appId, icon: p });
      onChanged();
      onClose();
    } catch (err: any) {
      setError(errMsg(err, '设置图标失败'));
    } finally {
      setBusy(false);
    }
  };

  /** 清除图标 */
  const clearIcon = async () => {
    setBusy(true);
    setError(null);
    try {
      await invoke('passthrough_set_custom_icon', { id: appId, icon: null });
      onChanged();
      onClose();
    } catch (err: any) {
      setError(errMsg(err, '清除图标失败'));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal
      title="选择应用图标"
      open={open}
      onCancel={onClose}
      width={640}
      destroyOnClose
      footer={[
        <Button key="clear" danger disabled={busy} onClick={clearIcon}>
          清除图标
        </Button>,
        <Button key="cancel" onClick={onClose}>
          取消
        </Button>,
        <Button key="ok" type="primary" disabled={!containerPath} loading={busy} onClick={confirm}>
          确定
        </Button>,
      ]}
    >
      <div className="icon-picker">
        <div className="icon-picker-row">
          <div className="icon-picker-container">
            <div className="icon-picker-label" style={{ marginBottom: 6 }}>
              图标（容器内路径，可键入补全，或「浏览」进目录挑选）：
            </div>
            <Space.Compact style={{ width: '100%' }}>
              <AutoComplete
                value={containerPath}
                onChange={(v) => setContainerPath(String(v ?? ''))}
                onSelect={handleSelect}
                open={acOpen}
                onOpenChange={setAcOpen}
                options={pathOpts}
                style={{ flex: 1 }}
                popupMatchSelectWidth={false}
              >
                <Input
                  placeholder="容器内图片路径，如 ~/.easytidy/icons/app.png"
                  allowClear
                  onPressEnter={confirm}
                />
              </AutoComplete>
              <Button
                icon={<FolderOpenOutlined />}
                onClick={() => setPickerOpen(true)}
                title="浏览容器内目录，挑选图片文件"
              >
                浏览
              </Button>
              <Button
                icon={<DownloadOutlined />}
                onClick={pickFromHost}
                loading={busy}
                title="打开宿主原生文件对话框选图片，自动复制进容器"
              >
                从宿主机选用
              </Button>
            </Space.Compact>
          </div>
          <div className="icon-picker-preview">
            {previewing ? (
              <Spin />
            ) : preview ? (
              <img key={containerPath} src={preview} alt="" onError={() => setPreview(null)} />
            ) : (
              <span className="icon-picker-preview-hint">
                {containerPath && !isImagePath(containerPath)
                  ? '当前路径不是图片文件'
                  : '选定容器内图片路径后预览'}
              </span>
            )}
          </div>
        </div>

        {error && (
          <div className="icon-picker-footer">
            <span className="error-message">{error}</span>
          </div>
        )}
      </div>

      {/* 「浏览」：容器内路径选择器（选一个图片文件，路径回填输入框） */}
      <ContainerPathPicker
        open={pickerOpen}
        initialPath={parentDir(containerPath) || '/'}
        onPick={(p) => {
          setContainerPath(p);
        }}
        onClose={() => setPickerOpen(false)}
      />
    </Modal>
  );
}
