// 用户身份面板：keep-id 开关 + 显式 UID/GID 重映射 + uid/gid + 容器内用户名。
//
// 新身份模型（协议 v2，容器以配置 uid/gid 直接运行，无 root 启动）：
// - **keep-id 开**：宿主登录 uid ↔ 容器同 uid 锁死 1:1（podman userns keep-id）；
//   此时 user_uid 应等于宿主 uid（不等 → warning，不阻断）
// - **显式 uid/gid 映射**（uidmaps/gidmaps 非空）：podman `--uidmap`/`--gidmap`，
//   精确决定容器 uid/gid 落位。与 keep-id **互斥**（podman 实测 `--uidmap` 与
//   `--userns=keep-id` 不能同开）——映射非空时 keep-id 开关被忽略。
// - **user_name 有值**：首次创建经宿主 root exec useradd 建号（/home/<name>）；
//   无值 = 跟随容器默认用户（镜像同 uid 的 passwd 条目，whoami 显示其名字；
//   无条目则显示 uid 数字），家目录 = 该用户 passwd home（容器层持久）
// uid/gid 留空 = 宿主登录用户值。keep-id 语义实证见 docs/12。

import { useState } from 'react';
import {
  App as AntApp,
  Alert,
  Button,
  Empty,
  Input,
  InputNumber,
  Space,
  Switch,
  Table,
  Tooltip,
  Typography,
} from 'antd';
import { DeleteOutlined, PlusOutlined } from '@ant-design/icons';
import type { ContainerConfigView, HostUser, IdMapping } from '../../types';

interface UserPaneProps {
  keepId: boolean;
  uidmaps: IdMapping[];
  gidmaps: IdMapping[];
  userUid: number | null;
  userGid: number | null;
  userName: string | null;
  onKeepIdChange(v: boolean): void;
  onUidmapsChange(v: IdMapping[]): void;
  onGidmapsChange(v: IdMapping[]): void;
  onUserUidChange(v: number | null): void;
  onUserGidChange(v: number | null): void;
  onUserNameChange(v: string | null): void;
  hostUser: HostUser | null;
  effective: ContainerConfigView | null;
}

interface UidRow {
  key: string;
  container: string;
  host: string;
  note: string;
}

/** 单条 IdMapping 列表编辑器（表格 + 添加行）。UID / GID 复用同一组件。 */
function IdMapEditor({
  title,
  maps,
  onAdd,
  onRemove,
}: {
  title: string;
  maps: IdMapping[];
  onAdd(m: IdMapping): void;
  onRemove(idx: number): void;
}) {
  const { message } = AntApp.useApp();
  const [draft, setDraft] = useState<IdMapping>({ container_id: 0, host_id: 0, length: 1 });

  const add = () => {
    if (draft.length <= 0) {
      message.error('长度需 ≥ 1');
      return;
    }
    onAdd(draft);
    setDraft({ container_id: 0, host_id: 0, length: 1 });
  };

  return (
    <div className="config-subsection">
      <Typography.Text strong>{title}</Typography.Text>
      <Table
        size="small"
        rowKey={(_rec, i) => `${title}-${i}`}
        columns={[
          { title: '容器起始', dataIndex: 'container_id', key: 'container_id', width: 110 },
          { title: '宿主起始', dataIndex: 'host_id', key: 'host_id', width: 110 },
          { title: '长度', dataIndex: 'length', key: 'length', width: 90 },
          {
            title: '操作',
            key: 'actions',
            width: 60,
            render: (_: unknown, _rec: IdMapping, i: number) => (
              <Button
                type="text"
                danger
                size="small"
                icon={<DeleteOutlined />}
                onClick={() => onRemove(i)}
              />
            ),
          },
        ]}
        dataSource={maps}
        pagination={false}
        locale={{ emptyText: <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="未配置" /> }}
      />
      <div className="add-row">
        <Space size="small">
          <InputNumber
            min={0}
            addonBefore="容器"
            value={draft.container_id}
            onChange={(v) => setDraft((d) => ({ ...d, container_id: v ?? 0 }))}
            style={{ width: 130 }}
          />
          <InputNumber
            min={0}
            addonBefore="宿主"
            value={draft.host_id}
            onChange={(v) => setDraft((d) => ({ ...d, host_id: v ?? 0 }))}
            style={{ width: 130 }}
          />
          <InputNumber
            min={1}
            addonBefore="长度"
            value={draft.length}
            onChange={(v) => setDraft((d) => ({ ...d, length: v ?? 1 }))}
            style={{ width: 120 }}
          />
          <Button type="primary" size="small" icon={<PlusOutlined />} onClick={add}>
            添加
          </Button>
        </Space>
      </div>
      <span className="section-hint">
        每行 = 一个连续区间：容器 [容器起始, +长度) ↔ 宿主 [宿主起始, +长度)。多行拼成完整映射。
      </span>
    </div>
  );
}

