// 轻提示：不打断操作的反馈（复制成功、启动失败等）。
//
// 只有一个浮层宿主，挂在 body 上（不在 #app 里）：视图每次重绘都是整块换 innerHTML，
// 提示若挂在视图内部就会被连带清掉 —— 「点了复制、提示一闪而过」正是这么来的。

let host: HTMLElement | null = null;
/** 同时最多保留的提示条数，超出时挤掉最早的，避免连点堆成一列。 */
const MAX_VISIBLE = 3;

/**
 * 提示停留多久。
 *
 * 错误要长得多：它现在是「操作失败」的唯一出口（原先那块顶部横幅已经去掉，
 * 横幅会把页面内容整体推下去 —— 那才是真正该避免的）。像
 * 「刷新凭据超时：45 秒内没有等到后端响应，已解锁界面，可以直接重试。」这种，
 * 2.2 秒连读完都来不及，等于没提示。
 */
const LIFE_MS = { info: 2200, error: 6000 } as const;

function ensureHost(): HTMLElement {
  if (!host) {
    host = document.createElement('div');
    host.id = 'toast-host';
    document.body.appendChild(host);
  }
  return host;
}

export function toast(message: string, kind: 'info' | 'error' = 'info'): void {
  const parent = ensureHost();
  while (parent.childElementCount >= MAX_VISIBLE) {
    parent.firstElementChild?.remove();
  }

  const node = document.createElement('div');
  node.className = kind === 'error' ? 'toast err' : 'toast';
  node.textContent = message;
  parent.appendChild(node);

  // 下一帧加 class 触发淡入，否则初始 opacity 与目标值同帧生效、看不到过渡
  requestAnimationFrame(() => node.classList.add('show'));
  setTimeout(() => {
    node.classList.remove('show');
    setTimeout(() => node.remove(), 220);
  }, LIFE_MS[kind]);
}
