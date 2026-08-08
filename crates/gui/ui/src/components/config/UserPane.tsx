// 用户与 uid 映射面板：keep-id 开关（user_home）+ uid 映射语义对照表
// + root/默认用户只读区。keep-id 语义见 docs/12 实证（字面 uid_map 不代表
// 实际属主；容器 root 宿主身份 = subuid 100000，非宿主默认用户）。

import { Alert, Space, Switch, Table, Typography } from 'antd';
import type { ContainerConfigView, HostUser } from '../../types';

interface UserPaneProps {
  userHome: boolean;
  onUserHomeChange(v: boolean): void;
  hostUser: HostUser | null;
  effective: ContainerConfigView | null;
}

interface UidRow {
  key: string;
  container: string;
  host: string;
  note: string;
}

export function UserPane({ userHome, onUserHomeChange, hostUser, effective }: UserPaneProps) {
  /** uid 映射语义对照表数据（keep-id 语义见 docs/12 实证） */
  const uidMapRows = (): UidRow[] => {
    const hu = hostUser;
    if (!hu) {
      return [
        {
          key: 'host-missing',
          container: '容器内身份',
          host: '宿主身份',
          note: '宿主用户探测失败，容器以 root 运行（用户一致性映射未生效）',
        },
      ];
    }
    if (userHome) {
      // keep-id 激活（实测文件属主，2026-08-07）：容器 uid 0（root）的宿主身份是
      // **subuid 100000**（容器文件系统属主），不是宿主默认用户；
      // 容器 uid N（node）= 宿主登录用户 N
      return [
        {
          key: 'keep-id-root',
          container: 'root（uid 0）· server/装包',
          host: '宿主 subuid 100000（容器文件系统属主）',
          note: '可写容器系统文件（apt/sudo）；写宿主 home 属主呈现 100000',
        },
        {
          key: 'keep-id-node',
          container: `node（uid ${hu.uid}）· 应用默认`,
          host: `${hu.name}（uid ${hu.uid}）`,
          note: '宿主 home 直读写（属主正确）、显示可用（GUI 应用沙盒完整）',
        },
      ];
    }
    // 默认 rootless：容器 uid 0 = 宿主用户；容器普通用户 = subuid 偏移
    return [
      {
        key: 'plain-root',
        container: 'root（uid 0）· server/装包',
        host: `${hu.name}（uid ${hu.uid}）`,
        note: '可写容器系统文件',
      },
      {
        key: 'plain-user',
        container: '容器内普通用户（uid N）',
        host: '宿主 subuid 100000+N（/etc/subuid）',
        note: '无法访问宿主 home（0700）与显示 socket',
      },
    ];
  };

  return (
    <div className="config-pane">
      <div className="config-field">
        <label>用户一致性映射（keep-id）</label>
        <Space>
          <Switch
            checked={userHome}
            onChange={onUserHomeChange}
            checkedChildren="开"
            unCheckedChildren="关"
          />
          <Typography.Text type="secondary">
            容器内用户 uid 与宿主对齐（keep-id），应用默认以 node 用户运行
          </Typography.Text>
        </Space>
      </div>
      {!userHome && (
        <Alert
          type="warning"
          showIcon
          message="关闭用户一致性映射有风险"
          description="关闭后容器进程以 root 运行，宿主 $HOME 挂载与 keep-id 一并移除：GUI 应用将无法访问宿主显示 socket（图形界面不可用）。GUI 容器请保持开启；此开关随「保存并重启」重建容器后生效。"
          className="mode-hint"
        />
      )}

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

      <div className="config-subsection">
        <Typography.Text strong>root 与容器内默认用户（只读）</Typography.Text>
        <div className="config-field">
          <label>容器进程用户</label>
          <Typography.Text code>{effective?.user ?? '0:0（创建约定）'}</Typography.Text>
          <span className="section-hint">server 以容器内 root 运行才有装包权；应用层经 su 降权到 node。</span>
        </div>
        <div className="config-field">
          <label>容器内默认用户</label>
          <Typography.Text code>node</Typography.Text>
          <span className="section-hint">
            固定名（server CONTAINER_USER），uid/gid = 宿主{' '}
            {hostUser ? `${hostUser.uid}/${hostUser.gid}` : '（探测失败）'}；免密 sudo 已配置。
          </span>
        </div>
        <div className="config-field">
          <label>宿主用户</label>
          {hostUser ? (
            <Typography.Text code>
              {hostUser.name}（uid {hostUser.uid} / gid {hostUser.gid}）home={hostUser.home}
            </Typography.Text>
          ) : (
            <Typography.Text type="warning">探测失败——容器将降级 root 运行</Typography.Text>
          )}
        </div>
      </div>
    </div>
  );
}
