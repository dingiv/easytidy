// 统一错误信息提取。透明原则：底层失败时尽量返回清晰友好的提示，
// 帮用户快速决策——后端已返回 anyhow `{:#}` 全错误链（含上下文），
// 前端只负责把各种错误形态归一化成可读字符串、如实展示。

/**
 * 归一化后端错误为可读字符串。
 *
 * Tauri 命令返回 `Result<T, String>`，失败时 promise 以「带 `.message`
 * 的错误对象」reject；但也可能出现裸字符串（部分命令直接 throw string）、
 * `Error` 实例、或未知对象。此处统一兜底，绝不抛 `[object Object]`。
 *
 * @param err     捕获的未知错误
 * @param fallback 无法提取时的兜底文案（如「启动失败」）
 */
export function errMsg(err: unknown, fallback = '操作失败'): string {
  if (err == null) return fallback;
  if (typeof err === 'string') return err.trim() || fallback;
  if (err instanceof Error) return err.message?.trim() || fallback;
  if (typeof err === 'object') {
    const m = (err as { message?: unknown }).message;
    if (typeof m === 'string' && m.trim()) return m.trim();
    const s = String(err);
    if (s && s !== '[object Object]') return s;
  }
  return fallback;
}
