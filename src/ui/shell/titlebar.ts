// 自绘标题栏：品牌区 + Windows 上的三个窗口按钮。
//
// 标题栏只建一次（不参与每次状态重绘），唯一的动态部分是版本号，由 `updateTitlebar()`
// 定点更新 —— 这样窗口按钮的事件绑定不会随重绘反复重建。
//
// 「检查更新」不在标题栏：更新是一次性的低频事务，入口收在左侧导航的「应用更新」分区里
// （发现新版本时该分区会亮出角标）。标题栏只留窗口相关的控件，免得常驻一条用不上的按钮。
//
// 平台分工：
// - Windows：`tauri.conf.json` 里去掉了系统装饰，最小化 / 最大化 / 关闭三个按钮由这里自绘。
// - macOS：`tauri.macos.conf.json` 用 `titleBarStyle: Overlay`，红绿灯由系统画在我们这层
//   标题栏之上（左侧用 CSS 留出安全区）。系统按钮带全屏、窗口菜单、Mission Control 等
//   一整套平台手势，重画一遍只会丢掉它们，所以这里不碰。
//
// 职责边界：标题栏管「窗口」——窗口按钮、拖拽区、贴靠命中区上报、页面自检上报。
// 「服务状态」属于状态栏，不在这里重复一份。
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow, type Window as TauriWindow } from '@tauri-apps/api/window';

import { detectPlatform, isTauri } from '../../core/platform';
import type { AppState } from '../../state/app-state';

/**
 * 与 Rust 侧 `window::snap` 的事件名一一对应（那边是 Windows 专属）。
 *
 * 只有悬停这一个方向：命中区一旦被算作非最大小化区，那块区域就收不到鼠标消息，`:hover` 失效，
 * 高亮必须由后端转发。点击不在此列 —— 后端在窗口过程里直接切最大化，不再绕前端一趟。
 */
const EVENT_MAX_BUTTON_HOVER = 'window://max-button-hover';

/** 与 icons-src/app-icon.svg 同几何，内联以免多一次请求。 */
const LOGO_SVG = `
<svg class="titlebar-logo" viewBox="0 0 1024 1024" aria-hidden="true">
  <defs>
    <linearGradient id="titlebar-logo-tile" x1="0" y1="0" x2="0.35" y2="1">
      <stop offset="0" stop-color="#26364e"/><stop offset="1" stop-color="#090d15"/>
    </linearGradient>
    <linearGradient id="titlebar-logo-a" x1="0" y1="1" x2="1" y2="0">
      <stop offset="0" stop-color="#0ea5a0"/><stop offset="1" stop-color="#4ade80"/>
    </linearGradient>
    <linearGradient id="titlebar-logo-b" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0" stop-color="#6366f1"/><stop offset="1" stop-color="#c084fc"/>
    </linearGradient>
  </defs>
  <rect width="1024" height="1024" rx="228" fill="url(#titlebar-logo-tile)"/>
  <g stroke-linecap="round" fill="none" stroke-width="112">
    <line x1="312" y1="266" x2="312" y2="758" stroke="#f1f5f9"/>
    <line x1="372" y1="504" x2="712" y2="266" stroke="url(#titlebar-logo-a)"/>
    <line x1="372" y1="520" x2="712" y2="758" stroke="url(#titlebar-logo-b)"/>
  </g>
</svg>`;

/* 图标沿用 Windows 11 的字形语言：16 视图框、1.1px 描边、平头端点、currentColor 上色。
   描边而非填充，浅色/深色主题下都不用另配一套颜色。 */
const ICON_MINIMIZE = `
<svg viewBox="0 0 16 16" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="1.1">
  <line x1="3.4" y1="8" x2="12.6" y2="8"/>
</svg>`;

const ICON_MAXIMIZE = `
<svg viewBox="0 0 16 16" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="1.1">
  <rect x="3.4" y="3.4" width="9.2" height="9.2" rx="1.6"/>
</svg>`;

