import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { invoke } from "@tauri-apps/api/core";
import "@xterm/xterm/css/xterm.css";

// DevTools 按钮（spike 调试）
document.querySelector("#devtools-btn")?.addEventListener("click", () => {
  invoke("toggle_devtools").catch((e) => console.error("toggle_devtools failed", e));
});

// HMR 安全：热重载时先销毁旧终端，避免对同一元素二次 open 导致空白/报错
let term: Terminal | null = null;
let fitAddon: FitAddon | null = null;

function termLog(tag: string, ev: string) {
  term?.writeln(`\x1b[90m[${tag}] ${ev}\x1b[0m`);
}

function initTerminal() {
  const terminalEl = document.querySelector<HTMLElement>("#terminal");

  console.log("terminal init")

  if (!terminalEl) return;

  // 清掉旧实例与残留 DOM（HMR 复用场景）
  term?.dispose();
  terminalEl.innerHTML = "";

  term = new Terminal({ convertEol: true, cursorBlink: true });
  fitAddon = new FitAddon();
  term.loadAddon(fitAddon);

  try {
    term.open(terminalEl);
    fitAddon.fit();
  } catch (e) {
    // xterm 初始化异常直接写进页面可见区域（便于无 devtools 时定位）
    const err = document.createElement("pre");
    err.style.color = "#f38ba8";
    err.textContent = `xterm open 抛错: ${String(e)}\n${(e as Error)?.stack ?? ""}`;
    terminalEl.appendChild(err);
    console.error("xterm open failed", e);
    return;
  }

  term.onData((data) => {
    // 显式 CRLF
    term?.write(data.replace(/\r/g, "\r\n"));
    if (data.includes("\r")) termLog("DATA", JSON.stringify(data));
  });

  term.element?.addEventListener("compositionstart", () =>
    termLog("IME", "compositionstart"),
  );
  term.element?.addEventListener("compositionupdate", (e) =>
    termLog("IME", `compositionupdate: ${(e as CompositionEvent).data}`),
  );
  term.element?.addEventListener("compositionend", (e) =>
    termLog("IME", `compositionend: ${(e as CompositionEvent).data}`),
  );
  term.element?.addEventListener("keydown", (e) => {
    const kd = e as KeyboardEvent;
    if (kd.isComposing || kd.keyCode === 229 || kd.key === "Enter") {
      termLog(
        "KEY",
        `keydown keyCode=${kd.keyCode} key=${JSON.stringify(kd.key)} ` +
          `isComposing=${kd.isComposing}`,
      );
    }
  });
}

function disposeTerminal() {
  term?.dispose();
  term = null;
  fitAddon = null;
}

// VITE_NOTERM=1 时跳过终端（隔离实验用）
//const noterm = import.meta.env.VITE_NOTERM === "1";
// if (!noterm) {
// }
initTerminal();

// HMR：接受更新，销毁旧终端后重载页面状态
if (import.meta.hot) {
  import.meta.hot.dispose(() => disposeTerminal());
  import.meta.hot.accept();
}

window.addEventListener("resize", () => fitAddon?.fit());

// 目标 D：环境提示
const envInfo = document.querySelector<HTMLElement>("#env-info");
if (envInfo) {
  envInfo.textContent = [
    `userAgent: ${navigator.userAgent}`,
    "运行前在启动终端执行: env | grep -E 'IM_MODULE|XMODIFIERS|XDG_SESSION_TYPE'",
  ].join("\n");
}
