// 自绘英文输入框：渲染 + 行为。
//
// 为什么不是一个 `<input>`：**候选词框是输入法在系统层弹的**。只要焦点落在可编辑元素上，
// web 层就没有开关能关掉它 —— `ime-mode` 是早已废弃的 CSS 属性（Chrome / Edge / Safari
// 从未实现过），`inputmode` 只管触屏上的虚拟键盘（`inputmode="latin"` 在现行 HTML 标准里
// 根本不存在），`autocomplete` / `spellcheck` / `autocorrect` / `autocapitalize` 也都管不到
// 输入法。换成**不可编辑**的元素之后，Chromium / WebKit 压根不给它建输入法上下文：选着中文
// 输入法敲字母也是不出候选、直接进框，Windows 与 macOS 一套机制（游戏里用键盘的 canvas 就是
// 这么做的）。这个做法照搬自 market-monitor 桌面端的 `components/EnglishField.tsx`。
//
// 代价是输入行为全部自绘：光标、选区、删改、方向键、Ctrl+A/C/X/V、双击全选。字符集这一条与
// market-monitor 不同：那边只收 `[A-Za-z0-9_]`（交易对名的字母表），这边的模型名是
// `claude-opus-4.6` 这种形状，**点和连字符都要留**，见 `core/format.ts` 的 `englishOnly`。
//
// 与 market-monitor 的另一个差别：那边是 React 受控组件，选区存在组件 state 里；这边视图是
// 「状态进、HTML 出」的纯函数、外壳每次重绘都整块换 innerHTML，所以**选区缓存在模块级 Map 里**
// （键是框的 id），渲染时按当前值夹一遍 —— 重绘之后光标才落在原处。
//
// 值的归属说清楚：**框里不留值**。值只有一个来源（store 的 `modelFilter`）、一个出口
// （`onValueChange` → `state/actions.ts` 的 `setModelFilter`）。这里只保证「能落进去的东西
// 只可能是英文」：单个按键先过 `englishOnly`，粘贴进来的整段原样交出去 —— 裁掉哪些、要不要
// 提示，由上层一把尺子量到底。
import { englishOnly, esc } from '../../core/format';

/** 一个自绘框的外观参数。值由调用方给（视图从 store 里读）。 */
export interface EnglishFieldSpec {
  /** 稳定的 id：外壳靠它把焦点放回重绘后的新框上（见 `ui/shell/shell.ts` 的焦点快照）。 */
  id: string;
  value: string;
  placeholder: string;
  /** 造型类（如 `filter-input`）：皮肤由样式表给，行为都在 `.ef-box` 里。 */
  className?: string;
  /** 无障碍标签；不给就用 placeholder。 */
  label?: string;
}

/** 三个文字片段：光标之前 / 选中的 / 光标之后的。 */
const PARTS = ['pre', 'sel', 'post'] as const;

interface Selection {
  /** 锚点：拉选区时不动的那一头。 */
  a: number;
  /** 活动端：光标画在这一头。 */
  c: number;
}

/**
 * 选区缓存，键是框的 id。
 *
 * 视图是纯函数、外壳每次重绘都整块换 innerHTML，选区没地方留在 DOM 里；存在这里之后，
 * 渲染时按当前值夹一遍就行。（market-monitor 那边是 React 受控组件，同一份东西放在组件 state。）
 */
const selections = new Map<string, Selection>();

/** 取选区并按 `length` 夹好（值可能刚被裁短），顺带把夹过的结果写回。 */
function clampSelection(id: string, length: number): Selection {
  const raw = selections.get(id) ?? { a: length, c: length };
  const next: Selection = {
    a: Math.max(0, Math.min(raw.a, length)),
    c: Math.max(0, Math.min(raw.c, length)),
  };
  selections.set(id, next);
  return next;
}

/** 框里当前显示的正文 —— 就是渲染进去的那份值（DOM 是这一刻的唯一真相）。 */
function currentValue(box: HTMLElement): string {
  return PARTS.map((part) => box.querySelector(`[data-part="${part}"]`)?.textContent ?? '').join('');
}

