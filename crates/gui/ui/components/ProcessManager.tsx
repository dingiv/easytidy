// 进程管理器：查看/管理 easytidy-server 托管的进程（apps.ps）。
//
// 范围：经 server spawn 的托管子进程——passthrough 应用、桌面快捷方式
// （apps.launch_app）、auto-start；**不含终端 bash**（PTY 会话另有生命周期）。
// - 列表：3s 轮询 apps.ps（pid/name/kind/cmd/状态/启动时刻/stdio 字节数）
// - 关闭：apps.kill（SIGTERM；进程不存在/已退出时报错提示）
// - stdio：apps.logs 拉取该进程捕获的 stdout+stderr（有界环形缓冲），
//   弹窗展示 + 2s 自动刷新（进程退出后停止刷新，保留最后输出）

import { memo, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { CloseOutlined, FileTextOutlined } from '@ant-design/icons';
import { ManagedProcess } from '../types';
import { errMsg } from '../lib/errors';

const REFRESH_MS = 3000;
const LOG_REFRESH_MS = 2000;

function fmtTime(unixMillis: number): string {
  const d = new Date(unixMillis);
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

function ProcessManagerInner() {
  const [procs, setProcs] = useState<ManagedProcess[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [killingPid, setKillingPid] = useState<number | null>(null);
  // stdio 查看区：选中查看的进程（内嵌文本框展示，不用弹窗）；null = 未选中
  const [logView, setLogView] = useState<{ pid: number; name: string } | null>(null);
  const [logText, setLogText] = useState('');
  const logBoxRef = useRef<HTMLPreElement>(null);

  useEffect(() => {
    let cancelled = false;
    const refresh = async () => {
      try {
        const ps = await invoke<ManagedProcess[]>('app_ps');
        if (!cancelled) {
          setProcs(ps);
          setError(null);
        }
      } catch (err: any) {
        if (!cancelled) setError(errMsg(err, '获取进程列表失败'));
      }
    };
    void refresh();
    const timer = setInterval(refresh, REFRESH_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, []);

  // stdio 查看区：选中期间轮询刷新（进程退出后保留最后输出）
  useEffect(() => {
    if (!logView) return;
    let cancelled = false;
    let dead = false;
    const pull = async () => {
      try {
        const text = await invoke<string>('app_logs', { pid: logView.pid });
        if (cancelled) return;
        setLogText(text);
        // 滚到底（新输出在尾部）
        requestAnimationFrame(() => {
          const el = logBoxRef.current;
          if (el) el.scrollTop = el.scrollHeight;
        });
      } catch {
        // 单次拉取失败不打断轮询
      }
    };
    void pull();
    const timer = setInterval(() => {
      if (!dead) void pull();
    }, LOG_REFRESH_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [logView]);

  const handleKill = async (p: ManagedProcess) => {
    if (!confirm(`确定终止 ${p.name}（pid=${p.pid}）？`)) return;
    setKillingPid(p.pid);
    setError(null);
    try {
      await invoke('app_kill', { pid: p.pid });
    } catch (err: any) {
      setError(errMsg(err, `终止 ${p.name} 失败`));
    } finally {
      setKillingPid(null);
    }
  };

  return (
    <div className="proc-manager">
      {error && <div className="proc-error">{error}</div>}
      {procs.length === 0 ? (
        <div className="proc-empty">
          暂无托管进程。
          <br />
          通过 Passthrough、桌面快捷方式或 auto-start 拉起的应用会出现在这里。
        </div>
      ) : (
        <table className="proc-table">
          <thead>
            <tr>
              <th>PID</th>
              <th>名称</th>
              <th>类型</th>
              <th>命令</th>
              <th>启动时刻</th>
              <th>状态</th>
              <th>stdio</th>
              <th>操作</th>
            </tr>
          </thead>
          <tbody>
            {procs.map((p) => (
              <tr key={p.pid} className={p.status === 'running' ? '' : 'proc-exited'}>
                <td className="mono">{p.pid}</td>
                <td title={p.name}>{p.name}</td>
                <td>{p.kind}</td>
                <td className="mono proc-cmd" title={p.cmd}>
                  {p.cmd}
                </td>
                <td className="mono">{fmtTime(p.started_at)}</td>
                <td>
                  {p.status === 'running' ? (
                    <span className="proc-running">运行中</span>
                  ) : (
                    <span className="proc-dead">已退出（{p.exit_code ?? '?'}）</span>
                  )}
                </td>
                <td className="mono">{p.stdio_len}B</td>
                <td>
                  <div className="proc-actions">
                    <button
                      className="secondary-button proc-btn"
                      title="查看该进程捕获的 stdout+stderr"
                      onClick={() => {
                        setLogText('');
                        // 再点同一个 → 收起查看区
                        setLogView((cur) =>
                          cur?.pid === p.pid ? null : { pid: p.pid, name: p.name },
                        );
                      }}
                    >
                      <FileTextOutlined /> stdio
                    </button>
                    {p.status === 'running' && (
                      <button
                        className="secondary-button proc-btn proc-btn-danger"
                        disabled={killingPid === p.pid}
                        title="发送 SIGTERM 终止进程"
                        onClick={() => handleKill(p)}
                      >
                        <CloseOutlined /> {killingPid === p.pid ? '终止中…' : '关闭'}
                      </button>
                    )}
                  </div>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}

      {logView && (
        <div className="proc-stdio-box">
          <div className="proc-stdio-title">
            <span>
              stdio — {logView.name}（pid={logView.pid}）
            </span>
            <button
              className="secondary-button proc-btn"
              title="收起"
              onClick={() => setLogView(null)}
            >
              <CloseOutlined /> 收起
            </button>
          </div>
          <pre ref={logBoxRef} className="proc-stdio">
            {logText || '（暂无输出）'}
          </pre>
        </div>
      )}
    </div>
  );
}

export const ProcessManager = memo(ProcessManagerInner);
