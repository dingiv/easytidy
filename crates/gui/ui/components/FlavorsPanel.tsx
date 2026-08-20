// 模板管理（主 GUI 独立 tab）：flavor 启动配置模板。
//
// flavor = "将一个镜像 run 起来"需要的完整配置清单：镜像 + GUI 透传 +
// setup 安装 + entry 应用 + 挂载 + 网络。预设模板让用户一键拉起预配置
// 容器（~/.easytidy/flavors/*.toml）。
//
// 与容器管理分离：模板是配置的批量管理层（存意图），容器是实例（存
// 快照）。「启动」把模板预填进创建表单——经 onLaunch 回调切到容器 tab
// 并打开页内创建表单（Centralized 持有 createRequest 触发器）。
//
// 血缘：派生计数 + 「同步派生」批量重展开（config ← flavor）。

import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { errMsg } from '../lib/errors';
import {
  App as AntApp,
  Alert,
  Button,
  Input,
  Modal,
  Select,
  Space,
  Spin,
  Switch,
  Tag,
  Tooltip,
  Typography,
} from 'antd';
import {
  DeleteOutlined,
  EditOutlined,
  PlusOutlined,
  ReloadOutlined,
  RocketOutlined,
  SyncOutlined,
} from '@ant-design/icons';
import type { Flavor, MountConfig } from '../types';
import './FlavorsPanel.css';

/** 空白 flavor（新建表单初始值；user_home 与 Rust 共享基座对齐：bool 默认 true） */
function emptyFlavor(): Flavor {
  return {
    name: '',
    image: 'docker.io/library/ubuntu:24.04',
    gui: true,
    setup: [],
    entry: null,
    entry_args: [],
    mounts: [],
    user_home: true,
    network: { mode: 'host', ports: [] },
  };
}

/** mounts 行文本（"host:container[:ro]"）→ MountConfig */
function parseMountLine(line: string): MountConfig | null {
  const parts = line.split(':').map((p) => p.trim());
  if (parts.length < 2 || !parts[0] || !parts[1]) return null;
  // 末段 "ro" = 只读（其余段合并为容器路径）
  const ro = parts.length >= 3 && parts[parts.length - 1] === 'ro';
  const container = ro ? parts.slice(1, -1).join(':') : parts.slice(1).join(':');
  return { host_path: parts[0], container_path: container, read_only: ro };
}

/** MountConfig → 行文本 */
function mountToLine(m: MountConfig): string {
  return `${m.host_path}:${m.container_path}${m.read_only ? ':ro' : ''}`;
}

interface FlavorsPanelProps {
  /** 模板卡「启动」：切到容器 tab 并打开页内创建表单，预选该模板 */
  onLaunch(flavor: string): void;
}

