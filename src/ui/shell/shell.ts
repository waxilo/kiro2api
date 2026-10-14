// 外壳组装与唯一的渲染循环。
//
// 这里是整个界面的「接线盒」，只做四件事：
//   1. 建一次静态外壳（标题栏 / 导航 / 内容区 / 状态栏）—— 窗口按钮与滚动容器的事件绑定
//      不随内容重绘重建；
//   2. 订阅 store，任何状态变化都从这里出发重画；
//   3. 把点击 / 变更事件按委托分发给 `state/actions`；
//   4. 启动时按「先本地后联网」的顺序取数据。
//
// 关键约束：视图层是纯函数（状态进、HTML 出），所以**重画只有一个入口**。
// 旧实现里每个动作都自己调一次 `render()`，改动一处忘一处就会出现「数据变了界面没动」；
// 现在只要 `store.set` 了，界面必然会跟上。
import { applyPlatformClass } from '../../core/platform';
import { updater } from '../../services/updater';
import {
  bindBackendEvents,
  dispatch,
  navigate,
  refresh,
  saveAutoStart,
  savePort,
  setModelFilter,
  useFeedback,
} from '../../state/actions';
import { store, type AppState, type ViewId } from '../../state/app-state';
import { esc } from '../../core/format';
import { bindEnglishFields } from '../shared/english-field';
import { ScrollArea } from '../shared/scroll-area';
import { toast } from '../shared/toast';
import { VIEWS, type ViewSpec } from '../views';
import { renderRail, updateRail } from './rail';
import { renderStatusbar, updateStatusbar } from './statusbar';
import { bindTitlebar, renderTitlebar, reportShellReady, updateTitlebar } from './titlebar';

let headEl: HTMLElement;
let bodyEl: HTMLElement;
let scrollArea: ScrollArea | null = null;

// ── 组装 ────────────────────────────────────────────────────────────────

export function mountApp(container: HTMLElement): void {
  applyPlatformClass();

  // `data-tauri-drag-region="deep"` 会跳过按钮 / 输入框等可点击元素，
  // 所以标题栏里的按钮（更新入口与三个窗口按钮）照常可点，空白处抬手就能拖窗口。
  container.innerHTML = `
    <div class="titlebar" data-tauri-drag-region="deep">${renderTitlebar()}</div>
    <div class="shell-main">
      <aside class="rail">${renderRail()}</aside>
      <main class="content">
        <header class="view-head" id="view-head"></header>
        <div class="view-host">
          <div class="view-body" id="view-body"></div>
        </div>
      </main>
    </div>
    <div class="statusbar">${renderStatusbar()}</div>
  `;

  const head = document.getElementById('view-head');
  const body = document.getElementById('view-body');
  if (!head || !body) throw new Error('外壳结构不完整：缺少 #view-head 或 #view-body');
  headEl = head;
  bodyEl = body;

  bindTitlebar();
  useFeedback({ toast });
  // 滚动容器在挂载时就固定下来，之后的每次重绘只换它的 innerHTML，
  // 因此自绘滚动条的观察者与指针事件不会被反复重建。
  scrollArea = new ScrollArea(bodyEl);
  bindEvents(container);

  store.subscribe((state, previous) => render(state, previous));
  render(store.get());

  // 顺序有讲究：外壳就绪先记一笔（后面任何一步炸掉，日志里都有这条分界线），
  // 再出本地状态（用户马上要看），最后才去联网检查更新 —— 它最慢，失败了也不该拖住首屏。
  reportShellReady();
  void boot();
}

async function boot(): Promise<void> {
  await refresh();
  await bindBackendEvents();
  await updater.init();
  // 首屏若已有溢出内容，让滚动条自己亮一次，免得用户不知道下面还有东西
  scrollArea?.revealHint();
}

// ── 渲染 ────────────────────────────────────────────────────────────────

