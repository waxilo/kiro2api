// 纯函数：转义与数值格式化。无副作用、不碰 DOM，因此可以被任何层引用。

/**
 * HTML 转义。
 *
 * 界面全部用模板字符串拼 HTML，任何来自后端的值（模型名、路径、错误文本）都必须先过这里，
 * 否则一个带 `<` 的路径就能破坏结构。少写一次转义就是一处 XSS 面。
 */
export function esc(value: unknown): string {
  return String(value ?? '')
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

/**
 * 转义后的「有则显示、无则占位」。
 *
 * 模板里 `esc(x || '—')` 这种写法出现频率极高，且容易漏掉转义或漏掉占位，
 * 收敛成一个函数后两边都不会忘。
 */
export function text(value: unknown, fallback = '—'): string {
  if (value === null || value === undefined || value === '') return fallback;
  return esc(value);
}

/** 把任意异常压成可展示的一行文本（Tauri 命令 reject 的是字符串）。 */
export function errorText(error: unknown): string {
  if (typeof error === 'string') return error;
  if (error instanceof Error) return error.message;
  return String(error);
}

/** 字节 → MB 字符串，用于更新包体积。 */
export function megabytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return '0.0';
  return (bytes / 1048576).toFixed(1);
}

/** 百分比：夹到 0–100 并取整，避免进度条宽度越界。 */
export function percent(part: number, whole: number): number {
  if (!Number.isFinite(part) || !Number.isFinite(whole) || whole <= 0) return 0;
  return Math.min(100, Math.max(0, Math.round((part / whole) * 100)));
}
