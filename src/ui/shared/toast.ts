// 轻提示：不打断操作的反馈（复制成功、启动失败等）。
//
// 只有一个浮层宿主，挂在 body 上（不在 #app 里）：视图每次重绘都是整块换 innerHTML，
// 提示若挂在视图内部就会被连带清掉 —— 「点了复制、提示一闪而过」正是这么来的。

let host: HTMLElement | null = null;
/** 同时最多保留的提示条数，超出时挤掉最早的，避免连点堆成一列。 */
const MAX_VISIBLE = 3;

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
  }, 2200);
}
