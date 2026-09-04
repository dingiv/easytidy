// Master GUI 总控：浏览器式多面板架构（与 WorkerView 对齐）。
//
// - 左侧 sidebar：图标入口 + 固定 56px 宽、不折叠、不拖拽（与 Worker
//   的可折叠文件浏览器侧栏区分 —— Master 侧栏是纯图标导航）
// - 右侧多 pane 多 tab：与 Worker 同款 `.pane-tabs` / `.pane-tab` /
//   `.tab-pane` / `.pane-empty` 模式。常驻渲染 + display 切换，
//   关闭面板才卸载
// - Pane kind 集合：`containers`(单实例) / `new-container`(多实例,
//   按 initialFlavor 区分) / `flavors`(单实例) / `images`(单实例)
// - 模板「启动」:FlavorsPanel 通过 onLaunch(flavor) 回调 → openPane
//   ('new-container', { initialFlavor: flavor }),激活新 pane；表单
//   提交成功后关 pane + refreshTick++ 触发 containers 列表 reload
//
// 样式复用 WorkerView 既有 class；侧栏自身新增 `.master-sidebar*` 一组。

import React, { useCallback, useRef, useState } from 'react';
import { App as AntApp, Tooltip } from 'antd';
import {
  AppstoreOutlined,
  CloseOutlined,
  DatabaseOutlined,
  PictureOutlined,
  PlusOutlined,
} from '@ant-design/icons';
import { useUiStore } from '../stores/uiStore';
import { ContainerCreateForm } from './ContainerCreateForm';
import { ContainersPanel, ContainerRef } from './ContainersPanel';
import { FlavorsPanel } from './FlavorsPanel';
import { ImagesPanel } from './ImagesPanel';
import { TemplateEditorPane } from './TemplateEditorPane';
import type { ConfTemplate } from '../types';
import logo from '../assets/logo.png';

type PaneKind = 'containers' | 'new-container' | 'flavors' | 'images' | 'template-editor';

interface Pane {
  id: string;
  kind: PaneKind;
  title: string;
  /** 仅 new-container:预选 conf 模板 */
  initialTemplate?: string;
  /** 仅 template-editor:待编辑模板快照（null = 新建） */
  templateData?: ConfTemplate | null;
}

const PANE_TITLE: Record<PaneKind, string> = {
  containers: '容器',
  'new-container': '新建容器',
  flavors: '模板',
  images: '镜像',
  'template-editor': '模板配置',
};

/** 长 flavor 名截断(避免 tab 标题撑开) */
function truncateFlavor(name: string, max = 16): string {
  return name.length > max ? `${name.slice(0, max)}…` : name;
}

