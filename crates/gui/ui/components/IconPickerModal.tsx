// 自定义应用图标选择弹窗（两个来源入口）：
// - 从宿主机导入：**独立按钮** → rfd 原生文件对话框（passthrough_pick_host_icon）
// - 从容器导入：**路径输入框**（移植 bind mount 的路径输入组件：AutoComplete
//   列目录补全 +「浏览」按钮打开容器内路径选择器）→ 拉取落盘
// 选定后图标数据统一落盘宿主机 ~/.easytidy/icons（import → set_custom_icon），
// 桌面入口 Icon= 用绝对路径直接显示。

import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { errMsg } from '../lib/errors';
import { Modal, Button, Input, Spin, AutoComplete, Space } from 'antd';
import { FolderOpenOutlined } from '@ant-design/icons';
import { mimeForPath } from './mime';
import { ContainerPathPicker, parentDir } from './ContainerPathPicker';

interface IconPickerModalProps {
  open: boolean;
  /** 自定义应用 id（custom:<name>）；null = 独立选取模式：选定后
   *  仅经 onPicked 返回宿主路径，由调用方决定用途（新增应用表单用） */
  appId: string | null;
  onClose: () => void;
  /** 图标已设置/清除（调用方重新拉取列表；appId 模式下） */
  onChanged: () => void;
  /** 独立选取模式：返回落盘后的宿主图标路径 */
  onPicked?: (hostPath: string) => void;
}

interface FsEntry {
  name: string;
  is_dir: boolean;
}

const IMAGE_EXT = new Set(['png', 'jpg', 'jpeg', 'svg', 'ico', 'webp', 'gif', 'bmp']);

function isImagePath(path: string): boolean {
  return IMAGE_EXT.has(path.split('.').pop()?.toLowerCase() ?? '');
}

export function IconPickerModal({ open, appId, onClose, onChanged, onPicked }: IconPickerModalProps) {
  // 容器图片路径输入框（移植自 bind mount 宿主路径输入：AutoComplete + 浏览）
  const [containerPath, setContainerPath] = useState('');
  // 补全选项（每键入一次从 fs_list 父目录拉取，前缀过滤）
  const [pathOpts, setPathOpts] = useState<{ value: string; label: React.ReactNode }[]>([]);
  // 选中目录后保持下拉打开 → 续接下一级（bind mount 同款行为）
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
    setContainerPath('');
    setPathOpts([]);
    setPreview(null);
    setAcOpen(false);
  }, [open]);

  /** 补全：fs_list 父目录 → 按已键入前缀过滤（移植 bind mount 宿主路径补全，
   *  数据源从宿主建议命令换成容器 fs_list） */
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

  /** 容器内图片预览（选中文件 / 浏览返回 / 回车时触发，不随每次键入） */
  const previewOf = async (path: string) => {
    if (!isImagePath(path)) {
      setPreview(null);
      return;
    }
    setPreviewing(true);
    setPreview(null);
    try {
      const b64 = await invoke<string>('fetch_file_b64', { path });
      setPreview(`data:${mimeForPath(path)};base64,${b64}`);
    } catch {
      setPreview(null); // 非图片/不可读：不阻塞
    } finally {
      setPreviewing(false);
    }
  };

  /** AutoComplete 选中：目录 → 保持下拉钻进下一级；文件 → 预览 */
  const handleSelect = (v: string) => {
    if (v.endsWith('/')) {
      setAcOpen(true);
    } else {
      void previewOf(v);
    }
  };

  /** 图标落盘后统一出口：appId 模式写配置；独立模式经 onPicked 交还调用方 */
  const applyPickedIcon = async (hostPath: string) => {
    if (appId) {
      await invoke('passthrough_set_custom_icon', { id: appId, icon: hostPath });
      onChanged();
    } else {
      onPicked?.(hostPath);
    }
    onClose();
  };

  /** 入口一（独立按钮）：宿主原生文件对话框选图片 → 落盘 */
  const pickFromHost = async () => {
    setBusy(true);
    setError(null);
    try {
      const hostPath = await invoke<string>('passthrough_pick_host_icon');
      await applyPickedIcon(hostPath);
    } catch (err: any) {
      setError(errMsg(err, '选择宿主图标失败'));
    } finally {
      setBusy(false);
    }
  };

  /** 入口二（路径输入框）：导入容器内该路径的图片 → 落盘 */
  const importContainer = async () => {
    const p = containerPath.replace(/\/+$/, '');
    if (!p) return;
    setBusy(true);
    setError(null);
    try {
      const hostPath = await invoke<string>('passthrough_import_container_icon', {
        containerPath: p,
      });
      await applyPickedIcon(hostPath);
    } catch (err: any) {
      setError(errMsg(err, '导入容器图标失败'));
    } finally {
      setBusy(false);
    }
  };

  const clearIcon = async () => {
    if (!appId) return; // 独立模式：清除由调用方处理
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
      footer={null}
      width={640}
      destroyOnClose
    >
      <div className="icon-picker">
        <div className="icon-picker-sources">
          <span className="icon-picker-label">图标来源：</span>
          <Button onClick={pickFromHost} loading={busy} title="打开宿主原生文件对话框选择图片">
            从宿主机导入
          </Button>
          {appId && (
            <Button onClick={clearIcon} disabled={busy}>
              清除图标
            </Button>
          )}
        </div>

        <div className="icon-picker-row">
          <div className="icon-picker-container">
            <div className="icon-picker-label" style={{ marginBottom: 6 }}>
              容器内图片路径（可键入补全，或「浏览」进目录挑选）：
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
                  placeholder="容器内图片路径，如 /usr/share/icons/…"
                  allowClear
                  onPressEnter={() =>
                    containerPath && void previewOf(containerPath.replace(/\/+$/, ''))
                  }
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
                type="primary"
                disabled={!containerPath}
                loading={busy}
                onClick={importContainer}
              >
                导入
              </Button>
            </Space.Compact>
          </div>
          <div className="icon-picker-preview">
            {previewing ? (
              <Spin />
            ) : preview ? (
              <img src={preview} alt="" />
            ) : (
              <span className="icon-picker-preview-hint">
                {containerPath && !isImagePath(containerPath)
                  ? '当前路径不是图片文件'
                  : '选定容器内图片文件后预览'}
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
          void previewOf(p);
        }}
        onClose={() => setPickerOpen(false)}
      />
    </Modal>
  );
}
