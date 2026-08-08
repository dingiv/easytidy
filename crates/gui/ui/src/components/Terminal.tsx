// 终端组件：xterm + server PTY 流。
//
// 多终端（2026-08-08）：每条会话一条专用连接，面板由 PerContainer 常驻渲染
// （display 切换，切 tab 不销毁）。会话生命周期由 server 持有：
// - 新开：pty_open{persistent} → 独立持久会话（不随连接断开清理）
// - 重开窗口/重挂载：pty_open{attach_stream} → server 清屏 + 环形缓冲回放
//   恢复屏幕（无需本地缓存流）
// 会话退出（pty.exited）→ 清理 store cwd → 通知父面板关闭。

import { memo, useEffect, useRef, useState } from 'react';
import { Terminal as XTerminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { ClipboardAddon } from '@xterm/addon-clipboard';
import { invoke } from '@tauri-apps/api/core';
import { Channel } from '@tauri-apps/api/core';
import '@xterm/xterm/css/xterm.css';
import type { PtyEvent } from '../types';
import { useTerminalStore } from '../stores/terminalStore';

/** 复制到系统剪贴板（Tauri WebView 非安全上下文下 navigator.clipboard
 * 可能不可用 → execCommand 兜底；execCommand 需用户手势内调用） */
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

interface TerminalProps {
  /** 以 root 身份运行（false = node 常规终端；各自独立持久会话） */
  asRoot?: boolean;
  /** 已有会话 stream_id：附接重连（server 回放当前屏幕）；
   *  缺省/null = 新建独立持久会话（多终端实例） */
  streamId?: number | null;
  /** 会话建立后回调（新建路径拿到 stream_id，父面板绑定/关闭用） */
  onStream?: (streamId: number) => void;
  /** 会话退出（pty.exited）→ 父面板关闭 */
  onExit?: (streamId: number) => void;
}

function TerminalInner({ asRoot, streamId, onStream, onExit }: TerminalProps) {
  const terminalRef = useRef<HTMLDivElement>(null);
  const terminalInstance = useRef<XTerminal | null>(null);
  const fitAddonRef = useRef<FitAddon | null>(null);
  const streamIdRef = useRef<number | null>(null);
  const [exited, setExited] = useState(false);

  useEffect(() => {
    if (!terminalRef.current || exited) return;

    // Clean up previous instance
    if (terminalInstance.current) {
      terminalInstance.current.dispose();
      terminalInstance.current = null;
    }
    if (fitAddonRef.current) {
      fitAddonRef.current = null;
    }
    // streamIdRef 不清：PTY 流跨挂载保持（连接不断开，输出持续）

    const terminalEl = terminalRef.current;
    terminalEl.innerHTML = '';

    // Create terminal instance
    const term = new XTerminal();
    term.options.convertEol = true;
    term.options.cursorBlink = true;
    term.options.fontSize = 14;
    // 拉丁等宽字体在前（避免 CJK 字体宽字距），CJK 字体作为中文回退
    term.options.fontFamily = '"DejaVu Sans Mono", "Noto Sans Mono", "Noto Sans CJK SC", monospace';
    term.options.theme = {
      background: '#1e1e2e',
      foreground: '#cdd6f4',
      cursor: '#f5e0dc',
      selectionBackground: '#45475a',
    };

    const fitAddon = new FitAddon();
    term.loadAddon(fitAddon);

    // 复制/粘贴（Ctrl+Shift+C/V、右键菜单）
    term.loadAddon(new ClipboardAddon());

    try {
      term.open(terminalEl);
      fitAddon.fit();
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

    // PTY 事件处理（data → xterm 输出；exited → 提示 + 失效缓存）。
    // 写入批量（rAF flush）：高频输出（回放/大输出）逐块 write 触发大量 DOM
    // 渲染（社区实测 100+ renders/sec）；攒帧后一帧内 flush 一次 write，
    // DOM 渲染器下闪烁与卡顿显著降低（调研 2026-08-07）
    let pendingWrites: number[] = [];
    let writeRaf: number | null = null;
    const flushWrites = () => {
      writeRaf = null;
      if (pendingWrites.length === 0) return;
      const data = new Uint8Array(pendingWrites);
      pendingWrites = [];
      term.write(data);
    };
    const handlePtyEvent = (event: PtyEvent) => {
      if (event.kind === 'data' && event.data) {
        // 循环 push（不用 spread：帧超过 ~65535 元素会 RangeError）
        for (const b of event.data) pendingWrites.push(b);
        if (writeRaf === null) {
          writeRaf = requestAnimationFrame(flushWrites);
        }
      } else if (event.kind === 'cwdChanged' && event.cwd) {
        // server TTY 事件驱动推送（输入回车时检测）：更新 store 供跟随
        const sid = streamIdRef.current;
        if (sid !== null) {
          useTerminalStore.getState().setCwd(sid, event.cwd);
        }
      } else if (event.kind === 'exited') {
        if (writeRaf !== null) {
          cancelAnimationFrame(writeRaf);
          writeRaf = null;
          flushWrites();
        }
        const code = event.code ?? 0;
        term.writeln(`\r\n\x1b[90m[Process exited with code ${code}]\x1b[0m`);
        // 会话已终结：清理 store cwd，通知父面板关闭（关闭面板 = 关闭终端）
        const sid = streamIdRef.current;
        if (sid !== null) {
          useTerminalStore.getState().clearCwd(sid);
          onExit?.(sid);
        }
        setExited(true);
      }
    };

    // Get initial size
    const { cols, rows } = term;

    // 多终端：已有会话 → 附接重连（server 清屏 + 环形缓冲回放恢复屏幕）；
    // 无 → 新建独立持久会话（server 持有，不随连接断开清理）。
    // ⚠️ 面板可能在 pty_open 返回前被关闭（快速开-关）：resolve 后检测
    // cancelled，立即 pty_close 回收刚建的会话，避免孤儿常驻终端。
    let streamCancelled = false;
    const ch = new Channel<PtyEvent>();
    ch.onmessage = handlePtyEvent;

    invoke<number>('pty_open', {
      onEvent: ch,
      cmd: null,
      cols,
      rows,
      // ⚠️ Tauri 2 invoke 参数为 camelCase（Rust snake_case 自动转换，
      // 如 streamId→stream_id）；传 snake_case 会被静默忽略——曾导致
      // as_root 丢失、root 终端 attach 到 node 会话（实测）
      asRoot: asRoot ?? false,
      persistent: streamId == null,          // 新开 = 独立持久会话
      attachStreamId: streamId ?? undefined, // 重连 = 附接已有会话
    })
      .then((sid) => {
        if (streamCancelled) {
          // 面板已卸载：回收刚建的会话（孤儿常驻终端）
          invoke('pty_close', { streamId: sid }).catch(() => {});
          return;
        }
        streamIdRef.current = sid;
        onStream?.(sid);
        term.focus();
      })
      .catch((err) => {
        console.error('pty_open failed:', err);
        term.writeln(`\r\n\x1b[91mFailed to open PTY: ${err}\x1b[0m`);
      });

    // 焦点管理（WebKitGTK 已知坑：窗口失焦/切走后再回来，xterm 的 textarea
    // 点击无法重新获得焦点——光标在闪但键盘事件进不了 xterm，表现为"终端
    // 失去响应"）。窗口/文档恢复焦点时自动 focus；点击终端区域强制 focus。
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

    // Ctrl+Shift+C 复制选中文本。⚠️ 捕获阶段（capture=true）先于 xterm
    // textarea 的 keydown 处理——否则组合键被当作输入发给 PTY，且
    // ClipboardAddon 在 WebKitGTK 下可能失效（Tauri 非安全上下文）
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

    // Handle terminal input。
    // ⚠️ 大文本（粘贴）必须 Uint8Array 直传 + 分块：Array.from 转数字数组 +
    // JSON 序列化会膨胀 4-5 倍（1MB 粘贴 → 3-5MB IPC），实测卡顿。
    // Uint8Array 经 Tauri 2 高效传输（Rust 侧 Vec<u8> 直接接收）；
    // 大输入分 64KB 块，小输入（打字）直发保持低延迟。
    let writeFailed = false;
    const INPUT_CHUNK = 64 * 1024;
    term.onData((data: string) => {
      if (streamIdRef.current === null) return;
      const bytes = new TextEncoder().encode(data);
      const streamId = streamIdRef.current;
      const sendChunk = (chunk: Uint8Array) => {
        invoke('pty_write', { streamId, data: chunk }).catch((err) => {
          // 写失败必须可见：静默吞掉会让用户面对"无法输入"而不知原因
          if (!writeFailed) {
            writeFailed = true;
            term.writeln(`\r\n\x1b[91m[输入通道错误：${err}，请刷新或切换 tab 重连]\x1b[0m`);
          }
          console.error('pty_write failed:', err);
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

    // 心跳保活：server 对无帧连接有 idle 超时（有 PTY 的连接 1h），
    // 30s ping 确保长挂机/无输出时连接不被回收
    const pingTimer = setInterval(() => {
      if (streamIdRef.current === null) return;
      invoke('pty_ping', { streamId: streamIdRef.current }).catch(() => {
        // 连接已断：ping 失败会反映到下一次 pty_write 的错误提示，无需在此处理
      });
    }, 30_000);

    // Handle resize with debounce。
    // ⚠️ 宽度时序（2026-08-07 实测方向）：切 tab 恢复瞬间容器尺寸过渡
    // （0 → 部分 → 全），过早 fit 会用中间尺寸 resize PTY → shell 按小宽度
    // 换行 → 提示符截断；随后布局稳定又被调大。对策：
    // 1. 150ms 防抖（连续尺寸变化取最终值）
    // 2. 双 requestAnimationFrame（布局稳定后）再 fit
    // 3. 尺寸为 0（隐藏/过渡期）跳过
    let resizeTimeout: ReturnType<typeof setTimeout> | null = null;
    const fitAndResize = () => {
      if (!fitAddonRef.current || !terminalInstance.current) return;
      const el = terminalRef.current;
      if (!el || el.clientWidth === 0 || el.clientHeight === 0) return; // 隐藏/过渡期
      fitAddonRef.current.fit();
      const { cols: newCols, rows: newRows } = terminalInstance.current;
      if (streamIdRef.current !== null && (newCols !== cols || newRows !== rows)) {
        invoke('pty_resize', {
          streamId: streamIdRef.current,
          cols: newCols,
          rows: newRows,
        }).catch((err) => console.error('pty_resize failed:', err));
      }
    };
    const scheduleResize = () => {
      if (resizeTimeout) clearTimeout(resizeTimeout);
      resizeTimeout = setTimeout(() => {
        // 双 rAF：等布局稳定（display:none → block 恢复的过渡期）
        requestAnimationFrame(() => {
          requestAnimationFrame(fitAndResize);
        });
      }, 150);
    };
    const resizeObserver = new ResizeObserver(scheduleResize);

    resizeObserver.observe(terminalEl);

    // 可见性恢复显式刷新：xterm 内部 IntersectionObserver 在 display:none
    // 时暂停渲染（_isPaused），恢复可见触发全屏 full refresh——切 tab 回来
    // 闪烁的直接原因。自己挂 IntersectionObserver，恢复可见时 refresh +
    // focus，把重绘提前到切换瞬间（社区标准做法，调研 2026-08-07）
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

    // Cleanup
    return () => {
      streamCancelled = true;
      resizeObserver.disconnect();
      visibilityObserver.disconnect();
      if (writeRaf !== null) {
        cancelAnimationFrame(writeRaf);
        writeRaf = null;
      }
      if (resizeTimeout) clearTimeout(resizeTimeout);
      clearInterval(pingTimer);
      document.removeEventListener('visibilitychange', restoreFocus);
      window.removeEventListener('focus', restoreFocus);
      terminalEl.removeEventListener('pointerdown', forceFocus);
      terminalEl.removeEventListener('keydown', handleCopyKey, true);
      // PTY 流不关闭：缓存供复用（连接保持，server 常驻会话语义）
      // streamCache 不清——remount 复用；exited 时才失效
      term.dispose();
    };
  }, [exited]);

  // Handle keyboard composition events for IME
  useEffect(() => {
    const term = terminalInstance.current;
    if (!term || !term.element) return;

    const handleKeydown = (e: KeyboardEvent) => {
      // Ignore keydown during composition or when keyCode is 229 (composition key)
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
export const Terminal = memo(TerminalInner);
