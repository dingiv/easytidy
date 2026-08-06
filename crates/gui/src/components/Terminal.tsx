import { useEffect, useRef, useState } from 'react';
import { Terminal as XTerminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { ClipboardAddon } from '@xterm/addon-clipboard';
import { invoke } from '@tauri-apps/api/core';
import { Channel } from '@tauri-apps/api/core';
import '@xterm/xterm/css/xterm.css';
import type { PtyEvent } from '../types';

export function Terminal() {
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
    if (streamIdRef.current !== null) {
      // Don't close stream on remount - let it continue
    }

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

    // Set up PTY channel
    const ch = new Channel<PtyEvent>();

    ch.onmessage = (event: PtyEvent) => {
      if (event.kind === 'data' && event.data) {
        term.write(new Uint8Array(event.data));
      } else if (event.kind === 'exited') {
        const code = event.code ?? 0;
        term.writeln(`\r\n\x1b[90m[Process exited with code ${code}]\x1b[0m`);
        setExited(true);
      }
    };

    // Get initial size
    const { cols, rows } = term;

    // Open PTY
    invoke<number>('pty_open', {
      onEvent: ch,
      cmd: null,
      cols,
      rows,
    })
      .then((sid) => {
        streamIdRef.current = sid;
      })
      .catch((err) => {
        console.error('pty_open failed:', err);
        term.writeln(`\r\n\x1b[91mFailed to open PTY: ${err}\x1b[0m`);
      });

    // Handle terminal input
    term.onData((data: string) => {
      if (streamIdRef.current === null) return;
      const encoder = new TextEncoder();
      const dataArr = Array.from(encoder.encode(data));
      invoke('pty_write', {
        streamId: streamIdRef.current,
        data: dataArr,
      }).catch((err) => console.error('pty_write failed:', err));
    });

    // Handle resize with debounce
    let resizeTimeout: ReturnType<typeof setTimeout> | null = null;
    const resizeObserver = new ResizeObserver(() => {
      if (resizeTimeout) clearTimeout(resizeTimeout);
      resizeTimeout = setTimeout(() => {
        if (!fitAddonRef.current || !terminalInstance.current) return;
        fitAddonRef.current.fit();
        const { cols: newCols, rows: newRows } = terminalInstance.current;
        if (streamIdRef.current !== null && (newCols !== cols || newRows !== rows)) {
          invoke('pty_resize', {
            streamId: streamIdRef.current,
            cols: newCols,
            rows: newRows,
          }).catch((err) => console.error('pty_resize failed:', err));
        }
      }, 100);
    });

    resizeObserver.observe(terminalEl);

    // Cleanup
    return () => {
      resizeObserver.disconnect();
      if (resizeTimeout) clearTimeout(resizeTimeout);
      if (streamIdRef.current !== null) {
        // Keep stream open - just detach UI
      }
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
