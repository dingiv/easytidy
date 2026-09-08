// 新建容器页 —— 配置编辑器的「创建容器」入口。
//
// 入口 specific 逻辑：本地 useState + conf 模板预填 + 提交校验 + 创建按钮。
// 布局/标题/Spin/Alert 由 `ConfigEditorPane` Shell 统一。

import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { errMsg } from '../lib/errors';
import { App as AntApp, Alert, Button, Select, Typography } from 'antd';
import { PlayCircleOutlined } from '@ant-design/icons';
import type { ConfTemplate, ContainerConfig } from '../types';
import { BLANK_CONTAINER_CONFIG } from './config/ContainerConfigEditor';
import { ConfigEditorPane } from './config/ConfigEditorPane';
import './ContainerCreateForm.css';

interface ContainerCreateFormProps {
  /** 创建成功后回调（刷新列表并退出表单） */
  onCreated(): void;
  /** 预选模板（conf 模板卡片「使用」进入）：名称就绪后自动展开预填 */
  initialTemplate?: string;
  /** 由容器卡片「复制」进入：预填完整配置（名字已被表单改为 <原名>_copy） */
  initialConfig?: ContainerConfig;
  /** 复制来源容器名（提示文案用） */
  sourceName?: string;
}

function ContainerCreateFormInner({
  onCreated,
  initialTemplate,
  initialConfig,
  sourceName,
}: ContainerCreateFormProps) {
  const { message, modal } = AntApp.useApp();

  const [templates, setTemplates] = useState<ConfTemplate[]>([]);
  const [selectedTemplate, setSelectedTemplate] = useState<string | undefined>(initialTemplate);
  // 复制入口：初始配置预填（名字 <原名>_copy，可改）；否则空白表单
  const [config, setConfig] = useState<ContainerConfig>(() =>
    initialConfig
      ? { ...initialConfig, name: `${initialConfig.name}_copy` }
      : BLANK_CONTAINER_CONFIG,
  );
  const [creating, setCreating] = useState(false);
  // 预选模板的自动展开只做一次（名称就绪后）；此后切换/手选均为手动。
  // 复制入口（initialConfig）不走模板自动展开，避免覆盖预填。
  const [autoExpanded, setAutoExpanded] = useState(Boolean(initialConfig));

  // 可用 conf 模板（挂载即取；空数组表示"无模板,直接走镜像方式"）
  useEffect(() => {
    invoke<ConfTemplate[]>('conf_templates')
      .then(setTemplates)
      .catch((err) => {
        console.error('conf_templates failed:', err);
        setTemplates([]);
      });
  }, []);

  /** 选定 conf 模板 → 预填表单。`name` 显式传入；缺省用表单当前名（切换模板场景）。 */
  const handleTemplateSelect = async (name: string, containerName?: string) => {
    const resolvedName = (containerName ?? config.name).trim();
    if (!resolvedName) return; // 展开结果含容器名，无名称时无意义
    try {
      // conf_template_expand 直接返回已注入宿主 env 的 ContainerConfig：
      // gui=true 时按宿主实时 DISPLAY/WAYLAND/XAUTHORITY/XDG_RUNTIME_DIR 注入。
      // 模板仅作创建期预填，容器创建后与模板解耦。省去前端再展开 / 拼装。
      const expanded = await invoke<ContainerConfig>('conf_template_expand', {
        name,
        containerName: resolvedName,
      });
      // 保留用户手动添加的挂载（2026-09-02 修复）：模板（重新）展开若整体覆盖
      // config，会把用户在表单里手动加的 mount 静默抹掉 → 创建后挂载「没生效」。
      // 按 container_path 合并：模板已声明的以模板为准，其余保留用户的手动项。
      const expandedTargets = new Set((expanded.mounts ?? []).map((m) => m.container_path));
      const manual = (config.mounts ?? []).filter(
        (m) => !expandedTargets.has(m.container_path),
      );
      setConfig({ ...expanded, mounts: [...(expanded.mounts ?? []), ...manual] });
      setSelectedTemplate(name);
    } catch (err: any) {
      message.error(errMsg(err, `展开模板 ${name} 失败`));
    }
  };

  /** 切换模板选中 → 清掉模板预填,保留已输入名称 + 镜像 */
  const handleTemplateClear = () => {
    setSelectedTemplate(undefined);
    setConfig((prev) => ({
      ...BLANK_CONTAINER_CONFIG,
      name: prev.name,
      image: prev.image,
    }));
  };

  const handleCreate = async () => {
    const name = config.name.trim();
    if (!name) {
      message.error('请输入容器名称');
      return;
    }
    if (!config.image.trim()) {
      message.error('请输入镜像（需已拉取，未拉取请到「镜像」面板）');
      return;
    }
    setCreating(true);
    try {
      await invoke('env_new', { config: { ...config, name } });
      message.success(`「${name}」已创建并运行`);
      onCreated();
    } catch (err: any) {
      // 完整报错展示：创建/启动链路多步易错，toast 会消失且截断，
      // 用 Modal 展示全文（可选中复制）
      const msg = errMsg(err);
      console.error('env_new failed:', err);
      modal.error({
        title: `创建容器「${name}」失败`,
        width: 620,
        content: <pre className="error-detail">{msg}</pre>,
        okText: '知道了',
      });
    } finally {
      setCreating(false);
    }
  };

  const nameReady = config.name.trim().length > 0;
  const hasTemplates = templates.length > 0;

  // 模板「使用」直达：挂载即有 initialTemplate 时立即展开预填（容器名预填
  // 模板名，可改），不再等用户先输名称。复制入口（initialConfig）已预填，跳过。
  useEffect(() => {
    if (initialConfig || !initialTemplate || autoExpanded) return;
    setAutoExpanded(true);
    // 先乐观填入容器名（展开返回前避免空名闪现，也让模板 Select 立即可用）
    setConfig((prev) => (prev.name.trim() ? prev : { ...prev, name: initialTemplate }));
    void handleTemplateSelect(initialTemplate, initialTemplate);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [initialTemplate, autoExpanded, initialConfig]);

  return (
    <div className="container-create-page">
      <ConfigEditorPane
        title="新建容器"
        mode="create"
        value={config}
        onChange={setConfig}
        headerActions={
          hasTemplates && (
            <Select
              style={{ minWidth: 260 }}
              placeholder={
                nameReady
                  ? '选择预设模板预填表单（可继续调整任何字段）'
                  : '请先在下方「容器」section 输入名称'
              }
              value={selectedTemplate}
              onChange={(value) => handleTemplateSelect(value)}
              onClear={handleTemplateClear}
              allowClear
              disabled={!nameReady}
              options={templates.map((t) => ({
                  value: t.id ?? t.name,
                  label: t.id ?? t.name,
                }))}
              notFoundContent="暂无可用模板"
            />
          )
        }
        notices={
          sourceName ? (
            <Alert
              type="info"
              showIcon
              message={`由容器「${sourceName}」的配置预填，名字已改为「${sourceName}_copy」（可改）；创建后与源容器完全独立。`}
            />
          ) : hasTemplates && !nameReady ? (
            <Alert
              type="info"
              showIcon
              message="模板按容器名称展开（挂载/环境变量与其绑定），请先在下方「容器」section 输入名称"
            />
          ) : undefined
        }
      />

      {/* 提交操作栏：放在 ConfigEditorPane 外，作为页面级动作。
        概念上「创建并启动」是表单提交按钮，不是编辑器的 footer——后者只承载
        编辑器自身的状态（如容器配置的 saved/apply），不混入跨生命周期的页面动作。 */}
      <div className="container-create-foot">
        <Typography.Text type="secondary" className="container-create-hint">
          {sourceName
            ? `由「${sourceName}」复制预填，镜像需已拉取；创建后新容器与源容器互不影响。`
            : selectedTemplate
              ? `由模板「${selectedTemplate}」预填，镜像需已拉取。修改字段后再次保存将以当前表单内容为准。`
              : '镜像需已在「镜像」面板拉取；创建后自动注册配置并生成桌面图标。'}
        </Typography.Text>
        <Button
          type="primary"
          icon={<PlayCircleOutlined />}
          loading={creating}
          onClick={handleCreate}
        >
          创建并启动
        </Button>
      </div>
    </div>
  );
}

/** antd App 包裹由 MasterView 提供 */
export function ContainerCreateForm(props: ContainerCreateFormProps) {
  return <ContainerCreateFormInner {...props} />;
}