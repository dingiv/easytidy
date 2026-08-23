// 模板管理（主 GUI 浏览器式面板之一）：conf YAML 启动配置模板。
//
// conf 模板 = 容器关键参数(镜像/entry/挂载/网络/用户映射)+ 可选 setup 安装命令。
// 存放:`~/.easytidy/conf/<name>.yaml`（运行时权威；源码 crates/gui/conf/*.yaml
// 编译期打进首跑播种）。本面板数据源全部从 conf 目录读取，模板 tab 不再展示
// TOML flavor 预设（已退役，CLI flavor apply 仍可访问 ~/.easytidy/flavors/）。
//
// 与容器管理分离：模板是配置的批量管理层（存意图），容器是实例（存
// 快照）。每卡片操作：使用（预填创建）/ 编辑 / 复制 / 删除；列表支持
// 多选 → 批量删除。血缘：派生计数 + 「同步派生」（次级入口）。
// 「使用」经 onLaunch 回调让 MasterView 打开 `new-container` pane + 预填。

import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { errMsg } from '../lib/errors';
import {
  App as AntApp,
  Alert,
  Button,
  Checkbox,
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
  CopyOutlined,
  DeleteOutlined,
  EditOutlined,
  PlusOutlined,
  ReloadOutlined,
  RocketOutlined,
  SyncOutlined,
} from '@ant-design/icons';
import type { ConfTemplate, MountConfig } from '../types';
import { BLANK_CONTAINER_CONFIG } from './config/ContainerConfigEditor';
import './FlavorsPanel.css';

/** 空白 conf 模板（新建表单初始值；user_home 默认 true 与 Rust 共享基座对齐） */
function emptyConfTemplate(): ConfTemplate {
  return {
    ...BLANK_CONTAINER_CONFIG,
    gui: false,
    setup: [],
  } as ConfTemplate;
}

/** 生成不冲突的复制名：`base-copy`，冲突则 `base-copy2`、`base-copy3`… */
function nextCopyName(base: string, existing: string[]): string {
  const candidate = `${base}-copy`;
  if (!existing.includes(candidate)) return candidate;
  let i = 2;
  while (existing.includes(`${base}-copy${i}`)) i++;
  return `${base}-copy${i}`;
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
  /** 模板卡「使用」：打开配置编辑器并预填该模板（创建新容器） */
  onLaunch(templateName: string): void;
}

