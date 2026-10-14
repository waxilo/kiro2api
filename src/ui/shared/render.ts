// 通用 HTML 片段：徽标、键值行、指标块、空态、提示条。
//
// 这些片段在多个分区视图里重复出现，集中一处意味着「运行中」这个徽标在状态栏、
// 概览卡片、侧边栏三处必然是同一个样子；也意味着改一次配色不用翻六个文件。
//
// 约定：所有接收「值」的地方都接收**已经转义过或受控**的 HTML 片段 ——
// 本模块只负责结构，不负责转义，调用方用 `core/format` 里的 `esc` / `text` 处理数据。
import { esc } from '../../core/format';
import { actionScope } from '../../state/actions';
import { isBlocking, isPending, type AppState, type StatusSummary, type Tone, type ViewId } from '../../state/app-state';
import { ICON } from './icons';

/**
 * 动作按钮的「暂时不能做」属性。
 *
 * 一律走 `aria-disabled` 而**不用** `disabled`：被禁用的表单控件连 click 都不会派发
 * （更不会冒泡到外壳的事件委托），于是 `state/actions` 里那句「正在刷新凭据，请稍候」
 * 永远进不去 —— 用户看到的还是一颗点了没反应的灰按钮。`aria-disabled` 在语义上同样是
 * 「不可用」（读屏会播报），但点击照常派发，由 `run()` 统一给出解释；重复执行也由它挡住。
 *
 * 什么情况算「现在不能做」，两种：
 * * 这颗按钮自己正在跑（它自己的文案已经在写「…中」，再点一次等于重复请求）；
 * * 别处有**会改配置**的动作在跑，而它自己也是会改配置的那一类。
 *   范围由 `actionScope` 给出：刷新凭据、查询额度是 `scoped`，永远不会被别处的忙碌波及
 *   —— 那正是「刷新凭据等 30 秒不该锁住整页」这条要求的落点。
 *
 * 真正的 `disabled` 只留给「这颗按钮自己现在无事可做」（没有地址可复制、代理没在运行）
 * —— 那些原因就写在同一个卡片里，静默是对的。
 */
export function actionAttrs(state: AppState, label: string): string {
  if (isPending(state, label)) return ' aria-disabled="true"';
  const blocked = actionScope(label) === 'global' && isBlocking(state);
  return blocked ? ' aria-disabled="true"' : '';
}

/** 状态徽标：一个色点 + 短标签。 */
export function statusPill(summary: StatusSummary): string {
  return (
    `<span class="pill ${summary.tone}">` +
    `<span class="pill-dot"></span>${esc(summary.label)}</span>`
  );
}

/** 语义色调标签（更新可用、已是最新…）。 */
export function tonePill(tone: Tone, label: string): string {
  return `<span class="pill ${tone}"><span class="pill-dot"></span>${esc(label)}</span>`;
}

/** 一行键值。`value` 需为已转义或受控的 HTML。 */
export function kv(label: string, value: string): string {
  return `<dt>${esc(label)}</dt><dd>${value}</dd>`;
}

/** 键值列表容器。 */
export function kvList(rows: string[]): string {
  return `<dl class="kv">${rows.join('')}</dl>`;
}

/** 等宽值：用于路径、地址、密钥这类需要逐字符辨认的内容。 */
export function mono(value: unknown, fallback = '—'): string {
  if (value === null || value === undefined || value === '') {
    return `<span class="dim">${esc(fallback)}</span>`;
  }
  return `<code>${esc(value)}</code>`;
}

/** 提示条。`tone` 传 `'info'` 得到中性底色。 */
export function note(body: string, tone: 'info' | Tone = 'info'): string {
  return `<div class="note${tone === 'info' ? '' : ` ${tone}`}">${body}</div>`;
}

export interface StatTileSpec {
  label: string;
  /** 主数值，需为已转义或受控的 HTML。 */
  value: string;
  hint?: string;
  /** 给了就整块可点，跳到该分区。 */
  goto?: ViewId;
}

/** 指标块：一个小标签 + 一个大数值（+ 可选脚注）。可点时右侧出现箭头。 */
export function statTile(spec: StatTileSpec): string {
  const inner = `
    <div class="stat-label">${esc(spec.label)}</div>
    <div class="stat-value">${spec.value}</div>
    ${spec.hint ? `<div class="stat-hint">${esc(spec.hint)}</div>` : ''}`;

  if (!spec.goto) return `<div class="stat">${inner}</div>`;
  return `
    <button class="stat is-link" data-action="goto" data-view="${spec.goto}" type="button">
      ${inner}
      <span class="stat-arrow">${ICON.chevron}</span>
    </button>`;
}

/** 空态：一个图标块 + 标题 + 一句引导。 */
export function emptyState(title: string, hint = '', icon: string = ICON.info): string {
  return `
    <div class="empty">
      <span class="empty-icon">${icon}</span>
      <div class="empty-title">${esc(title)}</div>
      ${hint ? `<div class="empty-hint">${esc(hint)}</div>` : ''}
    </div>`;
}