const ICON_RESTORE = `
<svg viewBox="0 0 16 16" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="1.1">
  <rect x="2.4" y="2.4" width="8.2" height="8.2" rx="1.5"/>
  <rect x="5.4" y="5.4" width="8.2" height="8.2" rx="1.5"/>
</svg>`;

const ICON_CLOSE = `
<svg viewBox="0 0 16 16" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="1.1">
  <line x1="3.6" y1="3.6" x2="12.4" y2="12.4"/>
  <line x1="12.4" y1="3.6" x2="3.6" y2="12.4"/>
</svg>`;

/** 自绘窗口按钮只在「Windows + Tauri 运行时」出现（macOS 用系统红绿灯）。 */
export function usesCustomWindowControls(): boolean {
  return detectPlatform() === 'windows' && isTauri();
}

export function renderTitlebar(): string {
  const controls = usesCustomWindowControls()
    ? `
      <div class="window-controls" id="window-controls">
        <button class="wc-btn" id="wc-min" type="button" title="最小化" aria-label="最小化窗口">
          ${ICON_MINIMIZE}
        </button>
        <button class="wc-btn" id="wc-max" type="button" title="最大化" aria-label="最大化窗口">
          <span class="wc-glyph wc-glyph-max">${ICON_MAXIMIZE}</span>
          <span class="wc-glyph wc-glyph-restore">${ICON_RESTORE}</span>
        </button>
        <button class="wc-btn wc-btn-close" id="wc-close" type="button" title="关闭"
                aria-label="关闭窗口">
          ${ICON_CLOSE}
        </button>
      </div>`
    : '';

  return `
    <div class="titlebar-brand">
      ${LOGO_SVG}
      <span class="titlebar-name">kiro2api</span>
      <span class="titlebar-version" id="titlebar-version"></span>
    </div>
    <div class="titlebar-spacer"></div>
    <div class="titlebar-actions">
      ${controls}
    </div>
  `;
}

/** 窗口实例只取一次；`onResized` 是窗口级监听，重复挂会越积越多。 */
let cachedWindow: TauriWindow | null = null;
let resizeListenerAttached = false;

function currentWindow(): TauriWindow {
  if (!cachedWindow) cachedWindow = getCurrentWindow();
  return cachedWindow;
}

/** 按当前最大化状态切换「最大化 / 还原」字形与提示文案。 */
async function syncMaximizeState(button: HTMLElement): Promise<void> {
  try {
    const maximized = await currentWindow().isMaximized();
    button.classList.toggle('is-maximized', maximized);
    const label = maximized ? '还原' : '最大化';
    button.title = label;
    button.setAttribute('aria-label', `${label}窗口`);
  } catch {
    // 窗口正在关闭时读状态会失败，不影响其它按钮
  }
}

/**
 * 把最大化按钮的矩形报给后端，供 Win11 贴靠布局的命中判定使用。
 *
 * 报的是 CSS 像素：DPI 换算由后端在每次命中时现算 —— 窗口被拖到另一块不同缩放的显示器上，
 * 上报时就算成物理像素的话坐标会全部失准。
 */
function reportSnapRect(button: HTMLElement): void {
  const rect = button.getBoundingClientRect();
  void invoke('k2a_snap_rect', {
    x: rect.left,
    y: rect.top,
    width: rect.width,
    height: rect.height,
  }).catch(() => undefined);
}