function FlavorsPanelInner({ onLaunch }: FlavorsPanelProps) {
  const { message, modal } = AntApp.useApp();

  const [templates, setTemplates] = useState<ConfTemplate[]>([]);
  const [lineage, setLineage] = useState<Record<string, string[]>>({});
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [syncingTemplate, setSyncingTemplate] = useState<string | null>(null);

  // conf 模板编辑器（模板表单）
  const [editing, setEditing] = useState<ConfTemplate | null>(null);
  const [isNew, setIsNew] = useState(false);
  const [setupText, setSetupText] = useState('');
  const [mountsText, setMountsText] = useState('');
  const [saving, setSaving] = useState(false);

  // 多选（批量删除）
  const [selected, setSelected] = useState<Set<string>>(new Set());

  const load = async () => {
    setLoading(true);
    setError(null);
    try {
      const [templateList, lineageMap] = await Promise.all([
        invoke<ConfTemplate[]>('conf_templates'),
        invoke<Record<string, string[]>>('template_lineage'),
      ]);
      setTemplates(templateList);
      setLineage(lineageMap ?? {});
    } catch (err: any) {
      setError(errMsg(err, '加载模板失败'));
      console.error('load (conf templates) failed:', err);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    load();
  }, []);

  /** 模板编辑器打开（t=null → 新建） */
  const openEditor = (t: ConfTemplate | null) => {
    const f = t ?? emptyConfTemplate();
    setEditing(f);
    setIsNew(t === null);
    setSetupText(f.setup.join('\n'));
    setMountsText(f.mounts.map(mountToLine).join('\n'));
  };

  const handleSave = async () => {
    const t = editing;
    if (!t) return;
    if (!t.name.trim() || !t.image.trim()) {
      message.warning('名称与镜像不能为空');
      return;
    }
    setSaving(true);
    try {
      const toSave: ConfTemplate = {
        ...t,
        name: t.name.trim(),
        image: t.image.trim(),
        setup: setupText.split('\n').map((s) => s.trim()).filter(Boolean),
      };
      // mountsText → mounts（mountsText 是局部分离编辑态）
      const parsedMounts = mountsText
        .split('\n')
        .map((l) => l.trim())
        .filter(Boolean)
        .map(parseMountLine)
        .filter((m): m is MountConfig => m !== null);
      toSave.mounts = parsedMounts;
      await invoke('conf_save_template', { template: toSave });
      message.success(`模板已保存：${toSave.name}`);
      setEditing(null);
      await load();
    } catch (err: any) {
      message.error(errMsg(err, '保存失败'));
      console.error('conf_save_template failed:', err);
    } finally {
      setSaving(false);
    }
  };

  const handleDelete = (t: ConfTemplate) => {
    modal.confirm({
      title: `删除模板 ${t.name}?`,
      content: '已创建的容器不受影响（血缘仍保留），仅删除模板。',
      okText: '删除',
      okButtonProps: { danger: true },
      cancelText: '取消',
      onOk: async () => {
        try {
          await invoke('conf_rm_template', { name: t.name });
          message.success(`已删除：${t.name}`);
          await load();
        } catch (err: any) {
          message.error(errMsg(err, '删除失败'));
        }
      },
    });
  };

  const toggleSelect = (name: string) =>
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(name)) next.delete(name);
      else next.add(name);
      return next;
    });
  const clearSelection = () => setSelected(new Set());
  const selectedNames = () => templates.filter((t) => selected.has(t.name)).map((t) => t.name);
  const allSelected = templates.length > 0 && templates.every((t) => selected.has(t.name));

  const handleToggleSelectAll = () => {
    if (allSelected) clearSelection();
    else setSelected(new Set(templates.map((t) => t.name)));
  };

  /** 复制：一键生成不冲突的「name-copy」→ 后端复制 → 刷新列表 */
  const handleCopy = async (t: ConfTemplate) => {
    const to = nextCopyName(t.name, templates.map((x) => x.name));
    try {
      await invoke('conf_duplicate_template', { from: t.name, to });
      message.success(`已复制为「${to}」`);
      await load();
    } catch (err: any) {
      message.error(errMsg(err, '复制失败'));
    }
  };

  /** 批量删除（多选动作；确认框列名字） */
  const handleBatchDelete = () => {
    const names = selectedNames();
    if (names.length === 0) return;
    modal.confirm({
      title: `删除所选 ${names.length} 个模板?`,
      content: (
        <div>
          <p>以下模板将被删除（已创建的容器不受影响，仅删模板）：</p>
          <p style={{ paddingLeft: 12 }}>{names.join('、')}</p>
        </div>
      ),
      okText: '删除',
      okButtonProps: { danger: true },
      cancelText: '取消',
      onOk: async () => {
        const failures: string[] = [];
        for (const name of names) {
          try {
            await invoke('conf_rm_template', { name });
          } catch (err: any) {
            failures.push(`${name}：${errMsg(err)}`);
          }
        }
        clearSelection();
        await load();
        if (failures.length > 0) {
          modal.error({
            title: `删除完成，${failures.length} 个失败`,
            width: 620,
            content: <pre className="error-detail">{failures.join('\n\n')}</pre>,
            okText: '知道了',
          });
        } else {
          message.success(`已删除 ${names.length} 个模板`);
        }
      },
    });
  };

  /** 批量同步：把模板当前声明重新展开到全部派生容器（逐个重建） */
  const handleSyncAll = (t: ConfTemplate) => {
    const derived = lineage[t.name] ?? [];
    if (derived.length === 0) return;
    modal.confirm({
      title: `按模板「${t.name}」重新同步全部派生容器？`,
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
        setSyncingTemplate(t.name);
        const failures: string[] = [];
        try {
          for (const name of derived) {
            try {
              await invoke('config_sync_from_template', { name });
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
            message.success(`已按模板「${t.name}」同步 ${derived.length} 个容器`);
          }
          await load();
        } finally {
          setSyncingTemplate(null);
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
        模板描述「将一个镜像 run 起来」的完整配置清单（镜像 / entry 应用 / 挂载 / 网络 /
        用户映射 / setup 安装命令），存放于 <code>~/.easytidy/conf/</code>。
        「使用」一键打开配置编辑器并预填该模板；列表支持多选后批量删除。
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
        <div className="flavor-panel-body">
          {templates.length > 0 && (
            <div className="flavor-toolbar">
              <Checkbox
                checked={allSelected}
                indeterminate={selected.size > 0 && !allSelected}
                onChange={handleToggleSelectAll}
              >
                全选
              </Checkbox>
              {selected.size > 0 && (
                <Space className="flavor-batch">
                  <Typography.Text type="secondary">已选 {selected.size} 个</Typography.Text>
                  <Button
                    size="small"
                    danger
                    icon={<DeleteOutlined />}
                    onClick={handleBatchDelete}
                  >
                    批量删除
                  </Button>
                  <Button size="small" onClick={clearSelection}>
                    取消选择
                  </Button>
                </Space>
              )}
            </div>
          )}
          <div className="flavor-list">
          {templates.map((t) => {
            const derived = lineage[t.name] ?? [];
            return (
              <div
                key={t.name}
                className={`flavor-item${selected.has(t.name) ? ' selected' : ''}`}
              >
                <Checkbox
                  className="flavor-check"
                  checked={selected.has(t.name)}
                  onChange={() => toggleSelect(t.name)}
                  title={`选择 ${t.name}`}
                />
                <div className="flavor-main">
                  <div className="flavor-title">
                    <span className="flavor-name">{t.name}</span>
                    {t.entry && <Tag color="green">entry: {t.entry}</Tag>}
                    {t.gui && <Tag color="blue">GUI 透传</Tag>}
                    {t.setup.length > 0 && <Tag color="purple">setup ×{t.setup.length}</Tag>}
                    {t.mounts.length > 0 && (
                      <Tag>mounts ×{t.mounts.length}</Tag>
                    )}
                    <Tag>{t.network.mode === 'host' ? 'host 网络' : 'bridge'}</Tag>
                    {derived.length > 0 && (
                      <Tooltip title={`派生容器：${derived.join('、')}`}>
                        <Tag color="gold">派生 ×{derived.length}</Tag>
                      </Tooltip>
                    )}
                  </div>
                  <div className="flavor-image">{t.image}</div>
                </div>
                <div className="flavor-actions">
                  <Tooltip title="使用模板创建容器（打开配置编辑器并预填，可继续修改）">
                    <Button
                      type="primary"
                      icon={<RocketOutlined />}
                      onClick={() => onLaunch(t.name)}
                    >
                      使用
                    </Button>
                  </Tooltip>
                  <Button
                    icon={<EditOutlined />}
                    onClick={() => openEditor(t)}
                    title="编辑"
                  />
                  <Button
                    icon={<CopyOutlined />}
                    onClick={() => handleCopy(t)}
                    title="复制"
                  />
                  {derived.length > 0 && (
                    <Tooltip
                      title={`按模板当前声明重新同步全部派生容器（${derived.join(
                        '、',
                      )}）`}
                    >
                      <Button
                        icon={<SyncOutlined />}
                        loading={syncingTemplate === t.name}
                        onClick={() => handleSyncAll(t)}
                        title="同步派生"
                      />
                    </Tooltip>
                  )}
                  <Button
                    danger
                    icon={<DeleteOutlined />}
                    onClick={() => handleDelete(t)}
                    title="删除"
                  />
                </div>
              </div>
            );
          })}
          {templates.length === 0 && (
            <div className="empty-message">暂无模板，点击「新建模板」创建启动配置。</div>
          )}
          </div>
        </div>
      )}

      {/* conf 模板编辑器（模板表单） */}
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
                onChange={(e) =>
                  setEditing({ ...editing, image: e.target.value })
                }
                placeholder="docker.io/library/ubuntu:24.04"
              />
            </div>
            <div className="form-row">
              <label>GUI 透传</label>
              <Switch
                checked={editing.gui}
                onChange={(v) => setEditing({ ...editing, gui: v })}
              />
              <span className="form-hint">
                展开时按宿主实时 env 注入 DISPLAY/WAYLAND/XAUTHORITY/XDG_RUNTIME_DIR + 字体图标挂载
              </span>
            </div>
            <div className="form-row">
              <label>entry 应用</label>
              <Input
                value={editing.entry ?? ''}
                onChange={(e) =>
                  setEditing({ ...editing, entry: e.target.value || null })
                }
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
              <label>安装命令（setup）</label>
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
                  setEditing({
                    ...editing,
                    network: { ...editing.network, mode: v },
                  })
                }
                style={{ width: 160 }}
                options={[
                  { value: 'host', label: 'host（宿主网络）' },
                  { value: 'mapped', label: 'bridge（默认）' },
                ]}
              />
            </div>
            <Typography.Text type="secondary" style={{ fontSize: 12 }}>
              setup 安装命令本轮只存不执行（执行链路与 data 启动脚本一起留待下一步）。
            </Typography.Text>
          </div>
        )}
      </Modal>
    </div>
  );
}

/** antd App 包裹由 MasterView 提供 */
export function FlavorsPanel(props: FlavorsPanelProps) {
  return <FlavorsPanelInner {...props} />;
}