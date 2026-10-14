// 图标集：全部内联 SVG，统一 16 视图框 / 1.3px 描边 / 圆头端点 / currentColor 上色。
//
// 为什么内联而不是图标字体或雪碧图：图标总量不到 10 个，内联省一次请求、
// 也不需要在构建期引入图标工具链；描边而非填充意味着浅色一套即可，不必为每个主题各配一份。
// 尺寸与颜色一律由 CSS 决定（`.icon`），这里只描述形状。

const svg = (body: string): string =>
  `<svg class="icon" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" ` +
  `stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${body}</svg>`;

export const ICON = {
  /** 概览：四宫格。 */
  overview: svg(`
    <rect x="2.4" y="2.4" width="4.6" height="4.6" rx="1.3"/>
    <rect x="9" y="2.4" width="4.6" height="4.6" rx="1.3"/>
    <rect x="2.4" y="9" width="4.6" height="4.6" rx="1.3"/>
    <rect x="9" y="9" width="4.6" height="4.6" rx="1.3"/>`),

  /** 双协议接入：双向箭头，一眼看出「两套协议并存」。 */
  endpoints: svg(`
    <path d="M3 5.5h9"/><path d="m9.8 3.2 2.3 2.3-2.3 2.3"/>
    <path d="M13 10.5H4"/><path d="m6.2 8.2-2.3 2.3 2.3 2.3"/>`),

  /** 模型：分层堆叠。 */
  models: svg(`
    <path d="M8 2.4 13.8 5.6 8 8.8 2.2 5.6z"/>
    <path d="m2.2 10.4 5.8 3.2 5.8-3.2"/>`),

  /** 额度：柱状图。 */
  usage: svg(`
    <path d="M3.6 13V9.4" stroke-width="1.5"/>
    <path d="M8 13V4" stroke-width="1.5"/>
    <path d="M12.4 13V7.4" stroke-width="1.5"/>`),

  /** 更新：向下装入托盘。 */
  updates: svg(`
    <path d="M8 2.6v7"/><path d="m5.2 6.8 2.8 2.8 2.8-2.8"/>
    <path d="M3 12.6h10"/>`),

  /** 维护：滑杆。 */
  maintenance: svg(`
    <path d="M2.6 5.2h10.8"/><path d="M2.6 10.8h10.8"/>
    <circle cx="6" cy="5.2" r="1.7"/><circle cx="10" cy="10.8" r="1.7"/>`),

  /** 层级跳转：向右的尖角。 */
  chevron: svg('<path d="m6.4 3.8 4.2 4.2-4.2 4.2"/>'),

  /** 外链 / 打开。 */
  open: svg(`
    <path d="M9.4 2.8h3.8v3.8"/>
    <path d="M13.2 2.8 7.6 8.4"/>
    <path d="M12 9.6v2.8c0 .8-.6 1.4-1.4 1.4H3.8c-.8 0-1.4-.6-1.4-1.4V5.6` +
      `c0-.8.6-1.4 1.4-1.4h2.8"/>`),

  /** 目录。 */
  folder: svg(
    '<path d="M2.4 4.6c0-.8.6-1.4 1.4-1.4h2l1.4 1.6h4.4c.8 0 1.4.6 1.4 1.4v5.2' +
      'c0 .8-.6 1.4-1.4 1.4H3.8c-.8 0-1.4-.6-1.4-1.4z"/>',
  ),

  /** 中性信息（空态等的默认图标）。 */
  info: svg(`
    <circle cx="8" cy="8" r="5.8"/>
    <path d="M8 7.4v3.4"/>
    <circle cx="8" cy="5.2" r=".75" fill="currentColor" stroke="none"/>`),
};