function MasterViewInner() {
  // 容器列表刷新触发器：new-container 提交成功后 +1
  const [refreshTick, setRefreshTick] = useState(0);
  // 模板列表刷新触发器：模板 tab 保存成功后 +1（FlavorsPanel 重新加载）
  const [templateTick, setTemplateTick] = useState(0);

  // 容器列表 ref：直接调 reload() 拉新数据
  const containersRef = useRef<ContainerRef>(null);

  // 默认开一个 containers pane —— 避免初次进入 0ms 空态闪烁
  const initialPane: Pane = {
    id: useUiStore.getState().nextPaneId(),
    kind: 'containers',
    title: PANE_TITLE.containers,
  };
  const [panes, setPanes] = useState<Pane[]>([initialPane]);
  const [activePaneId, setActivePaneId] = useState<string | null>(initialPane.id);
  // 初始激活第一个 pane（惰性：panes 始终包含初始项,这里主要是类型兼容）
  const activeId = activePaneId ?? panes[0]?.id ?? null;

  /** 打开 pane：'new-container' / 'template-editor' 始终新建,其他 kind 同类已开则聚焦 */
  const openPane = useCallback(
    (kind: PaneKind, opts?: { initialTemplate?: string; template?: ConfTemplate | null }) => {
      setPanes((prev) => {
        if (kind !== 'new-container' && kind !== 'template-editor') {
          const existing = prev.find((p) => p.kind === kind);
          if (existing) {
            setActivePaneId(existing.id);
            return prev;
          }
        }
        const id = useUiStore.getState().nextPaneId();
        const title =
          kind === 'new-container' && opts?.initialTemplate
            ? `新建容器 · ${truncateFlavor(opts.initialTemplate)}`
            : kind === 'template-editor'
              ? opts?.template
                ? `编辑模板 · ${opts.template.name}`
                : '新建模板'
              : PANE_TITLE[kind];
        const pane: Pane = {
          id,
          kind,
          title,
          initialTemplate: opts?.initialTemplate,
          templateData: kind === 'template-editor' ? (opts?.template ?? null) : undefined,
        };
        setActivePaneId(id);
        return [...prev, pane];
      });
    },
    [],
  );

  /** 关闭 pane：激活相邻项；终端/PTY 清理钩子在 Worker,Master 用不到 */
  const closePane = useCallback((id: string) => {
    setPanes((prev) => {
      const idx = prev.findIndex((p) => p.id === id);
      if (idx === -1) return prev;
      const next = prev.filter((p) => p.id !== id);
      if (activeId === id) {
        const neighbor = next[Math.min(idx, next.length - 1)];
        setActivePaneId(neighbor ? neighbor.id : null);
      }
      return next;
    });
  }, [activeId]);

  /** 模板「使用」：直接 openPane new-container + 预填模板 */
  const handleLaunchFlavor = useCallback(
    (flavor: string) => {
      openPane('new-container', { initialTemplate: flavor });
    },
    [openPane],
  );

  /** 模板「编辑」/「新建」：打开模板配置管理器 tab（t=null → 新建） */
  const handleEditTemplate = useCallback(
    (t: ConfTemplate | null) => {
      openPane('template-editor', { template: t });
    },
    [openPane],
  );

  /** 模板保存成功：关 tab + 触发模板列表刷新 */
  const handleTemplateSaved = useCallback(
    (paneId: string) => {
      closePane(paneId);
      setTemplateTick((t) => t + 1);
    },
    [closePane],
  );

  /** 新建容器表单提交成功：关 pane + 触发刷新 + 切回列表 */
  const handleNewContainerCreated = useCallback(
    (paneId: string) => {
      closePane(paneId);
      setRefreshTick((t) => t + 1);
      containersRef.current?.reload();
    },
    [closePane],
  );

  /** 侧栏图标项(包装 tooltip + active 高亮) */
  const SidebarIcon: React.FC<{
    label: string;
    icon: React.ReactNode;
    active: boolean;
    onClick(): void;
  }> = ({ label, icon, active, onClick }) => (
    <Tooltip title={label} placement="right" mouseEnterDelay={4}>
      <button
        className={`master-sidebar-icon ${active ? 'active' : ''}`}
        onClick={onClick}
        aria-label={label}
      >
        {icon}
      </button>
    </Tooltip>
  );

  /** 是否 active:同 kind 已开 + 当前 pane 焦点 */
  const isActive = (kind: PaneKind): boolean =>
    panes.some((p) => p.kind === kind && p.id === activeId);

  return (
    <div className="master-view">
      <div className="per-layout">
        {/* 左侧：固定 56px 图标侧栏 */}
        <aside className="per-left master-sidebar">
          <div className="master-sidebar-brand">
            <img src={logo} className="app-logo" alt="easytidy" />
          </div>
          <nav className="master-sidebar-icons">
            <SidebarIcon
              label="容器管理"
              icon={<DatabaseOutlined />}
              active={isActive('containers')}
              onClick={() => openPane('containers')}
            />
            <SidebarIcon
              label="镜像管理"
              icon={<PictureOutlined />}
              active={isActive('images')}
              onClick={() => openPane('images')}
            />
            <hr className="master-sidebar-divider" />
            <SidebarIcon
              label="模板管理"
              icon={<AppstoreOutlined />}
              active={isActive('flavors')}
              onClick={() => openPane('flavors')}
            />
            <hr className="master-sidebar-divider" />
            <SidebarIcon
              label="新增容器"
              icon={<PlusOutlined />}
              active={isActive('new-container')}
              onClick={() => openPane('new-container')}
            />
          </nav>
        </aside>

        {/* 右侧：标签栏 + 内容区 */}
        <div className="per-right">
          {panes.length > 0 && (
            <div className="pane-tabs">
              {panes.map((p) => (
                <div
                  key={p.id}
                  className={`pane-tab ${activeId === p.id ? 'active' : ''}`}
                  onClick={() => setActivePaneId(p.id)}
                  title={p.title}
                >
                  <span className="pane-tab-title">{p.title}</span>
                  <CloseOutlined
                    className="pane-tab-close"
                    onClick={(e) => {
                      e.stopPropagation();
                      closePane(p.id);
                    }}
                  />
                </div>
              ))}
            </div>
          )}

          <section className="main-panel">
            {panes.map((p) => (
              <div
                key={p.id}
                className="tab-pane"
                style={{ display: activeId === p.id ? undefined : 'none' }}
              >
                {p.kind === 'containers' && (
                  <ContainersPanel ref={containersRef} refreshTick={refreshTick} />
                )}
                {p.kind === 'images' && <ImagesPanel />}
                {p.kind === 'flavors' && (
                  <FlavorsPanel
                    onLaunch={handleLaunchFlavor}
                    onEditTemplate={handleEditTemplate}
                    refreshTick={templateTick}
                  />
                )}
                {p.kind === 'new-container' && (
                  <ContainerCreateForm
                    initialTemplate={p.initialTemplate}
                    onCreated={() => handleNewContainerCreated(p.id)}
                  />
                )}
                {p.kind === 'template-editor' && (
                  <TemplateEditorPane
                    initial={p.templateData ?? null}
                    onSaved={() => handleTemplateSaved(p.id)}
                    onCancel={() => closePane(p.id)}
                  />
                )}
              </div>
            ))}
            {panes.length === 0 && (
              <div className="pane-empty">
                点击左侧图标打开面板(容器 / 镜像 / 配置编辑器 / 模板)
              </div>
            )}
          </section>
        </div>
      </div>
    </div>
  );
}

/** antd App 包裹：让子组件 message/modal/notification 可用 */
export function MasterView() {
  return (
    <AntApp className='app'>
      <MasterViewInner />
    </AntApp>
  );
}