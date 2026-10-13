// 底部状态栏：常驻的遥测条 —— 服务状态、地址、凭据有效期。
//
// 为什么值得单开一层：这些值在任何分区下都要能看见（「代理到底跑没跑」是最常被问的问题），
// 但不能每次都塞进内容区 —— 那会让人从任意分区切回概览去确认。
// 放在窗口底部，它就成了一条不随分区变化的基线。
//
// 这里刻意**不**显示「当前模型」：本应用是代理服务，同一时刻不同客户端可能各用各的模型，
// 状态栏放一个模型名只会让人误以为那是全局设置，而这个全局设置并不存在。
//
// 同样地，只做定点文本更新，不参与整块重绘：状态点颜色与数字每秒都可能变，
// 重建 DOM 会让文字选中状态和过渡动画一起丢掉。
import { serviceAddress, summarizeStatus, type AppState } from '../../state/app-state';

export function renderStatusbar(): string {
  return `
    <div class="sb-slot">
      <span class="sb-dot" id="sb-dot"></span>
      <span class="sb-strong" id="sb-state"></span>
    </div>
    <div class="sb-slot sb-address">
      <code id="sb-address"></code>
    </div>
    <div class="sb-slot sb-busy" id="sb-busy">
      <span class="sb-spinner"></span>
      <span id="sb-busy-text"></span>
    </div>
    <div class="sb-spacer"></div>
    <div class="sb-slot">
      <span class="sb-key">凭据</span>
      <span id="sb-credential"></span>
    </div>
  `;
}

export function updateStatusbar(state: AppState): void {
  const summary = summarizeStatus(state);

  const dot = document.getElementById('sb-dot');
  if (dot) dot.className = `sb-dot ${summary.tone}`;

  const label = document.getElementById('sb-state');
  if (label) label.textContent = summary.label;

  const address = document.getElementById('sb-address');
  if (address) {
    const value = serviceAddress(state);
    address.textContent = value ?? '未运行';
    address.classList.toggle('is-dim', value === null);
  }

  const credential = document.getElementById('sb-credential');
  if (credential) {
    const status = state.status;
    if (!status) {
      credential.textContent = '读取中';
      credential.className = 'is-dim';
    } else if (!status.available) {
      // 注意：这一层全部写 textContent，因此**不能**用 `core/format` 的 `text()`
      // ——那个函数做的是 HTML 转义，喂给 textContent 会把 `&` 显示成 `&amp;`。
      credential.textContent = status.authSource || '未找到';
      credential.className = 'is-warn';
    } else {
      credential.textContent = status.expiresAt || '有效';
      credential.className = '';
    }
  }

  // 全局进行态：任何动作在跑，这里都显示它在干什么。
  // 只让按钮变灰的话，用户看到的是「点了没反应」——这一条把「它在忙」摆到明面上。
  const busy = document.getElementById('sb-busy');
  if (busy) {
    const label = state.busyLabel;
    busy.classList.toggle('is-on', Boolean(label));
    const text = document.getElementById('sb-busy-text');
    if (text) text.textContent = label ? `${label}…` : '';
  }
}
