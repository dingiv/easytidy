// 模板配置管理器 tab —— 复用 ContainerConfigEditor（配置管理器视图）封装为独立标签页。
//
// 取代 FlavorsPanel 旧 Modal：MasterView 以 tab 打开，与「新建容器」「容器配置」
// 同款浏览器式体验。外层直接用 ConfigEditorPane Shell 的布局 class
// （.config-editor-pane / -header / -body），编辑器本体即共享的
// ContainerConfigEditor（容器/挂载/网络/环境变量/用户 五 section + YAML 桥工具条）。
// 模板独有字段（GUI 透传 / setup 安装命令）在 body 末尾追加「模板选项」section。
// header 工具条承载本页动作：保存（conf_save_template）/ 取消（关 tab）。

import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { App as AntApp, Button, Input, Space, Typography } from 'antd';
import { CloseOutlined, SaveOutlined } from '@ant-design/icons';
import { errMsg } from '../lib/errors';
import type { ConfTemplate, ContainerConfig } from '../types';
import { BLANK_CONTAINER_CONFIG, ContainerConfigEditor } from './config/ContainerConfigEditor';

/** 空白 conf 模板（新建表单初始值；keep_id 默认 true 与 Rust 共享基座对齐。
 *  gui/gpu 透传意图在 BLANK_CONTAINER_CONFIG 共享基座内，随 spread 带出） */
function emptyConfTemplate(): ConfTemplate {
  return {
    ...BLANK_CONTAINER_CONFIG,
    setup: [],
  } as ConfTemplate;
}

export interface TemplateEditorPaneProps {
  /** null = 新建模板；否则为待编辑模板的初始快照 */
  initial: ConfTemplate | null;
  /** 保存成功：刷新模板列表 + 关闭本 tab */
  onSaved(): void;
  /** 取消：关闭本 tab（丢弃未保存修改） */
  onCancel(): void;
}

function TemplateEditorPaneInner({ initial, onSaved, onCancel }: TemplateEditorPaneProps) {
  const { message } = AntApp.useApp();
  const isNew = initial === null;

  const [config, setConfig] = useState<ConfTemplate>(() => initial ?? emptyConfTemplate());
  const [setupText, setSetupText] = useState(() => (initial?.setup ?? []).join('\n'));
  const [saving, setSaving] = useState(false);

  /** 编辑器 onChange：ContainerConfig 载荷 → ConfTemplate；
   *  加载 YAML/示例若缺 gui/gpu_nvidia/gpu_amd/setup（它们不在 ContainerConfig 五
   *  section 里），保留现值 */
  const handleChange = (next: ContainerConfig) => {
    const t = next as ConfTemplate;
    setConfig((prev) => ({
      ...t,
      gui: t.gui ?? prev.gui,
      gpu_nvidia: t.gpu_nvidia ?? prev.gpu_nvidia,
      gpu_amd: t.gpu_amd ?? prev.gpu_amd,
      setup: t.setup ?? prev.setup,
    }));
  };

  const handleSave = async () => {
    if (!config.name.trim() || !config.image.trim()) {
      message.warning('名称与镜像不能为空');
      return;
    }
    setSaving(true);
    try {
      const toSave: ConfTemplate = {
        ...config,
        name: config.name.trim(),
        image: config.image.trim(),
        setup: setupText.split('\n').map((s) => s.trim()).filter(Boolean),
      };
      await invoke('conf_save_template', { template: toSave });
      message.success(`模板已保存：${toSave.name}`);
      onSaved();
    } catch (err: any) {
      message.error(errMsg(err, '保存失败'));
      console.error('conf_save_template failed:', err);
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="config-editor-pane">
      <div className="config-editor-pane-header">
        <Typography.Title level={4} className="config-editor-pane-title">
          {isNew ? '新建模板' : `编辑模板：${config.name}`}
        </Typography.Title>
        <Space>
          <Button icon={<CloseOutlined />} onClick={onCancel}>
            取消
          </Button>
          <Button type="primary" icon={<SaveOutlined />} loading={saving} onClick={handleSave}>
            保存
          </Button>
        </Space>
      </div>

      <div className="config-editor-pane-body">
        <ContainerConfigEditor
          mode="create"
          value={config}
          onChange={handleChange}
          nameLocked={!isNew}
        />

        {/* 模板独有字段：setup 安装命令（GUI/GPU 透传已并入「容器」区一等项） */}
        <section className="config-section template-extra-section">
          <h3 className="config-section-title">模板选项</h3>
          <div className="config-fields">
            <div className="config-field">
              <label>安装命令（setup）</label>
              <Input.TextArea
                rows={3}
                value={setupText}
                onChange={(e) => setSetupText(e.target.value)}
                placeholder={'每行一条，按序执行\n如：apt-get update -qq && apt-get install -y -qq sudo'}
              />
              <span className="section-hint">本轮只存不执行（执行链路与 data 启动脚本一起留待下一步）。</span>
            </div>
          </div>
        </section>
      </div>
    </div>
  );
}

/** antd App 包裹由 MasterView 提供 */
export function TemplateEditorPane(props: TemplateEditorPaneProps) {
  return <TemplateEditorPaneInner {...props} />;
}
