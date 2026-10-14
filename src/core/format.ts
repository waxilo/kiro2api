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

/**
 * 只留英文：模型名 / 交易对名这类标识符允许的字符集 —— **可打印 ASCII 去掉空格**。
 *
 * 取 `[^\x21-\x7e]` 而不是 market-monitor 的 `[^A-Za-z0-9_]`：那边的字母表是交易对名
 * （`BTC_USDT`），这边的模型名长 `claude-opus-4.6` 这样，**点和连字符必须留着**，
 * 照抄会把搜 `4.6` 变成搜 `46`。空格也要去掉 —— 模型名里没有空格，放进来只会让
 * 一次误按空格把后续输入全变成「搜不到」。
 *
 * 用在两处：① 自绘输入框的按键闸门（`ui/shared/english-field.ts`，中文、全角、标点
 * 根本进不了框）；② 搜索词落库前的兜底（`state/actions.ts`，挡住粘贴整段文本那条路）。
 */
export function englishOnly(raw: string): string {
  return raw.replace(/[^\x21-\x7e]/g, '');
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