/** 框的 HTML：三个文字片段 + 光标 + 占位 + 粘贴用的 sink。 */
export function renderEnglishField(spec: EnglishFieldSpec): string {
  const sel = clampSelection(spec.id, spec.value.length);
  const lo = Math.min(sel.a, sel.c);
  const hi = Math.max(sel.a, sel.c);
  // 光标要不要露面，取决于这一刻焦点在不在框上。渲染发生在外壳换 innerHTML **之前**，
  // 所以读到的正是那个马上要被换掉的旧框 —— 答案刚好是我们要的那个。
  const focused = document.activeElement?.id === spec.id;
  const caret = '<span class="ef-caret"></span>';
  const parts =
    `<span data-part="pre">${esc(spec.value.slice(0, lo))}</span>` +
    // 光标画在「活动端」：`c` 落在选区后半段就画在选区之后
    (sel.c >= hi ? '' : caret) +
    `<span data-part="sel"${hi > lo ? ' class="ef-sel"' : ''}>${esc(spec.value.slice(lo, hi))}</span>` +
    (sel.c >= hi ? caret : '') +
    `<span data-part="post">${esc(spec.value.slice(hi))}</span>`;
  const placeholder = spec.value === '' ? `<span class="ef-ph">${esc(spec.placeholder)}</span>` : '';
  const extra = spec.className ? ` ${spec.className}` : '';
  return (
    `<div class="ef-box${extra}" id="${esc(spec.id)}" role="textbox" aria-multiline="false"` +
    ` aria-label="${esc(spec.label ?? spec.placeholder)}" tabindex="0"` +
    ` data-focus="${focused ? '1' : '0'}">${parts}${placeholder}` +
    // 粘贴走位用的 sink：不露面、不进 Tab 序，只借它接一次**原生**粘贴
    // （`navigator.clipboard.readText` 要权限，WebView 里不一定给）。
    // `data-refocus` 兜的是「粘贴期间恰好发生重绘」：外壳的焦点快照照它把焦点还给框。
    `<input class="ef-sink" type="text" tabindex="-1" aria-hidden="true"` +
    ` data-refocus="${esc(spec.id)}" autocomplete="off" spellcheck="false" />` +
    `</div>`
  );
}

/** 把选区画到**已经存在**的框上（不碰值）。 */
function paint(box: HTMLElement, sel: Selection): void {
  const value = currentValue(box);
  const lo = Math.min(sel.a, sel.c);
  const hi = Math.max(sel.a, sel.c);
  const pre = box.querySelector<HTMLElement>('[data-part="pre"]');
  const middle = box.querySelector<HTMLElement>('[data-part="sel"]');
  const post = box.querySelector<HTMLElement>('[data-part="post"]');
  const caret = box.querySelector<HTMLElement>('.ef-caret');
  if (!pre || !middle || !post || !caret) return;

  pre.textContent = value.slice(0, lo);
  middle.textContent = value.slice(lo, hi);
  middle.classList.toggle('ef-sel', hi > lo);
  post.textContent = value.slice(hi);
  if (sel.c >= hi) middle.after(caret);
  else middle.before(caret);
  box.querySelector('.ef-ph')?.toggleAttribute('hidden', value !== '');

  // 光标挪了位置就重播一次闪烁：刚移完先亮着，跟手（原生 input 也是这脾气）。
  // 同一位置反复画（拖选时的 mousemove）就不动它，免得每帧都去戳一次样式。
  const mark = `${lo}:${hi}:${sel.c}`;
  if (box.dataset.caretAt !== mark) {
    box.dataset.caretAt = mark;
    caret.style.animation = 'none';
    void caret.offsetWidth; // 强制一次样式重算：同一帧里「删了又加」不会重启动画
    caret.style.animation = '';
  }
}

/** 文字比框长时把框的横向滚动带到光标处（照原生 input 的老样子）。 */
function revealCaret(box: HTMLElement): void {
  const caret = box.querySelector<HTMLElement>('.ef-caret');
  if (!caret || box.dataset.focus !== '1') return;
  const at = caret.offsetLeft;
  if (at + caret.offsetWidth > box.scrollLeft + box.clientWidth) {
    box.scrollLeft = at + caret.offsetWidth - box.clientWidth + 2;
  } else if (at < box.scrollLeft) {
    box.scrollLeft = Math.max(0, at - 2);
  }
}

/** 按 id 把焦点给回（重绘之后旧节点已经不在文档里了，所以每次都现查）。 */
function focusField(id: string): void {
  document.getElementById(id)?.focus({ preventScroll: true });
}

