// 用户身份面板：keep-id 开关 + uid/gid 映射。
//
// - **keep-id 开**（且无显式映射）：紧凑只读展示 keep-id 产生的 uid/gid 映射
// - **keep-id 关**：uid/gid 自定义映射，每条映射 = 一个 input
//   （语法：容器起始:宿主起始:长度，三段冒号分割）
//
// keep-id 语义：宿主登录 uid ↔ 容器同 uid 锁死 1:1；容器 root（uid 0）↔
// 宿主 subuid 100000（容器文件系统属主）。实证见 docs/12。
// 显式映射与 keep-id 互斥（podman `--uidmap` 与 `--userns=keep-id` 不能同开）：
// 映射非空时 keep-id 被忽略。

import { useEffect, useState } from 'react';
import {
  Alert,
  Button,
  Input,
  Space,
  Switch,
  Tooltip,
  Typography,
} from 'antd';
import { DeleteOutlined, PlusOutlined } from '@ant-design/icons';
import type { HostUser, IdMapping } from '../../types';

interface UserPaneProps {
  keepId: boolean;
  uidmaps: IdMapping[];
  gidmaps: IdMapping[];
  /** 容器默认用户 uid/gid（null = 宿主登录用户）——keep-id 只读展示用 */
  userUid: number | null;
  userGid: number | null;
  onKeepIdChange(v: boolean): void;
  onUidmapsChange(v: IdMapping[]): void;
  onGidmapsChange(v: IdMapping[]): void;
  hostUser: HostUser | null;
}

/** input 语法：容器起始:宿主起始:长度（三段冒号） */
const MAP_PLACEHOLDER = '容器起始:宿主起始:长度（如 0:100000:10000）';

function parseMap(s: string): IdMapping | null {
  const parts = s.trim().split(':');
  if (parts.length !== 3) return null;
  const [c, h, l] = parts;
  if (!/^\d+$/.test(c.trim()) || !/^\d+$/.test(h.trim()) || !/^\d+$/.test(l.trim())) {
    return null;
  }
  const length = Number(l.trim());
  if (length < 1) return null;
  return { container_id: Number(c.trim()), host_id: Number(h.trim()), length };
}

const formatMap = (m: IdMapping) => `${m.container_id}:${m.host_id}:${m.length}`;

function sameMaps(a: IdMapping[], b: IdMapping[]): boolean {
  return (
    a.length === b.length &&
    a.every(
      (m, i) =>
        m.container_id === b[i].container_id && m.host_id === b[i].host_id && m.length === b[i].length,
    )
  );
}

/** 单条映射 = 一个 input 的列表编辑器。
 *
 * 本地状态 rows: string[]（草稿即状态）：合法解析行实时提交 config，
 * 非法/空行不提交（显示 error 态，可继续改）。外部 maps 变化（模板预填/
 * 复制/重置）时按「可解析行 vs maps」对比，不一致才同步回来。 */
function IdMapEditor({
  title,
  maps,
  onChange,
}: {
  title: string;
  maps: IdMapping[];
  onChange(v: IdMapping[]): void;
}) {
  const [rows, setRows] = useState<string[]>(() => maps.map(formatMap));

  useEffect(() => {
    const committed = rows
      .map(parseMap)
      .filter((m): m is IdMapping => m !== null);
    if (!sameMaps(committed, maps)) {
      setRows(maps.map(formatMap));
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [maps]);

  const commit = (next: string[]) => {
    setRows(next);
    onChange(next.map(parseMap).filter((m): m is IdMapping => m !== null));
  };

  return (
    <div className="config-subsection">
      <Typography.Text strong>{title}</Typography.Text>
      {rows.length === 0 ? (
        <Typography.Text type="secondary">未配置（空 = 无显式映射）</Typography.Text>
      ) : (
        rows.map((draft, i) => {
          const invalid = draft.trim() !== '' && parseMap(draft) === null;
          return (
            <Space key={i} className="add-row" style={{ width: '100%' }}>
              <Input
                value={draft}
                status={invalid ? 'error' : undefined}
                placeholder={MAP_PLACEHOLDER}
                onChange={(e) =>
                  commit(rows.map((r, j) => (j === i ? e.target.value : r)))
                }
                style={{ width: 320 }}
              />
              <Button
                type="text"
                danger
                icon={<DeleteOutlined />}
                onClick={() => commit(rows.filter((_, j) => j !== i))}
              />
            </Space>
          );
        })
      )}
      <div className="add-row">
        <Button
          type="dashed"
          size="small"
          icon={<PlusOutlined />}
          onClick={() => commit([...rows, ''])}
        >
          添加映射
        </Button>
      </div>
      <span className="section-hint">
        每条映射一个输入框，三段冒号：容器起始:宿主起始:长度（连续区间个数）。空行/非法行不生效。
      </span>
    </div>
  );
}

/** keep-id 只读映射行（紧凑）：`N ↔ N` 默认用户 1:1 · `0 ↔ 100000` root→subuid */
function KeepIdRow({ label, n }: { label: string; n: number | null }) {
  const val = n ?? '—';
  return (
    <div className="config-field">
      <label>{label}</label>
      <Space size="middle" wrap>
        <Typography.Text>
          <Typography.Text code>{val} ↔ {val}</Typography.Text>
          <Typography.Text type="secondary"> 默认用户（与宿主登录用户 1:1）</Typography.Text>
        </Typography.Text>
        <Typography.Text>
          <Typography.Text code>0 ↔ 100000</Typography.Text>
          <Typography.Text type="secondary"> root（宿主 subuid）</Typography.Text>
        </Typography.Text>
      </Space>
    </div>
  );
}

export function UserPane({
  keepId,
  uidmaps,
  gidmaps,
  userUid,
  userGid,
  onKeepIdChange,
  onUidmapsChange,
  onGidmapsChange,
  hostUser,
}: UserPaneProps) {
  /** 显式映射非空 → 编辑器模式（keep-id 被忽略）；否则按开关分支 */
  const hasExplicit = uidmaps.length > 0 || gidmaps.length > 0;
  const showEditors = hasExplicit || !keepId;
  const uidN = userUid ?? hostUser?.uid ?? null;
  const gidN = userGid ?? hostUser?.gid ?? null;

  return (
    <div className="config-pane">
      <div className="config-field">
        <label>用户一致性映射（keep-id）</label>
        <Space>
          <Tooltip
            title={
              hasExplicit
                ? '已配置显式映射——podman `--uidmap` 与 `--userns=keep-id` 互斥，keep-id 此时被忽略'
                : '宿主 uid ↔ 容器同 uid 锁死 1:1（GUI 容器请保持开启）'
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
        </Space>
      </div>

      {hasExplicit && (
        <Alert
          type="warning"
          showIcon
          message="已配置显式映射：keep-id 被忽略（podman `--uidmap` 与 `--userns=keep-id` 互斥）。清空全部映射可恢复 keep-id。"
          className="mode-hint"
        />
      )}

      {showEditors ? (
        <>
          <IdMapEditor
            title="UID 映射"
            maps={uidmaps}
            onChange={onUidmapsChange}
          />
          <IdMapEditor
            title="GID 映射"
            maps={gidmaps}
            onChange={onGidmapsChange}
          />
        </>
      ) : (
        <div className="config-subsection">
          <Typography.Text strong>keep-id 映射（只读）</Typography.Text>
          <KeepIdRow label="uid 映射" n={uidN} />
          <KeepIdRow label="gid 映射" n={gidN} />
        </div>
      )}
    </div>
  );
}
