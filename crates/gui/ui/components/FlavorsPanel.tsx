// flavor 启动配置管理（主 GUI）。
//
// flavor = "将一个镜像 run 起来"需要向 easytidy 传递的完整配置清单：
// 镜像 + GUI 透传 + setup 安装 + entry 应用 + 挂载 + 网络 + 用户映射。
// 预设模板让用户一键拉起 GUI 容器（详见 ~/.easytidy/flavors/*.toml）。

import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { App as AntApp, Button, Input, Modal, Select, Switch, Tag, Tooltip } from 'antd';
import {
  DeleteOutlined,
  EditOutlined,
  PlusOutlined,
  RocketOutlined,
  SyncOutlined,
} from '@ant-design/icons';
import type { Flavor, MountConfig } from '../types';

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

export function FlavorsPanel() {
  const { message, modal } = AntApp.useApp();
  const [flavors, setFlavors] = useState<Flavor[]>([]);
  const [loading, setLoading] = useState(true);
  // 血缘：每个模板派生了哪些容器（批量同步入口）
  const [lineage, setLineage] = useState<Record<string, string[]>>({});
  const [syncingFlavor, setSyncingFlavor] = useState<string | null>(null);
  // 编辑/新建表单
  const [editing, setEditing] = useState<Flavor | null>(null);
  const [isNew, setIsNew] = useState(false);
  const [setupText, setSetupText] = useState('');
  const [mountsText, setMountsText] = useState('');
  const [saving, setSaving] = useState(false);
  // 一键创建容器
  const [creating, setCreating] = useState<Flavor | null>(null);
  const [createName, setCreateName] = useState('');
  const [creatingBusy, setCreatingBusy] = useState(false);

  const load = async () => {
    setLoading(true);
    try {
      const [list, lineageMap] = await Promise.all([
        invoke<Flavor[]>('flavor_list_detailed'),
        invoke<Record<string, string[]>>('flavor_lineage'),
      ]);
      setFlavors(list);
      setLineage(lineageMap ?? {});
    } catch (err: any) {
      message.error(err?.message || '读取 flavor 列表失败');
      console.error('flavor_list_detailed failed:', err);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    load();
  }, []);

  /** 打开编辑表单（flavor=null → 新建） */
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
      message.success(`flavor 已保存：${toSave.name}`);
      setEditing(null);
      await load();
    } catch (err: any) {
      message.error(err?.message || '保存失败');
      console.error('flavor_save failed:', err);
    } finally {
      setSaving(false);
    }
  };

  const handleDelete = (f: Flavor) => {
    modal.confirm({
      title: `删除 flavor ${f.name}?`,
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
          message.error(err?.message || '删除失败');
        }
      },
    });
  };

  /** 从 flavor 一键创建容器：展开 → env_new(完整 config)（统一创建入口） */
  const handleCreate = async () => {
    const f = creating;
    if (!f) return;
    const name = createName.trim() || f.name;
    setCreatingBusy(true);
    try {
      // 预检容器名冲突（同名容器已存在时 podman create 必失败，提前给出可读错误）
      const containers = await invoke<{ name: string }[]>('list_containers');
      if (containers.some((c) => c.name === name)) {
        message.error(`容器名「${name}」已存在，请换一个名字`);
        return;
      }
      // 宿主侧展开（GUI 透传 env / 字体挂载）→ 统一入口提交完整 ContainerConfig
      const config = await invoke<Record<string, unknown>>('flavor_expand', {
        name,
        flavor: f.name,
      });
      await invoke('env_new', { config: { ...config, name } });
      message.success(`容器 ${name} 已创建并启动（来自 flavor ${f.name}）`);
      setCreating(null);
      setCreateName('');
      await load();
    } catch (err: any) {
      // 完整报错展示：容器创建/启动链路多步（镜像检查→烘焙→创建→启动→setup），
      // 任何一步都可能失败——toast 会消失且截断，用 Modal 展示全文（可选中复制）
      const msg = typeof err === 'string' ? err : err?.message || JSON.stringify(err);
      console.error('env_new (flavor) failed:', err);
      modal.error({
        title: `创建容器「${name}」失败`,
        width: 620,
        content: <pre className="error-detail">{msg}</pre>,
        okText: '知道了',
      });
    } finally {
      setCreatingBusy(false);
    }
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
              failures.push(`${name}：${err?.message || err}`);
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
        <h2>Flavor 启动配置</h2>
        <div className="header-actions">
          <Button icon={<PlusOutlined />} onClick={() => openEditor(null)}>
            新建
          </Button>
        </div>
      </div>

      <p className="panel-hint">
        flavor 描述「将一个镜像 run 起来」的完整配置清单（GUI 透传 / 安装命令 /
        entry 应用 / 挂载 / 网络），一键从模板创建预配置容器。
      </p>

      {loading ? (
        <div className="loading">Loading flavors...</div>
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
                <Tooltip title="从模板创建容器">
                  <Button
                    type="primary"
                    icon={<RocketOutlined />}
                    onClick={() => {
                      setCreating(f);
                      setCreateName(f.name);
                    }}
                  >
                    创建容器
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
            <div className="empty-message">暂无 flavor，点击「新建」创建启动配置模板。</div>
          )}
        </div>
      )}

      {/* 编辑/新建表单 */}
      <Modal
        title={isNew ? '新建 Flavor' : `编辑 ${editing?.name}`}
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

      {/* 一键创建容器 */}
      <Modal
        title={`从 flavor「${creating?.name}」创建容器`}
        open={creating !== null}
        onCancel={() => !creatingBusy && setCreating(null)}
        onOk={handleCreate}
        okText="创建并启动"
        cancelText="取消"
        confirmLoading={creatingBusy}
      >
        <p className="dialog-hint">
          将按模板展开完整配置创建容器（<code>{creating?.image}</code>），创建后自动启动。
          <br />
          ⚠️ 请确保基础镜像已在「镜像」面板拉取，容器名不能与现有容器重复。
        </p>
        <Input
          value={createName}
          onChange={(e) => setCreateName(e.target.value)}
          placeholder="容器名（默认 = flavor 名）"
          autoFocus
        />
      </Modal>
    </div>
  );
}
