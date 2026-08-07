// 纯文本编辑器（面板视图）：fs_read 读容器内文件 → textarea 编辑 → 保存回写。
// 传输经 server fs.read/fs.write（base64；UTF-8 安全编解码）。

import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { App as AntApp, Button, Space, Spin, Typography } from 'antd';
import { SaveOutlined, CloseOutlined } from '@ant-design/icons';

interface FileEditorProps {
  path: string;
  onClose(): void;
  onSaved(): void;
}

/** UTF-8 安全 base64 编码（btoa 仅 Latin-1，中文等会损坏） */
function textToB64(text: string): string {
  const bytes = new TextEncoder().encode(text);
  let bin = '';
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin);
}

/** UTF-8 安全 base64 解码 */
function b64ToText(b64: string): string {
  const bin = atob(b64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return new TextDecoder().decode(bytes);
}

export function FileEditor({ path, onClose, onSaved }: FileEditorProps) {
  const { message } = AntApp.useApp();
  const [text, setText] = useState('');
  const [saving, setSaving] = useState(false);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setLoadError(null);
    invoke<string>('fs_read', { path })
      .then((b64) => {
        if (cancelled) return;
        try {
          setText(b64ToText(b64));
        } catch (e) {
          setLoadError(`文件解码失败（可能不是文本）：${e}`);
        }
      })
      .catch((err: any) => {
        if (!cancelled) {
          setLoadError(err?.message || '读取文件失败');
          console.error('fs_read failed:', err);
        }
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [path]);

  const fileName = path.split('/').filter(Boolean).pop() ?? path;

  const handleSave = async () => {
    setSaving(true);
    try {
      await invoke('fs_write', {
        path,
        // Tauri 2 invoke 参数 camelCase（Rust data_b64 ↔ 前端 dataB64）
        dataB64: textToB64(text),
      });
      message.success('已保存');
      onSaved();
    } catch (err: any) {
      message.error(err?.message || '保存失败');
      console.error('fs_write failed:', err);
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="file-editor">
      <div className="file-editor-header">
        <Typography.Text ellipsis style={{ fontSize: '0.9rem' }}>
          <Typography.Text code>{fileName}</Typography.Text>
          <Typography.Text type="secondary" style={{ marginLeft: 8, fontSize: '0.75rem' }}>
            {path}
          </Typography.Text>
        </Typography.Text>
        <Space>
          <Button
            size="small"
            type="primary"
            icon={<SaveOutlined />}
            onClick={handleSave}
            loading={saving}
            disabled={loading || !!loadError}
          >
            保存
          </Button>
          <Button size="small" icon={<CloseOutlined />} onClick={onClose}>
            关闭
          </Button>
        </Space>
      </div>
      {loading ? (
        <div className="file-editor-loading">
          <Spin tip="加载文件…" size="large">
            <div className="spin-block" />
          </Spin>
        </div>
      ) : loadError ? (
        <div className="error-message" style={{ padding: '1rem' }}>
          {loadError}
        </div>
      ) : (
        <textarea
          className="file-editor-textarea"
          value={text}
          onChange={(e) => setText(e.target.value)}
          spellCheck={false}
        />
      )}
    </div>
  );
}
