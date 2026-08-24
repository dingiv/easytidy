// 容器面板：基本信息（名称/镜像，按模式可编辑）+ entry/参数 + 静默启动 +
// 持久化 + 血缘。统一编辑器（ContainerConfigEditor）的「容器」页。

import { useEffect, useMemo, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Input, Select, Space, Switch, Tag, Typography } from 'antd';
import type { ContainerConfig, ImageSummary } from '../../types';

interface ContainerPaneProps {
  edit: ContainerConfig;
  /** create = 名称/镜像可编辑；edit = 身份只读（改名/换镜像 = 另一个容器） */
  mode: 'create' | 'edit';
  onNameChange(v: string): void;
  onImageChange(v: string): void;
  onEntryChange(v: string): void;
  onEntryArgsChange(v: string[]): void;
  onSilentBootChange(v: boolean): void;
  onPersistentChange(v: boolean): void;
}

export function ContainerPane({
  edit, mode, onNameChange, onImageChange, onEntryChange, onEntryArgsChange,
  onSilentBootChange, onPersistentChange,
}: ContainerPaneProps) {
  // 镜像下拉数据：仅 create 模式需要拉（edit 模式镜像只读，渲染 Typography.Text）。
  const [images, setImages] = useState<ImageSummary[]>([]);
  useEffect(() => {
    if (mode !== 'create') return;
    invoke<ImageSummary[]>('images_list')
      .then((list) => setImages(list ?? []))
      .catch((err) => console.error('images_list failed:', err));
  }, [mode]);

  // 把 ImageSummary[] 摊平为 tag 字符串数组（去重 + 排序），同时保留 image 引用以便渲染次要信息
  const tagOptions = useMemo(() => {
    const seen = new Set<string>();
    const opts: { value: string; label: string; meta: string }[] = [];
    for (const img of images) {
      for (const tag of img.repo_tags) {
        if (tag === '' || seen.has(tag)) continue;
        seen.add(tag);
        opts.push({
          value: tag,
          label: tag,
          meta: `${formatSize(img.size)} · ${formatAge(img.created)}`,
        });
      }
    }
    opts.sort((a, b) => a.label.localeCompare(b.label));
    return opts;
  }, [images]);

  return (
    <div className="config-fields">
      {mode === 'create' && (
        <div className="config-field">
          <label>名称</label>
          <Input
            placeholder="如：my-env（模板按名称展开，请先输入）"
            value={edit.name}
            onChange={(e) => onNameChange(e.target.value)}
          />
        </div>
      )}
      <div className="config-field">
        <label>镜像{mode === 'create' ? '（需已拉取，可搜索本地已有）' : ''}</label>
        {mode === 'create' ? (
          // antd Select 内部用 rc-virtual-list,`virtual` prop + `listHeight` 把下拉
          // 限制为定高虚拟滚动面板(20-30 个可见)。showSearch 启用过滤,
          // filterOption 收敛到前缀匹配(不区分大小写)。空查询返回 true,
          // 配合 virtual 让面板仍可见 ~IMAGE_PANEL_DEFAULT_VISIBLE 个。
          <Select
            showSearch
            virtual
            listHeight={256}
            placeholder="如：docker.io/library/ubuntu:24.04（输入关键字筛选本地镜像）"
            value={edit.image || undefined}
            onChange={(v) => onImageChange(String(v ?? ''))}
            options={tagOptions.map((o) => ({
              value: o.value,
              label: (
                <div className="image-option">
                  <div className="image-option-tag">{o.label}</div>
                  <div className="image-option-meta">{o.meta}</div>
                </div>
              ),
            }))}
            defaultActiveFirstOption
            allowClear
            notFoundContent="无匹配镜像（先到「镜像」面板拉取）"
            popupMatchSelectWidth={false}
            optionFilterProp="value"
            listItemHeight={36}
          />
        ) : (
          <Typography.Text code>{edit.image}</Typography.Text>
        )}
      </div>
      <div className="config-field">
        <label>入口应用（entry）</label>
        <Input
          placeholder="容器启动时运行的命令，留空则不启动应用"
          value={edit.entry ?? ''}
          onChange={(e) => onEntryChange(e.target.value)}
        />
        <span className="section-hint">随「保存并重启」一起生效：容器重建后由 server 链式拉起。</span>
      </div>
      <div className="config-field">
        <label>入口应用参数</label>
        <Input
          placeholder="空格分隔，拼接在 entry 后执行（含空格参数需引号）"
          value={edit.entry_args.join(' ')}
          onChange={(e) => onEntryArgsChange(e.target.value.split(/\s+/).filter(Boolean))}
        />
      </div>
      <div className="config-field">
        <label>静默启动</label>
        <Space>
          <Switch
            checked={edit.silent_boot}
            onChange={onSilentBootChange}
            checkedChildren="开"
            unCheckedChildren="关"
          />
          <Typography.Text type="secondary">
            宿主开机时无头启动容器并拉起 entry 应用（不弹 GUI）
          </Typography.Text>
        </Space>
      </div>
      <div className="config-field">
        <label>持久化</label>
        <Space>
          <Switch
            checked={edit.persistent}
            onChange={onPersistentChange}
            checkedChildren="常驻"
            unCheckedChildren="非常驻"
          />
          <Typography.Text type="secondary">catatonit + server 生命周期托管</Typography.Text>
        </Space>
      </div>
      {edit.flavor && (
        <div className="config-field">
          <label>来源模板</label>
          <Space>
            <Tag color="blue">{edit.flavor}</Tag>
            <Typography.Text type="secondary">
              配置由此模板展开；模板修改后可在顶部「从模板同步」重新对齐
            </Typography.Text>
          </Space>
        </div>
      )}
    </div>
  );
}

/** 字节数 → 人读 ("1.4 GB") */
function formatSize(bytes: number): string {
  if (bytes <= 0) return '-';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let i = 0;
  let n = bytes;
  while (n >= 1024 && i < units.length - 1) {
    n /= 1024;
    i++;
  }
  return `${n.toFixed(n < 10 && i > 0 ? 1 : 0)} ${units[i]}`;
}

/** Unix epoch 秒 → "3d ago" / "2h ago" */
function formatAge(created: number): string {
  if (!created) return '-';
  const now = Math.floor(Date.now() / 1000);
  const diff = Math.max(0, now - created);
  if (diff < 60) return `${diff}s ago`;
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`;
  return `${Math.floor(diff / 86400)}d ago`;
}