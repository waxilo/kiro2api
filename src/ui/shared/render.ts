// 通用 HTML 片段：徽标、键值行、指标块、空态、提示条。
//
// 这些片段在多个分区视图里重复出现，集中一处意味着「运行中」这个徽标在状态栏、
// 概览卡片、侧边栏三处必然是同一个样子；也意味着改一次配色不用翻六个文件。
//
// 约定：所有接收「值」的地方都接收**已经转义过或受控**的 HTML 片段 ——
// 本模块只负责结构，不负责转义，调用方用 `core/format` 里的 `esc` / `text` 处理数据。
import { esc, formatRate } from '../../core/format';
import { actionScope } from '../../state/actions';
import {
  isBlocking,
  isPending,
  type AppState,
  type ModalId,
  type StatusSummary,
  type Tone,
} from '../../state/app-state';
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

/** 模型卡上的上游消耗倍率标注：已知才显示，未知整体省略（0 显示「免费」）。 */
export function rateLabel(rate: number | undefined): string {
  const label = formatRate(rate);
  return label ? `<span class="model-card-rate">${esc(label)}</span>` : '';
}

/** 一行键值。`value` 需为已转义或受控的 HTML。 */
export function kv(label: string, value: string): string {
  return `<dt>${esc(label)}</dt><dd>${value}</dd>`;
}

/** 键值列表容器。 */
export function kvList(rows: string[]): string {
  return `<dl class="kv">${rows.join('')}</dl>`;
}

/**
 * 会换文案的按钮内容：**宽度锁死**。
 *
 * 「启动代理 → 启动中…」「复制 → 已复制」这类替换会改变文字宽度，而按钮在卡片头、
 * 按钮行里都是靠边排的 —— 一个字宽的变化会让整排按钮左右挪一下，用户读到的是
 * 「界面抖了一下」，而不是「状态变了」。
 *
 * 做法是把两段文字都渲染出来、由样式叠在同一个网格单元里，只让当前那份可见：
 * 按钮宽度于是恒等于较宽的一份。不写死像素宽度，所以换字号、缩放、系统字体都不会失准。
 * 被藏起来的那份带 `aria-hidden`，读屏不会把两句话都念一遍。
 *
 * ⚠️ 叠放由**按钮自己**当网格容器完成（样式里的 `button:has(> .swap-alt)`）。
 * 别把它包成一个内联元素塞回按钮里：`inline-grid` 的行内基线会把按钮撑高 1.7px，
 * 于是「有 swap 的按钮」比同排其它按钮高一圈 —— 抖动换了个地方，并没有消失。
 */
export function swapLabel(shown: string, alternate: string): string {
  return (
    `<span class="swap-label">${esc(shown)}</span>` +
    `<span class="swap-alt" aria-hidden="true">${esc(alternate)}</span>`
  );
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
  /** 给了就整块可点，就地打开概览弹窗（可用模型 / 账户额度）。 */
  opens?: ModalId;
}

/**
 * 指标块：一个小标签 + 一个大数值（+ 可选脚注）。可点时右侧出现箭头。
 *
 * 可点的指标块**打开弹窗**而不是切页：它回答的是「看一眼就知道」的问题，
 * 弹窗关闭后用户还在原来那一屏，不必再点回概览。`aria-haspopup="dialog"` 把这件事
 * 提前告诉读屏，免得他们以为按下去会换页。
 */
export function statTile(spec: StatTileSpec): string {
  const inner = `
    <div class="stat-label">${esc(spec.label)}</div>
    <div class="stat-value">${spec.value}</div>
    ${spec.hint ? `<div class="stat-hint">${esc(spec.hint)}</div>` : ''}`;

  if (!spec.opens) return `<div class="stat">${inner}</div>`;
  return `
    <button class="stat is-link" data-action="open-modal" data-modal="${spec.opens}"
            aria-haspopup="dialog" type="button">
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
