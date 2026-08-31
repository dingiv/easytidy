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
  Space,
  Spin,
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
import type { ConfTemplate } from '../types';
import './FlavorsPanel.css';

/** 生成不冲突的复制名：`base-copy`，冲突则 `base-copy2`、`base-copy3`… */
function nextCopyName(base: string, existing: string[]): string {
  const candidate = `${base}-copy`;
  if (!existing.includes(candidate)) return candidate;
  let i = 2;
  while (existing.includes(`${base}-copy${i}`)) i++;
  return `${base}-copy${i}`;
}

interface FlavorsPanelProps {
  /** 模板卡「使用」：打开配置编辑器并预填该模板（创建新容器） */
  onLaunch(templateName: string): void;
  /** 模板卡「编辑」/「新建模板」：交给 MasterView 打开模板配置管理器 tab */
  onEditTemplate(template: ConfTemplate | null): void;
  /** 模板保存成功后 +1 → 本面板重新加载列表 */
  refreshTick: number;
}

function FlavorsPanelInner({ onLaunch, onEditTemplate, refreshTick }: FlavorsPanelProps) {
  const { message, modal } = AntApp.useApp();

  const [templates, setTemplates] = useState<ConfTemplate[]>([]);
  const [lineage, setLineage] = useState<Record<string, string[]>>({});
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [syncingTemplate, setSyncingTemplate] = useState<string | null>(null);

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

  // 挂载 + 模板 tab 保存成功后（refreshTick 变化）都重新加载
  useEffect(() => {
    load();
  }, [refreshTick]);

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
          <Button type="primary" icon={<PlusOutlined />} onClick={() => onEditTemplate(null)}>
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
                  {t.path && (
                    <Tooltip title={t.path}>
                      <div className="flavor-path">{t.path}</div>
                    </Tooltip>
                  )}
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
                    onClick={() => onEditTemplate(t)}
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
    </div>
  );
}

/** antd App 包裹由 MasterView 提供 */
export function FlavorsPanel(props: FlavorsPanelProps) {
  return <FlavorsPanelInner {...props} />;
}