function render(state: AppState, previous?: AppState): void {
  // 三处常驻区域各自做定点更新：状态点、当前导航项、版本号这些每次都可能变，
  // 但它们所在的 DOM 结构不该被重建（会打断过渡动画、丢失文字选中）。
  updateTitlebar(state);
  updateRail(state);
  updateStatusbar(state);

  const spec = VIEWS[state.view];
  headEl.innerHTML = renderHead(spec, state);

  // 整块换 innerHTML 会丢焦点：端口输入框正在编辑时被后台事件触发一次重绘，
  // 光标就会跳走。先记下焦点与选区，换完再放回去。
  const focus = captureFocus(bodyEl);
  bodyEl.innerHTML = renderBody(spec, state);
  restoreFocus(bodyEl, focus);

  const switched = !previous || previous.view !== state.view;
  if (switched) {
    bodyEl.scrollTop = 0;
    playEnter(bodyEl);
  }
  scrollArea?.refresh();

  // 放在最后：页面已经是正确的样子了，提示只是叠上去的一层，不参与布局。
  announceError(state, previous);
}

/**
 * 把「新的失败」播成一条浮层提示。
 *
 * 失败**不往页面里插东西**：一块顶部横幅会把内容整体推下去、把按钮挪位，
 * 而「刚才那件事失败了」并不是页面结构变了 —— 用户读到的是「页面抖了一下」。
 *
 * 由 store 的 `error` 字段统一驱动，而不是在每个失败点各调一次 `toast()`：
 * 后者漏一处就是「操作失败了，界面什么都没说」。事实上 `refresh()` 的失败与后端
 * `proxy://token-refresh-failed` 事件原本就只写 store、只靠横幅露面，
 * 收敛到这里之后它们也自动有了提示。
 *
 * `store.set` 只在值真的变了才通知，所以同一个错误连续写两次只会播一条 ——
 * 而 `run()` 在动作开始时会先把 `error` 清空，因此「重试又失败」照常提示。
 */
function announceError(state: AppState, previous?: AppState): void {
  if (!state.error || state.error === previous?.error) return;
  toast(state.error, 'error');
}

function renderHead(spec: ViewSpec, state: AppState): string {
  const actions = spec.head(state);
  return `
    <div class="view-head-text">
      <h1 class="view-title">${esc(spec.title)}</h1>
      <p class="view-desc">${esc(spec.desc)}</p>
    </div>
    ${actions ? `<div class="view-head-actions">${actions}</div>` : ''}`;
}

function renderBody(spec: ViewSpec, state: AppState): string {
  // 这里只放视图自己的内容 —— 失败提示走浮层（见 `announceError`），
  // 页面结构不为「刚才失败了」这件事让位。
  return `<div class="view-stack">${spec.body(state)}</div>`;
}

/**
 * 切分区时让内容依次浮入。
 *
 * 只在「换页」时播一次，而不是每次刷新都播 —— 后台事件每几秒就可能重画一次，
 * 每次都做入场动画会变成持续的闪烁。所以这里必须主动摘掉类名，否则后续替换进来的
 * 子节点会连带重新播一遍。
 */
function playEnter(host: HTMLElement): void {
  host.classList.remove('is-entering');
  // 强制一次样式重算：同一帧内「删了又加」不会重启 CSS 动画
  void host.offsetWidth;
  host.classList.add('is-entering');
  window.setTimeout(() => host.classList.remove('is-entering'), 420);
}

interface FocusSnapshot {
  id: string;
  start: number | null;
  end: number | null;
}

function captureFocus(host: HTMLElement): FocusSnapshot | null {
  const active = document.activeElement;
  if (!(active instanceof HTMLElement) || !host.contains(active)) return null;
  // 焦点落在**没有 id** 的按钮上时，`data-refocus` 指名渲染后该把焦点还给谁。
  // 「清空搜索」就是这种：点它的人下一步一定是接着改那个输入框，
  // 不还焦点的话光标会掉到 body 上，得再点一次输入框才能继续打字。
  const id = active.id || active.dataset.refocus || '';
  if (!id) return null;
  const field = active as HTMLInputElement;
  const hasSelection = typeof field.selectionStart === 'number';
  return {
    id,
    start: hasSelection ? field.selectionStart : null,
    end: hasSelection ? field.selectionEnd : null,
  };
}

function restoreFocus(host: HTMLElement, snapshot: FocusSnapshot | null): void {
  if (!snapshot) return;
  // `CSS.escape` 是因为 id 里可能有 `:` 这类需要转义的字符（本项目里没有，但别赌）
  const next = host.querySelector<HTMLElement>(`#${CSS.escape(snapshot.id)}`);
  if (!next) return;
  next.focus({ preventScroll: true });
  if (snapshot.start !== null && next instanceof HTMLInputElement) {
    // 夹到新长度：搜索框会把非 ASCII 字符裁掉，重绘后的值可能比快照里的光标位置还短
    const end = snapshot.end ?? snapshot.start;
    const length = next.value.length;
    next.setSelectionRange(Math.min(snapshot.start, length), Math.min(end, length));
  }
}