/** 绑定窗口按钮。标题栏只建一次，所以这里也只需绑定一次。 */
export function bindTitlebar(): void {
  if (!usesCustomWindowControls()) return;

  const minimize = document.getElementById('wc-min');
  const maximize = document.getElementById('wc-max');
  const close = document.getElementById('wc-close');
  if (!minimize || !maximize || !close) return;

  const appWindow = currentWindow();

  minimize.addEventListener('click', () => {
    void appWindow.minimize();
  });
  maximize.addEventListener('click', () => {
    void appWindow.toggleMaximize();
  });
  // 关闭必须走 close()：它发出的 CloseRequested 与点系统关闭按钮是同一条路径，交由 Rust 侧
  // 统一处理成「隐藏到托盘」。换成 destroy() 会绕过常驻逻辑，直接把窗口销毁掉。
  close.addEventListener('click', () => {
    void appWindow.close();
  });

  // 最大化 / 还原 / 拖边缩放都会触发 resize；读状态比猜事件来源可靠。两件事每次都要重做：
  // 按钮横坐标依赖窗口宽度，状态则可能被外部改掉。
  //
  // 但两件事**不能串成一条 `await` 链**：
  // * 命中区矩形是纯布局信息，此刻就能从 DOM 量出来，越早报越好（Win11 贴靠浮出全靠它）；
  // * 最大化状态要走一次 IPC 往返，慢，而且实测在启动阶段可能长时间不返回。
  // 之前写成 `await syncMaximizeState(...)` 之后再 `reportSnapRect(...)`，于是「状态读得慢」
  // 会连带让命中区一直没有矩形 —— 鼠标停在最大化按钮上毫无反应，看上去像贴靠整个坏了。
  // 分开之后，任何一边出问题都不会拖住另一边。
  const sync = (): void => {
    reportSnapRect(maximize);
    void syncMaximizeState(maximize);
  };
  sync();

  if (!resizeListenerAttached) {
    resizeListenerAttached = true;
    void appWindow.onResized(() => {
      // 元素重新取一次：万一标题栏被重建过，闭包里那个引用就失效了。
      const button = document.getElementById('wc-max');
      if (!button) return;
      // 顺序同上：布局信息先报，状态后读
      reportSnapRect(button);
      void syncMaximizeState(button);
    });
  }

  // 贴靠命中区一旦算作非客户区，网页就收不到那块区域的鼠标消息，CSS :hover 会失效，
  // 悬停高亮只能由后端转发过来。（点击不走这里：后端在窗口过程里自己切最大化。）
  void listen<boolean>(EVENT_MAX_BUTTON_HOVER, (event) => {
    document.getElementById('wc-max')?.classList.toggle('is-hover', event.payload);
  });

  // 从按钮移到标题栏其它位置时，后端不会再收到那条非客户区消息，残留高亮在这里兜住。
  document.addEventListener('mousemove', () => maximize.classList.remove('is-hover'));
  window.addEventListener('mouseleave', () => maximize.classList.remove('is-hover'));
}

/**
 * 页面自检上报：把「外壳装到哪一步」交给后端记一笔（命令 `k2a_diag`）。
 *
 * 为什么由页面**主动**报，而不是后端 `window.eval` 进来问：在这套 WebView2 环境里实测
 * `eval` 会成功返回、脚本却到不了页面 —— `ExecuteScript` 那一步不报错，命令就是没被调用，
 * 后端一行日志都等不到，整条路是死的。页面自己调 `invoke` 走的是它平时就在用的链路。
 *
 * 代价是「页面压根没起来时不会有报告」，但那种情况下「没有报告」本身就是信号：和
 * `k2a_snap_rect` 的缺失一起看，足以区分「白屏 / 脚本没跑」与「窗口过程那边没命中」。
 */
export function reportShellReady(): void {
  void invoke('k2a_diag', {
    report: {
      href: location.href,
      readyState: document.readyState,
      htmlLength: document.documentElement.outerHTML.length,
      appLength: (document.getElementById('app')?.innerHTML ?? '').length,
      titlebar: !!document.querySelector('.titlebar'),
      maxButton: !!document.getElementById('wc-max'),
      scripts: Array.from(document.querySelectorAll('script'))
        .map((script) => script.src || '(inline)')
        .join(' | '),
    },
  }).catch(() => undefined);
}

/**
 * 定点更新标题栏里的动态内容。
 *
 * 只剩版本号一项：更新入口已移到左侧导航的「应用更新」分区，标题栏不再承载业务动作。
 */
export function updateTitlebar(state: AppState): void {
  const version = document.getElementById('titlebar-version');
  if (!version) return;
  version.textContent = state.update.currentVersion ? `v${state.update.currentVersion}` : '';
}