export function UserPane({
  keepId, uidmaps, gidmaps, userUid, userGid, userName,
  onKeepIdChange, onUidmapsChange, onGidmapsChange,
  onUserUidChange, onUserGidChange, onUserNameChange,
  hostUser, effective,
}: UserPaneProps) {
  /** 显式映射是否启用（任一非空）——与 keep-id 互斥 */
  const hasExplicit = uidmaps.length > 0 || gidmaps.length > 0;

  /** uid 映射语义对照表数据（keep-id 语义见 docs/12 实证） */
  const uidMapRows = (): UidRow[] => {
    const hu = hostUser;
    const cfgUid = userUid ?? hu?.uid ?? 0;
    const cfgGid = userGid ?? hu?.gid ?? 0;
    if (!hu) {
      return [
        {
          key: 'host-missing',
          container: `默认用户（uid ${cfgUid}）`,
          host: '宿主身份',
          note: '宿主用户探测失败，创建时将报错（不再静默降级 root）',
        },
      ];
    }
    if (keepId) {
      // keep-id 激活（实测文件属主，2026-08-07）：容器 uid N（默认用户）=
      // 宿主登录用户 N 锁死 1:1；容器 root（uid 0）的宿主身份是 subuid 100000
      return [
        {
          key: 'keep-id-user',
          container: `默认用户（uid ${cfgUid}）· server/应用`,
          host: keepId && cfgUid === hu.uid
            ? `${hu.name}（uid ${hu.uid}）· 与宿主锁死 1:1`
            : `${hu.name}（uid ${hu.uid}）· 配置值 ${cfgUid} 不一致`,
          note: '宿主 home 直读写（属主正确）、显示可用（GUI 应用沙盒完整）',
        },
        {
          key: 'keep-id-root',
          container: 'root（uid 0）· root 终端/装包',
          host: '宿主 subuid 100000（容器文件系统属主）',
          note: '可写容器系统文件（apt）；写宿主 home 属主呈现 100000',
        },
      ];
    }
    // keep-id 关：显式 uid 映射（rootless /etc/subuid 偏移）
    return [
      {
        key: 'plain-user',
        container: `默认用户（uid ${cfgUid}:gid ${cfgGid}）· server/应用`,
        host: '宿主 subuid 100000+偏移（/etc/subuid）',
        note: '无法访问宿主 home（0700）与显示 socket——GUI 应用不可用',
      },
      {
        key: 'plain-root',
        container: 'root（uid 0）· root 终端/装包',
        host: '宿主 subuid 100000（容器文件系统属主）',
        note: '可写容器系统文件',
      },
    ];
  };

  return (
    <div className="config-pane">
      <div className="config-field">
        <label>用户一致性映射（keep-id）</label>
        <Space>
          <Tooltip
            title={
              hasExplicit
                ? '已配置显式 UID/GID 映射——podman 实测 `--uidmap` 与 `--userns=keep-id` 互斥，keep-id 此时被忽略（由显式映射决定 uid/gid 落位）'
                : undefined
            }
          >
            <Switch
              checked={keepId}
              disabled={hasExplicit}
              onChange={onKeepIdChange}
              checkedChildren="开"
              unCheckedChildren="关"
            />
          </Tooltip>
          <Typography.Text type="secondary">
            宿主 uid ↔ 容器同 uid 锁死 1:1（GUI 容器请保持开启）
          </Typography.Text>
        </Space>
      </div>

      {hasExplicit && (
        <Alert
          type="warning"
          showIcon
          message="已配置显式 UID/GID 映射"
          description="podman 实测 `--uidmap`/`--gidmap` 与 `--userns=keep-id` 互斥：映射非空时 keep-id 开关被忽略，容器 uid/gid 落位完全由下方显式映射决定。清空全部映射行可恢复 keep-id。"
          className="mode-hint"
        />
      )}

      <IdMapEditor
        title="显式 UID 重映射"
        maps={uidmaps}
        onAdd={(m) => onUidmapsChange([...uidmaps, m])}
        onRemove={(idx) => onUidmapsChange(uidmaps.filter((_, i) => i !== idx))}
      />
      <IdMapEditor
        title="显式 GID 重映射"
        maps={gidmaps}
        onAdd={(m) => onGidmapsChange([...gidmaps, m])}
        onRemove={(idx) => onGidmapsChange(gidmaps.filter((_, i) => i !== idx))}
      />

      <div className="config-field">
        <label>容器默认用户 uid</label>
        <Space>
          <InputNumber
            min={0}
            style={{ width: 140 }}
            value={userUid}
            onChange={onUserUidChange}
            placeholder={hostUser ? String(hostUser.uid) : '宿主 uid'}
          />
          <Typography.Text type="secondary">
            留空 = 宿主登录用户（{hostUser ? `uid ${hostUser.uid}` : '探测失败'}）
          </Typography.Text>
        </Space>
      </div>

      <div className="config-field">
        <label>容器默认用户 gid</label>
        <Space>
          <InputNumber
            min={0}
            style={{ width: 140 }}
            value={userGid}
            onChange={onUserGidChange}
            placeholder={hostUser ? String(hostUser.gid) : '宿主 gid'}
          />
          <Typography.Text type="secondary">
            留空 = 宿主登录用户（{hostUser ? `gid ${hostUser.gid}` : '探测失败'}）
          </Typography.Text>
        </Space>
      </div>

      <div className="config-field">
        <label>容器内用户名（可选）</label>
        <Space>
          <Input
            style={{ width: 200 }}
            value={userName ?? ''}
            onChange={(e) => onUserNameChange(e.target.value.trim() || null)}
            placeholder="留空 = 按 uid 运行（whoami 显示 uid 数字）"
          />
          <Typography.Text type="secondary">
            有值 = 创建时经 root exec useradd 建号（/home/&lt;name&gt;）
          </Typography.Text>
        </Space>
      </div>

      {keepId && hostUser && userUid !== null && userUid !== hostUser.uid && (
        <Alert
          type="warning"
          showIcon
          message="keep-id 开启但 uid 与宿主不一致"
          description={`keep-id 下宿主登录 uid（${hostUser.uid}）↔ 容器同 uid 锁死 1:1。配置 uid ${userUid} 与宿主不一致，keep-id 映射将按容器 uid ${userUid} 生效（宿主侧需存在该 uid 的 subuid 映射），容器内文件宿主侧属主呈现将异常。请确认有意为之。`}
          className="mode-hint"
        />
      )}
      {!keepId && !hasExplicit && (
        <Alert
          type="warning"
          showIcon
          message="keep-id 已关闭"
          description="容器默认用户 uid 与宿主不再对齐：显示环境可能不可用（GUI 应用无法访问宿主显示 socket）。仅适用于无头/非 GUI 容器；此配置随「保存并重启」重建容器后生效。"
          className="mode-hint"
        />
      )}

      {!hasExplicit && (
        <div className="config-subsection">
          <Typography.Text strong>uid 映射关系</Typography.Text>
          <Table
            size="small"
            rowKey={(r) => r.key}
            dataSource={uidMapRows()}
            pagination={false}
            columns={[
              { title: '容器内身份', dataIndex: 'container', key: 'container' },
              { title: '宿主身份', dataIndex: 'host', key: 'host' },
              { title: '能力', dataIndex: 'note', key: 'note' },
            ]}
          />
          <span className="section-hint">
            {effective?.userns_mode
              ? `podman 实际 userns 模式：${effective.userns_mode}（keep-id 语义见 docs/12；字面 uid_map 不代表实际属主）。`
              : '容器未创建，以上为按当前配置的预期映射（keep-id 语义见 docs/12）。'}
          </span>
        </div>
      )}

      <div className="config-subsection">
        <Typography.Text strong>当前生效（只读）</Typography.Text>
        <div className="config-field">
          <label>容器进程用户</label>
          <Typography.Text code>{effective?.user ?? '（未创建）'}</Typography.Text>
          <span className="section-hint">容器默认用户（"uid:gid"）；server 与该用户同身份运行（无 root）。</span>
        </div>
        <div className="config-field">
          <label>宿主用户</label>
          {hostUser ? (
            <Typography.Text code>
              {hostUser.name}（uid {hostUser.uid} / gid {hostUser.gid}）home={hostUser.home}
            </Typography.Text>
          ) : (
            <Typography.Text type="warning">探测失败——创建容器将报错（不再静默降级 root）</Typography.Text>
          )}
        </div>
      </div>
    </div>
  );
}
