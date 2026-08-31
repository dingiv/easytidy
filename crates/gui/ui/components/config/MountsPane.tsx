// 挂载（路径映射）面板：表格 + 添加行（宿主路径/容器路径/只读）。

import { useEffect, useState } from 'react';
import { App as AntApp, AutoComplete, Button, Empty, Input, Space, Switch, Table, Typography } from 'antd';
import type { TableProps } from 'antd';
import { DeleteOutlined, FolderOpenOutlined, LockOutlined, PlusOutlined } from '@ant-design/icons';
import { invoke } from '@tauri-apps/api/core';
import { errMsg } from '../../lib/errors';
import type { MountConfig } from '../../types';

/** 后端返回的宿主路径条目（list_host_path_suggestions） */
interface HostEntry {
  name: string;
  full_path: string;
  is_dir: boolean;
}

interface MountsPaneProps {
  mounts: MountConfig[];
  /** GUI 透传开启时引擎将隐式注入的挂载（只读展示；模板编辑器 gui=true 时由
   *  passthrough_preview 计算） */
  readonlyMounts?: MountConfig[];
  onAdd(m: MountConfig): void;
  onRemove(idx: number): void;
}

export function MountsPane({ mounts, readonlyMounts = [], onAdd, onRemove }: MountsPaneProps) {
  const { message } = AntApp.useApp();
  const [newMount, setNewMount] = useState({
    host_path: '',
    container_path: '',
    read_only: false,
  });
  // AutoComplete 选项(每键入重新拉后端)
  const [hostOpts, setHostOpts] = useState<
    { value: string; label: React.ReactNode }[]
  >([]);
  // 控制下拉打开态——antd AutoComplete 默认在 onSelect 后关闭,选中目录后
  // 需强制 open=true 才能看到下一级子项,否则用户必须再点输入框 + 重新键入。
  const [hostOpen, setHostOpen] = useState(false);

  /** 「浏览」按钮：调用 mount_pick_host_dir 命令打开宿主目录选择对话框。
   *  返回空字符串 = 用户取消(此时不更新输入框)。 */
  const handleBrowseHost = async () => {
    try {
      const picked = await invoke<string>('mount_pick_host_dir', {
        initial: newMount.host_path || null,
      });
      if (picked) {
        setNewMount((m) => ({ ...m, host_path: picked }));
      }
    } catch (err: any) {
      message.error(errMsg(err, '打开宿主目录选择器失败'));
    }
  };

  /** AutoComplete onSearch:每键入拉一次路径建议(目录优先,字母序) */
  const handleHostSearch = async (input: string) => {
    if (!input) {
      setHostOpts([]);
      return;
    }
    try {
      const entries = await invoke<HostEntry[]>('list_host_path_suggestions', {
        prefix: input,
      });
      setHostOpts(
        entries.map((e) => ({
          value: e.full_path + (e.is_dir ? '/' : ''), // 目录补 / 视觉提示
          label: (
            <div className="host-path-option">
              <span className={e.is_dir ? 'host-path-dir' : 'host-path-file'}>
                {e.is_dir ? '📁' : '📄'}
              </span>
              <span className="host-path-name">{e.name}</span>
              {e.full_path !== e.name && (
                <span className="host-path-tail">{e.full_path.replace(/[^/]+$/, '')}</span>
              )}
            </div>
          ),
        })),
      );
    } catch (err: any) {
      // 静默失败:AutoComplete 选项清空即可,不影响用户输入
      setHostOpts([]);
    }
  };

  /** AutoComplete onSelect:用户从下拉选了一个条目 → 把路径写回 state。
   *  预测由 useEffect 监听 host_path 变化触发(覆盖键入 + 选中两条路径,
   *  antd 的 onSearch 不在程序性 value 变化时触发,单独靠 onSearch 漏选中)。
   *  选中目录则强制 open=true 保持下拉 → 续接下一级。
   */
  const handleHostSelect = (selected: string) => {
    if (selected.endsWith('/')) {
      setHostOpen(true);
    }
  };

  /** 路径预测触发器:`newMount.host_path` 任意变化 → 拉后端建议。
   *  - 键入:onChange → setNewMount → 此 effect 触发
   *  - 选中:onSelect → onChange → setNewMount → 此 effect 触发
   *  - 浏览:setNewMount → 此 effect 触发
   *  每次 host_path 变化都 fetch 一次,异步,前端 hostOpts 最终态由 fetch 决定。
   */
  useEffect(() => {
    if (!newMount.host_path) {
      setHostOpts([]);
      return;
    }
    void handleHostSearch(newMount.host_path);
    // handleHostSearch 闭包内只依赖 setHostOpts;deps 用 host_path 已足够覆盖触发面
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [newMount.host_path]);

  const addMount = () => {
    let host = newMount.host_path.trim();
    // 规范化:AutoComplete 给目录补 `/` 后缀作视觉提示(方便钻进),
    // 入库形态剥掉末尾全部 `/`——避免出现 `/home/user/` 这种半路径形式
    // 干扰后端校验与展示。
    while (host.endsWith('/')) host = host.slice(0, -1);
    const target = newMount.container_path.trim();
    if (!host) {
      message.error('宿主路径不能为空');
      return;
    }
    if (!target) {
      message.error('容器路径不能为空');
      return;
    }
    onAdd({ host_path: host, container_path: target, read_only: newMount.read_only });
    setNewMount({ host_path: '', container_path: '', read_only: false });
  };

  const mountColumns: TableProps<MountConfig>['columns'] = [
    {
      title: '宿主路径',
      dataIndex: 'host_path',
      key: 'host_path',
      ellipsis: true,
      render: (v: string) => <span className="path-cell">{v}</span>,
    },
    {
      title: '容器路径',
      dataIndex: 'container_path',
      key: 'container_path',
      ellipsis: true,
      render: (v: string) => <span className="path-cell">{v}</span>,
    },
    {
      title: '只读',
      dataIndex: 'read_only',
      key: 'read_only',
      width: 90,
      render: (ro: boolean) => <Switch size="small" checked={ro} disabled />,
    },
    {
      title: '操作',
      key: 'actions',
      width: 70,
      render: (_: unknown, _rec: MountConfig, idx: number) => (
        <Button
          type="text"
          danger
          size="small"
          icon={<DeleteOutlined />}
          onClick={() => onRemove(idx)}
        />
      ),
    },
  ];

  return (
    <div className="config-pane">
      <Table
        size="small"
        rowKey={(_rec: MountConfig, i) => `mount-${i}`}
        columns={mountColumns}
        dataSource={mounts}
        pagination={false}
        locale={{ emptyText: <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="暂无路径映射" /> }}
      />
      <div className="add-row">
        <Space.Compact style={{ flex: 1, minWidth: 240 }}>
          <AutoComplete
            value={newMount.host_path}
            onChange={(v) => setNewMount((m) => ({ ...m, host_path: String(v ?? '') }))}
            onSelect={handleHostSelect}
            open={hostOpen}
            onOpenChange={setHostOpen}
            options={hostOpts}
            style={{ flex: 1 }}
            popupMatchSelectWidth={false}
            allowClear
            placeholder="宿主路径，如 /home/user/data"
          >
            {/* host_path 故意不绑 onPressEnter→addMount:
              antd AutoComplete 键盘导航(↑↓ + Enter 选中)有时 keypress
              会穿透到 inner Input 的 onPressEnter,误触发"添加"——彼时
              container_path 还没填,弹"容器路径不能为空"toast 让用户困惑。
              提交权交由"添加"按钮 + container_path Enter 触发。 */}
            <Input style={{ flex: 1 }} />
          </AutoComplete>
          <Button
            icon={<FolderOpenOutlined />}
            onClick={handleBrowseHost}
            title="从宿主文件系统选择目录"
          >
            浏览
          </Button>
        </Space.Compact>
        <Input
          placeholder="容器路径，如 /data"
          value={newMount.container_path}
          onChange={(e) => setNewMount((m) => ({ ...m, container_path: e.target.value }))}
          onPressEnter={addMount}
        />
        <Switch
          checked={newMount.read_only}
          onChange={(v) => setNewMount((m) => ({ ...m, read_only: v }))}
          checkedChildren="只读"
          unCheckedChildren="读写"
        />
        <Button type="primary" icon={<PlusOutlined />} onClick={addMount}>
          添加
        </Button>
      </div>

      {readonlyMounts.length > 0 && (
        <div className="config-subsection">
          <Typography.Text strong>
            <LockOutlined className="readonly-badge-icon" /> GUI 透传注入（只读）
          </Typography.Text>
          <Table
            size="small"
            rowKey={(_rec: MountConfig, i) => `gui-mount-${i}`}
            columns={[
              {
                title: '宿主路径',
                dataIndex: 'host_path',
                key: 'host_path',
                ellipsis: true,
                render: (v: string) => <span className="path-cell">{v}</span>,
              },
              {
                title: '容器路径',
                dataIndex: 'container_path',
                key: 'container_path',
                ellipsis: true,
                render: (v: string) => <span className="path-cell">{v}</span>,
              },
              {
                title: '只读',
                dataIndex: 'read_only',
                key: 'read_only',
                width: 90,
                render: (ro: boolean) => <Switch size="small" checked={ro} disabled />,
              },
            ]}
            dataSource={readonlyMounts}
            pagination={false}
          />
          <span className="section-hint">
            GUI 透传开启时由引擎按宿主实时环境注入（X11/Wayland socket、$XDG_RUNTIME_DIR、字体图标），不可手动修改。
          </span>
        </div>
      )}
    </div>
  );
}
