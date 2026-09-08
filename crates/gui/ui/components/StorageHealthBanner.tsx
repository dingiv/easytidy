// 存储健康横幅（docs/18 道路二）：MasterView 顶部提示 rootless 原生 overlay
// 建容器慢，可一键切 fuse-overlayfs。
//
// 行为（docs/18 §4）：
// - 启动时调一次 diagnose（只读）
// - Recommended → 琥珀色横幅 + [一键修复][安装说明][忽略]
// - NeedsInstall → 同位置横幅，主按钮 [查看安装命令]
// - Ok / NotApplicable → 不显示
// - 忽略 → 用户级 dismissed 标记，横幅不再出现；保留「重新检查」入口
// - 一键修复 → Modal 确认（影响面）→ apply_fix → 成功/失败（CLI 可回滚）
// - 横幅不阻塞任何操作，纯提示

import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { errMsg } from '../lib/errors';
import { App as AntApp, Alert, Button, Space, Typography } from 'antd';
import { CopyOutlined } from '@ant-design/icons';
import './StorageHealthBanner.css';

/** 后端 storage_health_diagnose（core::storage_health::DiagnoseReport） */
interface DiagnoseReport {
  verdict: 'ok' | 'recommended' | 'needs_install' | 'not_applicable';
  rootless: boolean;
  storage_driver: string | null;
  mount_program: string | null;
  fuse_overlayfs: string | null;
  dev_fuse: boolean;
  summary: string;
}

/** 后端 storage_health_fix（core::storage_health::ApplyReport） */
interface ApplyReport {
  backup_path: string | null;
  config_path: string;
  verified: boolean;
  note_daemon: boolean;
  fuse_overlayfs_path: string;
}

const INSTALL_CMD = 'sudo apt install fuse-overlayfs';

export function StorageHealthBanner() {
  const { message, modal } = AntApp.useApp();
  const [dismissed, setDismissed] = useState(false);
  const [report, setReport] = useState<DiagnoseReport | null>(null);
  const [checking, setChecking] = useState(true);
  const [fixing, setFixing] = useState(false);

  const load = useCallback(async () => {
    setChecking(true);
    try {
      const d = await invoke<boolean>('storage_health_dismissed');
      if (d) {
        setDismissed(true);
        return; // 已忽略：不拉诊断（省一次 /info）
      }
      setReport(await invoke<DiagnoseReport>('storage_health_diagnose'));
    } catch (err: any) {
      // 诊断失败静默（横幅是辅助提示，不阻塞、不打扰）
      console.error('storage_health_diagnose failed:', err);
      setReport(null);
    } finally {
      setChecking(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const doFix = () => {
    modal.confirm({
      title: '切换到 fuse-overlayfs？',
      width: 520,
      content: (
        <div style={{ fontSize: 13, lineHeight: 1.8 }}>
          <p style={{ margin: '0 0 8px' }}>将写入宿主配置 <Typography.Text code>~/.config/containers/storage.conf</Typography.Text>：</p>
          <ul style={{ margin: 0, paddingLeft: 18 }}>
            <li>现有容器/镜像<b>不受影响</b>；运行中容器下次重启后自动切到 fuse-overlayfs</li>
            <li>原配置自动备份（可回滚）</li>
            <li>若你手动跑着常驻 <Typography.Text code>podman system service</Typography.Text>，需重启它</li>
          </ul>
        </div>
      ),
      okText: '确认修复',
      cancelText: '取消',
      onOk: async () => {
        setFixing(true);
        try {
          const r = await invoke<ApplyReport>('storage_health_fix');
          if (r.verified) {
            message.success('已修复并验证生效（建容器恢复快速路径）');
            if (r.backup_path) {
              message.info(`原配置已备份：${r.backup_path}`, 8);
            }
            await load(); // 结论变 ok → 横幅消失
          } else {
            message.warning('配置已写入，但尚未验证生效（可能常驻 podman daemon 未重启）');
            await load();
          }
          if (r.note_daemon) {
            message.info('若你在跑常驻 podman daemon（podman system service），需重启它才生效', 8);
          }
        } catch (err: any) {
          modal.error({
            title: '修复失败',
            width: 480,
            content: (
              <div style={{ fontSize: 13, lineHeight: 1.8 }}>
                <p style={{ margin: 0 }}>{errMsg(err, '修复失败')}</p>
                <p style={{ margin: '8px 0 0', color: 'rgba(0,0,0,0.45)' }}>
                  如已写入部分配置，可用 CLI 回滚：<Typography.Text code>easytidy doctor --rollback</Typography.Text>
                </p>
              </div>
            ),
          });
        } finally {
          setFixing(false);
        }
      },
    });
  };

  const showInstall = () => {
    modal.info({
      title: '安装 fuse-overlayfs',
      width: 440,
      content: (
        <div style={{ fontSize: 13, lineHeight: 1.8 }}>
          <p style={{ margin: '0 0 8px' }}>
            先安装 fuse-overlayfs（按发行版调整），装完回到这里重新检查：
          </p>
          <Space.Compact style={{ width: '100%' }}>
            <Typography.Text code copyable={false} style={{ flex: 1, padding: '4px 8px' }}>
              {INSTALL_CMD}
            </Typography.Text>
            <Button
              icon={<CopyOutlined />}
              onClick={() => {
                navigator.clipboard.writeText(INSTALL_CMD);
                message.success('已复制安装命令');
              }}
            >
              复制
            </Button>
          </Space.Compact>
        </div>
      ),
    });
  };

  const dismiss = async () => {
    try {
      await invoke('storage_health_dismiss');
      setDismissed(true);
    } catch (err: any) {
      message.error(errMsg(err, '设置忽略失败'));
    }
  };

  const recheck = async () => {
    try {
      await invoke('storage_health_reset_dismiss');
      setDismissed(false);
      await load();
    } catch (err: any) {
      message.error(errMsg(err, '重新检查失败'));
    }
  };

  // dismissed → 仅保留「重新检查」入口（一行，不打扰）
  if (dismissed) {
    return (
      <div className="storage-health-banner dismissed">
        <Typography.Link onClick={recheck}>重新检查存储健康</Typography.Link>
      </div>
    );
  }

  if (checking || !report) {
    return null; // 不显示加载态（横幅是辅助提示）
  }

  if (report.verdict === 'ok' || report.verdict === 'not_applicable') {
    return null;
  }

  const isRecommended = report.verdict === 'recommended';

  return (
    <Alert
      className="storage-health-banner"
      type="warning"
      showIcon
      message={
        <span className="storage-health-message">
          {isRecommended
            ? `检测到 rootless + 原生 overlay：首次从镜像建容器会慢（~2s/GB）。可一键切换到 fuse-overlayfs（已有配置自动备份，可回滚）。${
                report.fuse_overlayfs ? `（${report.fuse_overlayfs}）` : ''
              }`
            : '检测到 rootless + 原生 overlay：首次从镜像建容器会慢（~2s/GB），且 fuse-overlayfs 未安装（或 /dev/fuse 缺失）。'}
        </span>
      }
      action={
        <Space size={8}>
          {isRecommended ? (
            <Button size="small" type="primary" loading={fixing} onClick={doFix}>
              一键修复
            </Button>
          ) : (
            <Button size="small" type="primary" onClick={showInstall}>
              查看安装命令
            </Button>
          )}
          {isRecommended && (
            <Button size="small" onClick={showInstall}>
              安装说明
            </Button>
          )}
          <Button size="small" onClick={dismiss}>
            忽略
          </Button>
        </Space>
      }
    />
  );
}