// ── 事件 ────────────────────────────────────────────────────────────────

function bindEvents(container: HTMLElement): void {
  container.addEventListener('click', (event) => {
    const origin = event.target as HTMLElement | null;
    // 先看导航：它与动作按钮可能嵌套在同一块结构里，先匹配更具体的语义
    const nav = origin?.closest<HTMLElement>('[data-nav]');
    if (nav?.dataset.nav) {
      navigate(nav.dataset.nav as ViewId);
      return;
    }
    const action = origin?.closest<HTMLElement>('[data-action]');
    if (action) void dispatch(action.dataset.action ?? '', action.dataset);
  });

  // 用 change 而不是 input：输入端口时不要每敲一个字符就写盘 + 重绘
  container.addEventListener('change', (event) => {
    const target = event.target as HTMLElement;
    switch (target.id) {
      case 'port-input':
        void savePort((target as HTMLInputElement).value);
        return;
      case 'autostart-input':
        void saveAutoStart((target as HTMLInputElement).checked);
        return;
      case 'update-source-input':
        void updater.setSource((target as HTMLSelectElement).value);
        return;
      default:
        return;
    }
  });

  // 搜索框反过来：边打边筛，用 change 得等失焦才生效，那是筛选框最不能忍的延迟。
  //
  // 它是**自绘**的（不是 `<input>`，见 `ui/shared/english-field.ts`）：输入、光标、选区、
  // Ctrl+A/C/X/V、粘贴全归字段模块自己管 —— 外壳只负责把「用户改成了什么值」转给状态层。
  // 顺带也就没有组字态这回事了：不可编辑元素拿不到输入法上下文，压根不会产生候选词。
  bindEnglishFields(container, (_id, next) => setModelFilter(next));

  // Escape 清空搜索。焦点还在框里，重绘后外壳的焦点快照会把光标放回原位。
  container.addEventListener('keydown', (event) => {
    if (event.key !== 'Escape') return;
    const target = event.target as HTMLElement;
    if (target.id === 'model-filter' && store.get().modelFilter) setModelFilter('');
  });

  // 窗口重新可见 / 重新获得焦点时补一次状态同步。
  //
  // WebView2 在窗口最小化 / 被遮挡时会被 Chromium 节流，任务投递（后端命令的响应、
  // 后端推来的事件）都会延后。实测出现过「后端早就把新凭据写好并 emit 了事件，
  // 界面还停在旧的到期时间」，而动作的进行态也就一直不解除。回到可见状态时补一次即可，
  // 代价是四个本地 IPC（不发网络请求）。
  //
  // `focus` 那一条另有一个场景：应用窗口一直开着，用户去窗口外面执行了 `kiro-cli login`，
  // 回到窗口就该看见凭据已经就绪。这原本是概览页那颗「刷新」按钮唯一独占的事 ——
  // 现在由它自己完成，那颗按钮就不再需要了。
  //
  // 唯一例外：光标正停在某个输入控件里时不重画（见 `isEditing`）。
  document.addEventListener('visibilitychange', () => {
    if (document.visibilityState === 'visible') void syncOnRegain();
  });
  window.addEventListener('focus', () => void syncOnRegain());
}

/**
 * 焦点回到本窗口时的那次同步。
 *
 * 用户正把光标放在某个输入控件里（端口、自启动开关、模型搜索框、下载源下拉框）时跳过：
 * 重绘是整块换 innerHTML，没提交的输入会回到上一次的状态 —— 而「读一遍本地状态」这件事
 * 晚一秒没有任何代价，用户正在敲的字却会白敲。
 */
function syncOnRegain(): Promise<void> {
  return isEditing() ? Promise.resolve() : refresh();
}

/** 焦点是不是落在某个输入控件里（原生输入框 / 下拉框 / 自绘搜索框）。 */
function isEditing(): boolean {
  const active = document.activeElement;
  if (!(active instanceof HTMLElement)) return false;
  return (
    active instanceof HTMLInputElement ||
    active instanceof HTMLSelectElement ||
    active.matches('[role="textbox"]')
  );
}
