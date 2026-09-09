// root 终端组件：xterm + 容器内 root 通道（easytidy-dock daemon）。
//
// 与用户终端（Terminal.tsx）的区别：
// - **单例共享会话**：每容器一个 root shell（容器内父 = conmon），无
//   stream_id 概念（流 ID 恒 ROOT_STREAM_ID）；多面板/重连 attach 同一
//   会话只 fan-out，不新建
// - **attach 即回放**：root_terminal_attach 自动 2026 包裹回放恢复屏幕
// - **detach 语义**：面板关闭/重连 = 退订（root shell 与 root 通道继续
//   运行）；真正关闭 root shell 是 root_terminal_close（父面板负责调用）
// - 无 pty_ping（root 通道无 idle 超时；连接由进程持有）
// - 无 cwd 跟随（root 终端不进文件浏览器跟随）

import { memo, useEffect, useRef, useState } from 'react';
import { Terminal as XTerminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { ClipboardAddon } from '@xterm/addon-clipboard';
import { invoke, Channel } from '@tauri-apps/api/core';
import '@xterm/xterm/css/xterm.css';
import type { PtyEvent } from '../types';

/** 复制到系统剪贴板（与 Terminal.tsx 同款：非安全上下文 execCommand 兜底） */
function copyToClipboard(text: string) {
  if (navigator.clipboard?.writeText) {
    navigator.clipboard.writeText(text).catch(() => fallbackCopy(text));
    return;
  }
  fallbackCopy(text);
}

function fallbackCopy(text: string) {
  const ta = document.createElement('textarea');
  ta.value = text;
  ta.style.position = 'fixed';
  ta.style.opacity = '0';
  document.body.appendChild(ta);
  ta.select();
  try {
    document.execCommand('copy');
  } catch (e) {
    console.error('copy failed:', e);
  }
  document.body.removeChild(ta);
}

interface RootTerminalProps {
  /** root 会话退出（rc.exited / easytidy-dock 退出）→ 父面板关闭 */
  onExited?: () => void;
}

function RootTerminalInner({ onExited }: RootTerminalProps) {
  const terminalRef = useRef<HTMLDivElement>(null);
  const terminalInstance = useRef<XTerminal | null>(null);
  const fitAddonRef = useRef<FitAddon | null>(null);
  const [exited, setExited] = useState(false);

  useEffect(() => {
    if (!terminalRef.current || exited) return;

    if (terminalInstance.current) {
      terminalInstance.current.dispose();
      terminalInstance.current = null;
    }
    if (fitAddonRef.current) {
      fitAddonRef.current = null;
    }

    const terminalEl = terminalRef.current;
    terminalEl.innerHTML = '';

    const term = new XTerminal();
    term.options.convertEol = true;
    term.options.cursorBlink = true;
    term.options.fontSize = 14;
    term.options.fontFamily = '"DejaVu Sans Mono", "Noto Sans Mono", "Noto Sans CJK SC", monospace';
    term.options.theme = {
      background: '#1e1e2e',
      foreground: '#cdd6f4',
      cursor: '#f5e0dc',
      selectionBackground: '#45475a',
    };

    const fitAddon = new FitAddon();
    term.loadAddon(fitAddon);
    term.loadAddon(new ClipboardAddon());

    try {
      term.open(terminalEl);
    } catch (e) {
      console.error('xterm open failed:', e);
      const err = document.createElement('pre');
      err.style.color = '#f38ba8';
      err.textContent = `Terminal init error: ${String(e)}`;
      terminalEl.appendChild(err);
      return;
    }

    terminalInstance.current = term;
    fitAddonRef.current = fitAddon;

    // base64 解码直写 xterm（后端已 base64 编码；xterm 内部写缓冲保序）
    const b64ToBytes = (b64: string): Uint8Array => {
      const bin = atob(b64);
      const bytes = new Uint8Array(bin.length);
      for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
      return bytes;
    };
    const handlePtyEvent = (event: PtyEvent) => {
      if (event.kind === 'data' && event.data) {
        term.write(b64ToBytes(event.data));
      } else if (event.kind === 'exited') {
        term.writeln('\r\n\x1b[90m[root 会话已退出]\x1b[0m');
        setExited(true);
        onExited?.();
      }
    };

    // 挂载 root 会话（attach 自动 2026 包裹回放）。写失败 = 连接断 →
    // 重新 attach（共享会话仍在，只重连本面板的输出通道）。
    let cancelled = false;
    let reconnecting = false;
    let writeFailed = false;
    let streamGen = 0;

    const attach = async (hint: string | null) => {
      const gen = ++streamGen;
      const ch = new Channel<PtyEvent>();
      ch.onmessage = (ev) => {
        if (gen === streamGen) handlePtyEvent(ev); // 旧代事件丢弃
      };
      try {
        await invoke('root_terminal_attach', {
          onEvent: ch,
          cols: term.cols,
          rows: term.rows,
        });
        if (cancelled) return;
        writeFailed = false;
        if (hint) {
          term.writeln(`\r\n\x1b[32m[已重连${hint}]\x1b[0m`);
        }
        term.focus();
      } catch (err) {
        console.error('root_terminal_attach failed:', err);
        if (!cancelled) {
          term.writeln(`\r\n\x1b[91m[root 终端${hint ?? '挂载'}失败：${err}]\x1b[0m`);
        }
      }
    };

    const reconnect = (reason: unknown) => {
      if (cancelled || reconnecting) return;
      reconnecting = true;
      term.writeln(`\r\n\x1b[33m[通道断开（${reason}），重连中…]\x1b[0m`);
      attach('重连').finally(() => {
        reconnecting = false;
      });
    };

    // 初始 attach 延迟到布局稳定（双 rAF + 150ms 防抖），与 ResizeObserver
    // 共用同一节奏。
    //
    // 原因：WorkerView 挂载瞬间 main-panel 从 pane-empty 切到 tab-pane（含
    // RootTerminal），flex 链 `.app > .per-layout > .per-right > .main-panel
    // > .tab-pane > .terminal-wrapper > .terminal-container` 要算高度。
    // useEffect 同步 fit 时容器可能还在过渡态（0 → 部分 → 全），拿到的是
    // 偏小的 cols。把这个偏小值传到 daemon = PTY resize 到小宽度 + 回放
    // 按小宽度渲染 + bash 按小宽度排版 = 提示符 wrap（`root ➜ ~\r\n $ `）。
    //
    // xterm 不回溯已渲染内容：迟到的 ResizeObserver 二次 fit 把 xterm 调大
    // 后，老的 wrap 仍留在屏幕上；bash 已经在小宽度下重绘过 prompt，新
    // 提示符也按小宽度出。直到再次 SIGWINCH 才会重绘。
    //
    // 推迟 attach + 重新 fit 取稳定值是 root 终端的关键（用户终端同款问题
    // 由首次建流 + SIGWINCH 自动重绘覆盖，因为新会话没有遗留 ring 内容；
    // root 走 attach 到已有 session + 回放历史 → wrap 视觉持久化）。
    let initialAttachTimer: ReturnType<typeof setTimeout> | null = null;
    const doInitialAttach = () => {
      if (cancelled) return;
      fitAddon.fit();
      attach(null);
    };
    initialAttachTimer = setTimeout(() => {
      requestAnimationFrame(() => {
        requestAnimationFrame(doInitialAttach);
      });
    }, 150);

    // 焦点管理（与 Terminal.tsx 同款）
    const restoreFocus = () => {
      if (document.hasFocus() && terminalInstance.current) {
        terminalInstance.current.focus();
      }
    };
    document.addEventListener('visibilitychange', restoreFocus);
    window.addEventListener('focus', restoreFocus);
    const forceFocus = () => {
      terminalInstance.current?.focus();
    };
    terminalEl.addEventListener('pointerdown', forceFocus);

    const handleCopyKey = (e: KeyboardEvent) => {
      if (e.ctrlKey && e.shiftKey && (e.key === 'C' || e.key === 'c')) {
        const sel = term.getSelection();
        if (sel) {
          e.preventDefault();
          e.stopPropagation();
          copyToClipboard(sel);
        }
      }
    };
    terminalEl.addEventListener('keydown', handleCopyKey, true);

    // 输入 → root_terminal_write（大文本分块，与 Terminal.tsx 同款）
    const INPUT_CHUNK = 64 * 1024;
    term.onData((data: string) => {
      const bytes = new TextEncoder().encode(data);
      const sendChunk = (chunk: Uint8Array) => {
        invoke('root_terminal_write', { data: chunk }).catch((err) => {
          if (!writeFailed) {
            writeFailed = true;
            console.error('root_terminal_write failed:', err);
            reconnect(err);
          }
        });
      };
      if (bytes.length <= INPUT_CHUNK) {
        sendChunk(bytes);
      } else {
        for (let i = 0; i < bytes.length; i += INPUT_CHUNK) {
          sendChunk(bytes.subarray(i, i + INPUT_CHUNK));
        }
      }
    });

    // resize（150ms 防抖 + 双 rAF，与 Terminal.tsx 同款）
    let resizeTimeout: ReturnType<typeof setTimeout> | null = null;
    const fitAndResize = () => {
      if (!fitAddonRef.current || !terminalInstance.current) return;
      const el = terminalRef.current;
      if (!el || el.clientWidth === 0 || el.clientHeight === 0) return;
      fitAddonRef.current.fit();
      const { cols, rows } = terminalInstance.current;
      invoke('root_terminal_resize', { cols, rows }).catch((err) =>
        console.error('root_terminal_resize failed:', err),
      );
    };
    const scheduleResize = () => {
      if (resizeTimeout) clearTimeout(resizeTimeout);
      resizeTimeout = setTimeout(() => {
        requestAnimationFrame(() => {
          requestAnimationFrame(fitAndResize);
        });
      }, 150);
    };
    const resizeObserver = new ResizeObserver(scheduleResize);
    resizeObserver.observe(terminalEl);

    // 可见性恢复显式刷新（与 Terminal.tsx 同款）
    const visibilityObserver = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting) && terminalInstance.current) {
          terminalInstance.current.refresh(0, terminalInstance.current.rows - 1);
          terminalInstance.current.focus();
        }
      },
      { threshold: 0 },
    );
    visibilityObserver.observe(terminalEl);

    return () => {
      cancelled = true;
      resizeObserver.disconnect();
      visibilityObserver.disconnect();
      if (resizeTimeout) clearTimeout(resizeTimeout);
      if (initialAttachTimer) clearTimeout(initialAttachTimer);
      document.removeEventListener('visibilitychange', restoreFocus);
      window.removeEventListener('focus', restoreFocus);
      terminalEl.removeEventListener('pointerdown', forceFocus);
      terminalEl.removeEventListener('keydown', handleCopyKey, true);
      term.dispose();
      // 不 root_terminal_close：detach 语义——root 会话保留（关面板 ≠ 关会话）
    };
  }, [exited]);

  // IME 组合键（与 Terminal.tsx 同款）
  useEffect(() => {
    const term = terminalInstance.current;
    if (!term || !term.element) return;
    const handleKeydown = (e: KeyboardEvent) => {
      if ((e as any).isComposing || e.keyCode === 229) {
        e.stopPropagation();
      }
    };
    term.element.addEventListener('keydown', handleKeydown);
    return () => {
      if (term.element) {
        term.element.removeEventListener('keydown', handleKeydown);
      }
    };
  }, []);

  return (
    <div className="terminal-wrapper">
      <div ref={terminalRef} className="terminal-container" style={{ height: '100%', width: '100%' }} />
    </div>
  );
}

/** memo 包裹：无 props，父组件重渲染不触发重建 */
export const RootTerminal = memo(RootTerminalInner);
