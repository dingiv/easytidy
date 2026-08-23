// 配置编辑器 Shell —— 所有「配置管理器」入口的通用包装。
//
// 设计原则：配置编辑器是个很通用的组件；多个入口（新建容器、容器配置编辑、
// 未来可能的其他入口）共用同一个编辑器本体。每个入口提供各自的：
//   - 标题（pane 名）
//   - 右侧操作位（模板 sync / 刷新 / 保存 / 模板 select …）
//   - 警示位（错误 / dirty / drift / empty-name …）
//   - 加载态
//   - 底部操作位（创建按钮 / hint …）
// Shell 不持有提交语义——校验和持久化仍在各入口的 handler 中（entry-specific）。
//
// Tab 标签一律按入口名取，Shell 不暴露任何全局标题。

import {
  Alert,
  Skeleton,
  Space,
  Typography,
} from 'antd';
import type { ReactNode } from 'react';

import type {
  ContainerConfig,
  ContainerConfigView,
  HostUser,
} from '../../types';
import { ContainerConfigEditor } from './ContainerConfigEditor';

export interface ConfigEditorPaneProps {
  /** Pane 标题（左上大字号），由入口传入；Shell 不写死 */
  title: string;

  /** 标题右侧操作位（Space 子节点），如 sync / refresh / save / 模板 select */
  headerActions?: ReactNode;

  /** 错误条：出错时在警示区顶部显示（可关闭） */
  error?: string | null;
  onClearError?: () => void;

  /** 其它警示（dirty / drift / empty-name 等），堆叠在错误条之下 */
  notices?: ReactNode;

  /** 加载中 → 编辑器主体替换为 Skeleton；用于 load / apply 等异步态 */
  loading?: boolean;

  // 编辑器本体（透传给 ContainerConfigEditor）
  mode: 'create' | 'edit';
  value: ContainerConfig;
  onChange(next: ContainerConfig): void;
  /** podman inspect 当前生效投影（edit 模式对照；创建时 null） */
  effective?: ContainerConfigView | null;
  /** 宿主用户（uid 映射语义对照） */
  hostUser?: HostUser | null;

  /** 底部操作位（创建按钮 / hint 等）；未传则不渲染 footer */
  footer?: ReactNode;

  /** 外层 className（默认 'config-editor-pane'） */
  className?: string;
}

/**
 * 配置编辑器 Shell。所有「配置管理器」入口的公共骨架。
 */
export function ConfigEditorPane({
  title,
  headerActions,
  error,
  onClearError,
  notices,
  loading = false,
  mode,
  value,
  onChange,
  effective = null,
  hostUser = null,
  footer,
  className,
}: ConfigEditorPaneProps) {
  const hasHeader = Boolean(title) || Boolean(headerActions);
  return (
    <div className={className ?? 'config-editor-pane'}>
      {hasHeader && (
        <div className="config-editor-pane-header">
          {title && (
            <Typography.Title level={4} className="config-editor-pane-title">
              {title}
            </Typography.Title>
          )}
          {headerActions && <Space>{headerActions}</Space>}
        </div>
      )}

      {error && (
        <Alert
          type="error"
          showIcon
          message="操作失败"
          description={error}
          closable={Boolean(onClearError)}
          onClose={onClearError}
        />
      )}
      {notices}

      <div className="config-editor-pane-body">
        {loading ? (
          <div className="config-editor-pane-loading">
            <Skeleton active paragraph={{ rows: 8 }} />
          </div>
        ) : (
          <ContainerConfigEditor
            mode={mode}
            value={value}
            onChange={onChange}
            effective={effective}
            hostUser={hostUser}
          />
        )}
      </div>

      {footer && <div className="config-editor-pane-footer">{footer}</div>}
    </div>
  );
}