/**
 * 窗口坐标 → 字符下标。
 *
 * `caretRangeFromPoint` 给的是**文字节点**里的偏移，按 pre/sel/post 三段折算出全文下标；
 * 点到文字之外（右侧留白 / 垂直偏出去）时它给不出片段，就按左右半区兜底。
 */
function indexAt(box: HTMLElement, x: number, y: number): number {
  const value = currentValue(box);
  if (value === '') return 0;
  const probe = (
    document as Document & { caretRangeFromPoint?: (x: number, y: number) => Range | null }
  ).caretRangeFromPoint;
  const range = probe ? probe.call(document, x, y) : null;
  if (range) {
    const node = range.startContainer;
    const element = node.nodeType === Node.TEXT_NODE ? node.parentElement : (node as Element);
    const part = element?.closest<HTMLElement>('[data-part]');
    if (part && box.contains(part)) {
      let base = 0;
      for (const name of PARTS) {
        if (part.dataset.part === name) {
          return Math.max(0, Math.min(base + range.startOffset, value.length));
        }
        base += box.querySelector(`[data-part="${name}"]`)?.textContent?.length ?? 0;
      }
    }
  }
  const rect = box.getBoundingClientRect();
  return x < rect.left + rect.width / 2 ? 0 : value.length;
}

/**
 * 接上容器里所有 `.ef-box` 的行为。
 *
 * 走事件委托：外壳每次重绘都整块换 innerHTML，逐个绑监听意味着每渲染一次就重绑一遍，
 * 漏一处就是「框没反应」（和 `data-action` 那条理由一样）。
 *
 * `onValueChange` 收到的是一份新值，过滤与落库都归它 —— 见文件头「值的归属」。
 */