function FlavorsPanelInner({ onLaunch }: FlavorsPanelProps) {
  const { message, modal } = AntApp.useApp();

  const [flavors, setFlavors] = useState<Flavor[]>([]);
  const [lineage, setLineage] = useState<Record<string, string[]>>({});
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [syncingFlavor, setSyncingFlavor] = useState<string | null>(null);

  // flavor 编辑器（模板表单）
  const [editing, setEditing] = useState<Flavor | null>(null);
  const [isNew, setIsNew] = useState(false);
  const [setupText, setSetupText] = useState('');
  const [mountsText, setMountsText] = useState('');
  const [saving, setSaving] = useState(false);

  const load = async () => {
    setLoading(true);
    setError(null);
    try {
      const [flavorList, lineageMap] = await Promise.all([
        invoke<Flavor[]>('flavor_list_detailed'),
        invoke<Record<string, string[]>>('flavor_lineage'),
      ]);
      setFlavors(flavorList);
      setLineage(lineageMap ?? {});
    } catch (err: any) {
      setError(errMsg(err, '加载模板失败'));
      console.error('load (flavors) failed:', err);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    load();
  }, []);

  /** 模板编辑器打开（flavor=null → 新建） */
  const openEditor = (flavor: Flavor | null) => {
    const f = flavor ?? emptyFlavor();
    setEditing(f);
    setIsNew(flavor === null);
    setSetupText(f.setup.join('\n'));
    setMountsText(f.mounts.map(mountToLine).join('\n'));
  };

  const handleSave = async () => {
    const f = editing;
    if (!f) return;
    if (!f.name.trim() || !f.image.trim()) {
      message.warning('名称与镜像不能为空');
      return;
    }
    setSaving(true);
    try {
      const toSave: Flavor = {
        ...f,
        name: f.name.trim(),
        image: f.image.trim(),
        setup: setupText.split('\n').map((s) => s.trim()).filter(Boolean),
        mounts: mountsText
          .split('\n')
          .map((l) => l.trim())
          .filter(Boolean)
          .map(parseMountLine)
          .filter((m): m is MountConfig => m !== null),
      };
      await invoke('flavor_save', { flavor: toSave });
      message.success(`模板已保存：${toSave.name}`);
      setEditing(null);
      await load();
    } catch (err: any) {
      message.error(errMsg(err, '保存失败'));
      console.error('flavor_save failed:', err);
    } finally {
      setSaving(false);
    }
  };

  const handleDelete = (f: Flavor) => {
    modal.confirm({
      title: `删除模板 ${f.name}?`,
      content: '已创建的容器不受影响，仅删除模板。',
      okText: '删除',
      okButtonProps: { danger: true },
      cancelText: '取消',
      onOk: async () => {
        try {
          await invoke('flavor_delete', { name: f.name });
          message.success(`已删除：${f.name}`);
          await load();
        } catch (err: any) {
          message.error(errMsg(err, '删除失败'));
        }
      },
    });
  };

  /** 批量同步：把模板当前声明重新展开到全部派生容器（逐个重建） */
  const handleSyncAll = (f: Flavor) => {
    const derived = lineage[f.name] ?? [];
    if (derived.length === 0) return;
    modal.confirm({
      title: `按模板「${f.name}」重新同步全部派生容器？`,
      content: (
        <div>
          <p>以下 {derived.length} 个容器将按模板当前声明重新展开并逐个重建（本地的自启/常驻设置保留，其余本地修改被模板覆盖）：</p>
          <p style={{ paddingLeft: 12 }}>{derived.join('、')}</p>
        </div>
      ),
      okText: '全部重新同步',
      okButtonProps: { danger: true },
      cancelText: '取消',
      onOk: async () => {
        setSyncingFlavor(f.name);
        const failures: string[] = [];
        try {
          for (const name of derived) {
            try {
              await invoke('config_sync_from_flavor', { name });
            } catch (err: any) {
              failures.push(`${name}：${errMsg(err)}`);
            }
          }
          if (failures.length > 0) {
            modal.error({
              title: `同步完成，${failures.length} 个失败`,
              width: 620,
              content: <pre className="error-detail">{failures.join('\n\n')}</pre>,
              okText: '知道了',
            });
          } else {
            message.success(`已按模板「${f.name}」同步 ${derived.length} 个容器`);
          }
          await load();
        } finally {
          setSyncingFlavor(null);
        }
      },
    });
  };

  return (
    <div className="flavors-panel">
      <div className="panel-header">
        <Typography.Title level={4} className="panel-title">
          模板
        </Typography.Title>
        <Space>
          <Button icon={<ReloadOutlined />} onClick={load} loading={loading}>
            刷新
          </Button>
          <Button type="primary" icon={<PlusOutlined />} onClick={() => openEditor(null)}>
            新建模板
          </Button>
        </Space>
      </div>

      <p className="panel-hint">
        模板描述「将一个镜像 run 起来」的完整配置清单（GUI 透传 / 安装命令 /
        entry 应用 / 挂载 / 网络），「启动」一键展开创建预配置容器。
      </p>

      {error && (
        <Alert
          type="error"
          showIcon
          message="操作失败"
          description={error}
          closable
          onClose={() => setError(null)}
        />
      )}

      {loading ? (
        <div className="flavors-loading">
          <Spin tip="加载模板…" size="large">
            <div className="spin-block" />
          </Spin>
        </div>
      ) : (
        <div className="flavor-list">
          {flavors.map((f) => {
            const derived = lineage[f.name] ?? [];
            return (
              <div key={f.name} className="flavor-item">
                <div className="flavor-main">
                  <div className="flavor-title">
                    <span className="flavor-name">{f.name}</span>
                    {f.gui && <Tag color="blue">GUI</Tag>}
                    {f.entry && <Tag color="green">entry: {f.entry}</Tag>}
                    {f.setup.length > 0 && <Tag>setup ×{f.setup.length}</Tag>}
                    {f.mounts.length > 0 && <Tag>mounts ×{f.mounts.length}</Tag>}
                    <Tag>{f.network.mode === 'host' ? 'host 网络' : 'bridge'}</Tag>
                    {derived.length > 0 && (
                      <Tooltip title={`派生容器：${derived.join('、')}`}>
                        <Tag color="purple">派生 ×{derived.length}</Tag>
                      </Tooltip>
                    )}
                  </div>
                  <div className="flavor-image">{f.image}</div>
                </div>
                <div className="flavor-actions">
                  <Tooltip title="按模板创建容器（切到容器 tab，输入名称后自动展开预填）">
                    <Button type="primary" icon={<RocketOutlined />} onClick={() => onLaunch(f.name)}>
                      启动
                    </Button>
                  </Tooltip>
                  {derived.length > 0 && (
                    <Tooltip title={`按模板当前声明重新同步全部派生容器（${derived.join('、')}）`}>
                      <Button
                        icon={<SyncOutlined />}
                        loading={syncingFlavor === f.name}
                        onClick={() => handleSyncAll(f)}
                      >
                        同步派生
                      </Button>
                    </Tooltip>
                  )}
                  <Button icon={<EditOutlined />} onClick={() => openEditor(f)} title="编辑" />
                  <Button danger icon={<DeleteOutlined />} onClick={() => handleDelete(f)} title="删除" />
                </div>
              </div>
            );
          })}
          {flavors.length === 0 && (
            <div className="empty-message">暂无模板，点击「新建模板」创建启动配置。</div>
          )}
        </div>
      )}

      {/* flavor 编辑器（模板表单） */}
      <Modal
        title={isNew ? '新建模板' : `编辑 ${editing?.name}`}
        open={editing !== null}
        onCancel={() => setEditing(null)}
        onOk={handleSave}
        okText="保存"
        cancelText="取消"
        confirmLoading={saving}
        width={640}
      >
        {editing && (
          <div className="flavor-form">
            <div className="form-row">
              <label>名称</label>
              <Input
                value={editing.name}
                onChange={(e) => setEditing({ ...editing, name: e.target.value })}
                placeholder="如 chrome、dev-gui"
                disabled={!isNew}
              />
            </div>
            <div className="form-row">
              <label>镜像</label>
              <Input
                value={editing.image}
                onChange={(e) => setEditing({ ...editing, image: e.target.value })}
                placeholder="docker.io/library/ubuntu:24.04"
              />
            </div>
            <div className="form-row">
              <label>GUI 透传</label>
              <Switch
                checked={editing.gui}
                onChange={(v) => setEditing({ ...editing, gui: v })}
              />
              <span className="form-hint">注入宿主显示环境 + 字体/图标 + 用户映射</span>
            </div>
            <div className="form-row">
              <label>entry 应用</label>
              <Input
                value={editing.entry ?? ''}
                onChange={(e) => setEditing({ ...editing, entry: e.target.value || null })}
                placeholder="容器内可执行名（可留空）"
              />
            </div>
            <div className="form-row">
              <label>entry 参数</label>
              <Input
                value={editing.entry_args.join(' ')}
                onChange={(e) =>
                  setEditing({
                    ...editing,
                    entry_args: e.target.value.split(/\s+/).filter(Boolean),
                  })
                }
                placeholder="空格分隔"
              />
            </div>
            <div className="form-row">
              <label>安装命令</label>
              <Input.TextArea
                rows={3}
                value={setupText}
                onChange={(e) => setSetupText(e.target.value)}
                placeholder={'每行一条，按序执行\n如：apt-get update -qq && apt-get install -y -qq sudo'}
              />
            </div>
            <div className="form-row">
              <label>额外挂载</label>
              <Input.TextArea
                rows={2}
                value={mountsText}
                onChange={(e) => setMountsText(e.target.value)}
                placeholder={'每行一条：宿主路径:容器路径[:ro]'}
              />
            </div>
            <div className="form-row">
              <label>网络模式</label>
              <Select
                value={editing.network.mode}
                onChange={(v) =>
                  setEditing({ ...editing, network: { ...editing.network, mode: v } })
                }
                style={{ width: 160 }}
                options={[
                  { value: 'host', label: 'host（宿主网络）' },
                  { value: 'mapped', label: 'bridge（默认）' },
                ]}
              />
            </div>
          </div>
        )}
      </Modal>
    </div>
  );
}

/** antd App 包裹（message/modal 继承暗色主题与中文 locale） */
export function FlavorsPanel(props: FlavorsPanelProps) {
  return (
    <AntApp>
      <FlavorsPanelInner {...props} />
    </AntApp>
  );
}
