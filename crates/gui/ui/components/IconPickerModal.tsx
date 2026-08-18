// 自定义应用图标选择弹窗（两个入口）：
// - 从宿主机选择：rfd 原生文件对话框（passthrough_pick_host_icon）
// - 从容器中选择：容器内迷你文件浏览器（fs_list + fetch_file_b64 预览，
//   socket 拉取——pasta 网络下容器 HTTP 端口宿主不可达）
// 选定后图标数据统一落盘宿主机 ~/.easytidy/icons（import → set_custom_icon），
// 桌面入口 Icon= 用绝对路径直接显示。

import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { errMsg } from '../lib/errors';
import { Modal, Button, Input, Spin, Empty } from 'antd';
import { mimeForPath } from './mime';

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
  size?: number;
}

const IMAGE_EXT = new Set(['png', 'jpg', 'jpeg', 'svg', 'ico', 'webp', 'gif', 'bmp']);

export function IconPickerModal({ open, appId, onClose, onChanged, onPicked }: IconPickerModalProps) {
  // 容器内文件浏览状态
  const [cwd, setCwd] = useState('/');
  const [entries, setEntries] = useState<FsEntry[]>([]);
  const [browsing, setBrowsing] = useState(false);
  const [selected, setSelected] = useState<string | null>(null);
  const [preview, setPreview] = useState<string | null>(null);
  const [previewing, setPreviewing] = useState(false);
  const [navInput, setNavInput] = useState('/');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // 打开时重置并加载容器根目录
  useEffect(() => {
    if (!open) return;
    setError(null);
    setSelected(null);
    setPreview(null);
    setNavInput('/');
    loadDir('/');
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  const loadDir = async (dir: string) => {
    setBrowsing(true);
    try {
      const list = await invoke<FsEntry[]>('fs_list', { path: dir });
      setCwd(dir);
      setNavInput(dir);
      setEntries(list);
      setSelected(null);
      setPreview(null);
    } catch (err: any) {
      setError(errMsg(err, '读取容器目录失败'));
    } finally {
      setBrowsing(false);
    }
  };

  const goUp = () => {
    const parent = cwd === '/' ? '/' : cwd.replace(/\/[^/]*\/?$/, '') || '/';
    loadDir(parent);
  };

  /** 点击图片文件 → 经 server 拉取预览 */
  const previewFile = async (path: string) => {
    setSelected(path);
    setPreviewing(true);
    setPreview(null);
    try {
      const b64 = await invoke<string>('fetch_file_b64', { path });
      setPreview(`data:${mimeForPath(path)};base64,${b64}`);
    } catch {
      setPreview(null); // 非图片/不可读：仅选中，不阻塞
    } finally {
      setPreviewing(false);
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

  /** 入口一：宿主机原生对话框选图标 → 落盘 */
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

  /** 入口二：容器内选定图片 → 拉取落盘 */
  const pickFromContainer = async () => {
    if (!selected) return;
    setBusy(true);
    setError(null);
    try {
      const hostPath = await invoke<string>('passthrough_import_container_icon', {
        containerPath: selected,
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
      width={680}
      destroyOnClose
    >
      <div className="icon-picker">
        <div className="icon-picker-sources">
          <span className="icon-picker-label">图标来源：</span>
          <Button onClick={pickFromHost} loading={busy}>
            从宿主机选择…
          </Button>
          {appId && (
            <Button onClick={clearIcon} disabled={busy}>
              清除图标
            </Button>
          )}
        </div>

        <div className="icon-picker-browser">
          <div className="icon-picker-nav">
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

          {browsing ? (
            <div className="icon-picker-list icon-picker-loading">
              <Spin />
            </div>
          ) : (
            <div className="icon-picker-list">
              {entries.map((e) => {
                const isImage =
                  !e.is_dir &&
                  IMAGE_EXT.has(e.name.split('.').pop()?.toLowerCase() ?? '');
                const full = cwd === '/' ? `/${e.name}` : `${cwd}/${e.name}`;
                return (
                  <div
                    key={full}
                    className={`icon-picker-entry ${selected === full ? 'selected' : ''}`}
                    onClick={() => (e.is_dir ? loadDir(full) : previewFile(full))}
                    title={e.is_dir ? full : `${full}${e.size != null ? `（${e.size} B）` : ''}`}
                  >
                    <span className="icon-picker-entry-icon">
                      {e.is_dir ? '📁' : isImage ? '🖼️' : '📄'}
                    </span>
                    <span className="icon-picker-name">{e.name}</span>
                  </div>
                );
              })}
              {entries.length === 0 && <Empty description="空目录" />}
            </div>
          )}

          <div className="icon-picker-preview">
            {previewing ? (
              <Spin />
            ) : preview ? (
              <img src={preview} alt="" />
            ) : (
              <span className="icon-picker-preview-hint">
                {selected ? '文件无法预览（非图片）' : '选择容器内图片文件以预览'}
              </span>
            )}
          </div>
        </div>

        <div className="icon-picker-footer">
          {error && <span className="error-message">{error}</span>}
          <Button
            type="primary"
            disabled={!selected}
            loading={busy}
            onClick={pickFromContainer}
          >
            使用所选容器图片
          </Button>
        </div>
      </div>
    </Modal>
  );
}