export function bindEnglishFields(
  root: HTMLElement,
  onValueChange: (id: string, value: string) => void,
): void {
  /** 事件源 → 它所在的自绘框（sink 是框的子节点，也能这样找回来）。 */
  const boxOf = (target: EventTarget | null): HTMLElement | null =>
    target instanceof Element ? target.closest<HTMLElement>('.ef-box') : null;
  const isSink = (target: EventTarget | null): boolean =>
    target instanceof Element && target.classList.contains('ef-sink');

  /** 值交出去之后的收尾：按框里**真正**的值夹一次光标，并把选区画上去。 */
  const settle = (id: string): void => {
    const box = document.getElementById(id);
    if (!box) return;
    paint(box, clampSelection(id, currentValue(box).length));
  };

  /**
   * 提交一次「值」的变化：先把光标定在新位置，再把新值交上去（上层负责过滤与重绘）。
   * 重绘是同步发生的（`store.set` → 外壳重画），所以收尾读到的已经是新值 ——
   * 粘贴里混了中文被裁短时，光标正是靠这一步夹回合法位置。
   */
  const commit = (id: string, next: string, caret: number): void => {
    selections.set(id, { a: caret, c: caret });
    onValueChange(id, next);
    settle(id);
  };

  /** 只动选区（不碰值）：store 收不到变化、外壳不会重绘，所以得当场把选区画上去。 */
  const select = (box: HTMLElement, next: Selection): void => {
    selections.set(box.id, next);
    paint(box, next);
  };

  /** 鼠标拖选的框（mousedown 起、mouseup 止），只可能有一个。 */
  let dragged = '';
  /** 正在走粘贴流程的框：这段时间焦点借给 sink，Ctrl+V 那一下不归键盘处理管。 */
  let pasteFor = '';
  let pasteTimer = 0;

  /** 复制：先走异步剪贴板；被拒（权限 / WebView 限制）就退回「sink + execCommand」的老办法。 */
  const copyText = (text: string, id: string): void => {
    const fallback = (): void => {
      // 按 id 现查：异步那一步落地时，这个框可能已经被重绘换掉了
      const sink = document.getElementById(id)?.querySelector<HTMLInputElement>('.ef-sink');
      if (!sink) return;
      sink.value = text;
      sink.focus();
      sink.select();
      try {
        document.execCommand('copy');
      } catch {
        /* 剪贴板用不了就作罢 */
      }
      sink.value = '';
      focusField(id);
    };
    if (navigator.clipboard?.writeText) void navigator.clipboard.writeText(text).catch(fallback);
    else fallback();
  };

  /** 粘贴落盘：插到选区处，然后把焦点还给框。 */
  const finishPaste = (id: string, raw: string): void => {
    pasteFor = '';
    window.clearTimeout(pasteTimer);
    const box = document.getElementById(id);
    if (!box) return;
    const value = currentValue(box);
    const sel = clampSelection(id, value.length);
    const lo = Math.min(sel.a, sel.c);
    const hi = Math.max(sel.a, sel.c);
    // 整段**原样**交出去（不在这里过滤）：裁掉什么、要不要提示，是上层一把尺子的事
    commit(id, value.slice(0, lo) + raw + value.slice(hi), lo + raw.length);
    // 重绘之后是个新框，sink 也换了一个 —— 清掉它，别让残留的字被下一次粘贴读回去
    const sink = document.getElementById(id)?.querySelector<HTMLInputElement>('.ef-sink');
    if (sink) sink.value = '';
    focusField(id);
  };

  // 光标只在框有焦点时露面。`focus` / `blur` 不冒泡，所以用会冒泡的 focusin / focusout。
  root.addEventListener('focusin', (event) => {
    const box = boxOf(event.target);
    if (!box) return;
    box.dataset.focus = '1';
    revealCaret(box);
  });
  root.addEventListener('focusout', (event) => {
    const box = boxOf(event.target);
    if (box) box.dataset.focus = '0';
  });

  // 窗口失焦时元素还留着 `:focus`（Chromium 如此），光标不会自己收 —— 得手动收一下，
  // 否则应用退到后台了，那个光标还在别人窗口上面闪。
  const allBoxes = (): HTMLElement[] => Array.from(root.querySelectorAll<HTMLElement>('.ef-box'));
  window.addEventListener('blur', () => {
    for (const box of allBoxes()) box.dataset.focus = '0';
  });
  window.addEventListener('focus', () => {
    for (const box of allBoxes()) {
      if (document.activeElement === box) box.dataset.focus = '1';
    }
  });

  root.addEventListener('mousedown', (event) => {
    const box = boxOf(event.target);
    if (!box || event.button !== 0 || isSink(event.target)) return;
    event.preventDefault(); // 原生选择关掉（样式里还有 user-select: none 兜底），焦点手动给
    box.focus({ preventScroll: true });
    const at = indexAt(box, event.clientX, event.clientY);
    const current = clampSelection(box.id, currentValue(box).length);
    // 按住 shift 是拉选区（锚点不动），否则就是把光标挪过去
    select(box, event.shiftKey ? { a: current.a, c: at } : { a: at, c: at });
    dragged = box.id;
  });

  root.addEventListener('dblclick', (event) => {
    const box = boxOf(event.target);
    if (!box) return;
    event.preventDefault();
    const value = currentValue(box);
    select(box, { a: 0, c: value.length }); // 双击 = 整词（模型名就是一个词）
  });

  // 拖选要跟着鼠标走，但鼠标可能离开框（甚至离开窗口）—— 所以监听挂在 window 上，只挂一次。
  window.addEventListener('mousemove', (event) => {
    if (!dragged) return;
    const box = document.getElementById(dragged);
    if (!box) return;
    const anchor = selections.get(dragged)?.a ?? 0;
    select(box, { a: anchor, c: indexAt(box, event.clientX, event.clientY) });
  });
  window.addEventListener('mouseup', () => {
    dragged = '';
  });

  root.addEventListener('keydown', (event) => {
    const box = boxOf(event.target);
    // 粘贴走位期间焦点在 sink 上：那一下的按键不归这里管
    if (!box || isSink(event.target)) return;
    // 真被输入法组合拿去了（理论上不会 —— 这框不可编辑）：这轮别掺和
    if (event.isComposing || event.key === 'Process') return;

    const id = box.id;
    const value = currentValue(box);
    const sel = clampSelection(id, value.length);
    const lo = Math.min(sel.a, sel.c);
    const hi = Math.max(sel.a, sel.c);
    const len = value.length;
    const key = event.key;

    /** 值变了：挡掉默认行为（这框不是编辑元素，默认行为本来也没有），再交上去。 */
    const write = (next: string, caret: number): void => {
      event.preventDefault();
      commit(id, next, caret);
    };
    /** 只挪光标 / 拉选区。 */
    const restyle = (next: Selection): void => {
      event.preventDefault();
      select(box, next);
    };
    /** 左右方向键：带 shift = 拉选区；无 shift 且有选区 = 先收拢到对应那一头。 */
    const move = (delta: number, shift: boolean): void => {
      if (shift) {
        restyle({ a: sel.a, c: Math.max(0, Math.min(sel.c + delta, len)) });
        return;
      }
      const at = lo !== hi ? (delta < 0 ? lo : hi) : Math.max(0, Math.min(sel.c + delta, len));
      restyle({ a: at, c: at });
    };
    /** Home / End（单行框里 ↑↓ 也是它俩）：带 shift = 拉到这一头。 */
    const jump = (to: number, shift: boolean): void => {
      restyle(shift ? { a: sel.a, c: to } : { a: to, c: to });
    };

    if (event.ctrlKey || event.metaKey) {
      switch (key.toLowerCase()) {
        case 'a':
          restyle({ a: 0, c: len });
          return;
        case 'c': {
          const picked = value.slice(lo, hi);
          if (picked) {
            event.preventDefault();
            copyText(picked, id);
          }
          return;
        }
        case 'x': {
          const picked = value.slice(lo, hi);
          if (picked) {
            event.preventDefault();
            copyText(picked, id);
            write(value.slice(0, lo) + value.slice(hi), lo);
          }
          return;
        }
        case 'v':
          // 这里**不能** preventDefault：粘贴这下的默认行为要留给浏览器，
          // 我们只把焦点先让给 sink —— 原生粘贴落在「当前焦点」上，正好是它。
          pasteFor = id;
          {
            const sink = box.querySelector<HTMLInputElement>('.ef-sink');
            if (sink) {
              sink.value = '';
              sink.focus();
            }
          }
          window.clearTimeout(pasteTimer);
          pasteTimer = window.setTimeout(() => {
            // 剪贴板里没文本 / 没贴进来：把焦点收回来，别把框晾着
            if (pasteFor === id) {
              pasteFor = '';
              focusField(id);
            }
          }, 400);
          return;
        default:
          return; // 其余组合键（Ctrl+K 之类）不认，任其上行
      }
    }

    switch (key) {
      case 'ArrowLeft':
        move(-1, event.shiftKey);
        return;
      case 'ArrowRight':
        move(1, event.shiftKey);
        return;
      case 'ArrowUp':
      case 'Home':
        jump(0, event.shiftKey);
        return;
      case 'ArrowDown':
      case 'End':
        jump(len, event.shiftKey);
        return;
      case 'Backspace':
        if (lo !== hi) write(value.slice(0, lo) + value.slice(hi), lo);
        else if (sel.c > 0) write(value.slice(0, sel.c - 1) + value.slice(sel.c), sel.c - 1);
        return;
      case 'Delete':
        if (lo !== hi) write(value.slice(0, lo) + value.slice(hi), lo);
        else if (sel.c < len) write(value.slice(0, sel.c) + value.slice(sel.c + 1), sel.c);
        return;
      default:
        break;
    }

    // 可打印字符：与滤词同一把尺子 —— 中文、全角、空格、标点到这里就被拒，一格都进不去。
    // 也正因为在这里就被拒了，输入法上屏的那条路根本不存在（没有可编辑元素 ⇒ 没有组合态）。
    if (key.length === 1 && !event.altKey) {
      event.preventDefault();
      const ch = englishOnly(key);
      if (ch) write(value.slice(0, lo) + ch + value.slice(hi), lo + ch.length);
    }
  });

  // sink 接到原生粘贴：正文直接从事件里拿（不需要读剪贴板的权限）
  root.addEventListener('paste', (event) => {
    if (!isSink(event.target) || !pasteFor) return;
    event.preventDefault(); // sink 不留字：正文只进框的模型
    const data = (event as ClipboardEvent).clipboardData;
    const raw = data?.getData('text/plain') || data?.getData('text') || '';
    finishPaste(pasteFor, raw);
  });

  // 没走 paste 事件（某些引擎只把值回灌给 sink）时兜底：从 sink 的值里拿
  root.addEventListener('input', (event) => {
    if (!isSink(event.target) || !pasteFor) return;
    finishPaste(pasteFor, (event.target as HTMLInputElement).value ?? '');
  });
}
