// 自绘覆盖式滚动条。
//
// 为什么不用 `::-webkit-scrollbar` 换皮肤：那条路只能改外观，滚动条依旧占据布局宽度 ——
// 内容一旦从「不溢出」变成「溢出」，整页会横向抖一下；也做不到闲置淡出。这里把原生滚动条
// 彻底隐藏（宽度置 0，滚轮 / 键盘 / 触控板照常可用），改用绝对定位的轨道 + 滑块自己绘制，
// 完全不参与布局。
//
// 结构（`host` 是定位基准，`view` 是真正的滚动容器，两者是兄弟关系）：
//
//   <div class="view-host">          ← position: relative
//     <div class="view-body">…</div>  ← overflow-y: auto
//     <div class="osb-track"><div class="osb-thumb"></div></div>
//   </div>
//
// 轨道放在 host 而不是 view 里，是为了不被 view 的滚动带走；也正因为它是兄弟节点，
// 滑块样式变化不会触发 view 的 MutationObserver，不会自激。
//
// 类名保留 `osb-` 前缀（overlay scroll bar 的缩写）：它描述的是这套控件的形状，
// 与类名 `ScrollArea` 指的是同一样东西，改名的收益抵不上动一处已验证过的选择器。
//
// 只实现纵向：这个界面里所有滚动都是纵向的，横向滚动条属于没人会走到的分支。

export interface ScrollAreaOptions {
  /** 滑块最短像素长度，避免内容极长时滑块细到抓不住。 */
  minThumbSize?: number;
}

export class ScrollArea {
  private readonly view: HTMLElement;
  private readonly host: HTMLElement;
  private readonly track: HTMLDivElement;
  private readonly thumb: HTMLDivElement;
  private readonly minThumbSize: number;
  private readonly resizeObserver: ResizeObserver;
  private readonly contentObserver: MutationObserver;

  private hideTimer: ReturnType<typeof setTimeout> | null = null;
  private dragging = false;
  private dragOffset = 0;
  private updateScheduled = false;

  constructor(
    view: HTMLElement,
    host: HTMLElement | null = view.parentElement,
    options: ScrollAreaOptions = {},
  ) {
    if (!host) {
      throw new Error('自绘滚动条需要一个定位父元素（position: relative）');
    }
    this.view = view;
    this.host = host;
    this.minThumbSize = options.minThumbSize ?? 32;

    this.track = document.createElement('div');
    this.track.className = 'osb-track is-hidden';
    this.thumb = document.createElement('div');
    this.thumb.className = 'osb-thumb';
    this.track.appendChild(this.thumb);
    this.host.appendChild(this.track);

    this.view.addEventListener('scroll', this.handleScroll, { passive: true });
    this.thumb.addEventListener('pointerdown', this.handleThumbPointerDown);
    this.thumb.addEventListener('pointermove', this.handleThumbPointerMove);
    this.thumb.addEventListener('pointerup', this.handleThumbPointerUp);
    this.thumb.addEventListener('pointercancel', this.handleThumbPointerUp);
    this.track.addEventListener('pointerdown', this.handleTrackPointerDown);
    this.track.addEventListener('pointerenter', this.handleTrackEnter);

    this.resizeObserver = new ResizeObserver(this.scheduleUpdate);
    this.resizeObserver.observe(this.view);
    // view 自身尺寸不变、内容却增减时（整块重绘、更新说明展开）ResizeObserver 是看不见的，
    // 需要另开一路观察子树。
    this.contentObserver = new MutationObserver(this.scheduleUpdate);
    this.contentObserver.observe(this.view, { childList: true, subtree: true });

    this.scheduleUpdate();
  }

  /** 外部主动要求重算（内容变化之外的场景，例如切换分区后立刻重算）。 */
  refresh(): void {
    this.update();
  }

  /** 内容确实溢出时让滚动条亮一次，用于首屏 / 切区提示；不溢出则什么都不做。 */
  revealHint(): void {
    if (this.view.scrollHeight - this.view.clientHeight <= 1) return;
    this.reveal();
  }

  dispose(): void {
    this.resizeObserver.disconnect();
    this.contentObserver.disconnect();
    if (this.hideTimer !== null) clearTimeout(this.hideTimer);
    this.view.removeEventListener('scroll', this.handleScroll);
    this.track.remove();
  }

  // ── 计算与绘制 ────────────────────────────────────────────────────────
  //
  // 合并到同一帧：滚动事件与 MutationObserver 都可能在一帧里连发多次，
  // 每次都同步读写 offsetHeight 会造成反复强制重排。
  private scheduleUpdate = (): void => {
    if (this.updateScheduled) return;
    this.updateScheduled = true;
    requestAnimationFrame(() => {
      this.updateScheduled = false;
      this.update();
    });
  };

  private update = (): void => {
    const { clientHeight, scrollHeight, scrollTop } = this.view;
    const scrollable = scrollHeight - clientHeight;

    if (scrollable <= 1) {
      this.track.classList.add('is-hidden');
      return;
    }
    // 必须先恢复显示再量高度：display: none 的元素 clientHeight 恒为 0。
    this.track.classList.remove('is-hidden');

    const trackHeight = this.track.clientHeight;
    if (trackHeight <= 0) return; // host 尺寸为 0（窗口隐藏/最小化）时不做除法

    const thumbHeight = Math.min(
      trackHeight,
      Math.max(this.minThumbSize, Math.round(trackHeight * (clientHeight / scrollHeight))),
    );
    const maxThumbTop = trackHeight - thumbHeight;
    const top = maxThumbTop === 0 ? 0 : Math.round((scrollTop / scrollable) * maxThumbTop);

    // 用 transform 而不是 top：不触发重排
    this.thumb.style.height = `${thumbHeight}px`;
    this.thumb.style.transform = `translateY(${top}px)`;
  };

  /** 亮起来，并在一段时间无操作后重新淡出（鼠标停在轨道上时由 CSS :hover 兜住）。 */
  private reveal = (): void => {
    this.track.classList.add('is-active');
    if (this.hideTimer !== null) clearTimeout(this.hideTimer);
    this.hideTimer = setTimeout(() => {
      this.hideTimer = null;
      if (!this.dragging) this.track.classList.remove('is-active');
    }, 1200);
  };

  // ── 事件 ──────────────────────────────────────────────────────────────

  private handleScroll = (): void => {
    this.scheduleUpdate();
    this.reveal();
  };

  private handleTrackEnter = (): void => {
    if (this.hideTimer !== null) clearTimeout(this.hideTimer);
    this.hideTimer = null;
  };

  private handleThumbPointerDown = (event: PointerEvent): void => {
    if (event.button !== 0) return;
    // 阻止默认行为：否则按住拖动会选中背后的文字，并触发原生拖拽幽灵图
    event.preventDefault();
    event.stopPropagation();
    this.dragging = true;
    this.dragOffset = event.clientY - this.thumb.getBoundingClientRect().top;
    this.track.classList.add('is-dragging');
    // 指针捕获：拖出轨道（甚至拖出窗口）也能继续跟随
    this.thumb.setPointerCapture(event.pointerId);
  };

  private handleThumbPointerMove = (event: PointerEvent): void => {
    if (!this.dragging) return;
    const geometry = this.geometry();
    if (!geometry) return;
    const desired = event.clientY - geometry.trackTop - this.dragOffset;
    this.view.scrollTop = clamp01(desired / geometry.maxThumbTop) * geometry.scrollable;
  };

  private handleThumbPointerUp = (event: PointerEvent): void => {
    if (!this.dragging) return;
    this.dragging = false;
    this.track.classList.remove('is-dragging');
    if (this.thumb.hasPointerCapture(event.pointerId)) {
      this.thumb.releasePointerCapture(event.pointerId);
    }
    this.reveal();
  };

  /** 点轨道空白处：把滑块中心挪到点击位置，等同于「跳到该处」。 */
  private handleTrackPointerDown = (event: PointerEvent): void => {
    if (event.button !== 0) return;
    event.preventDefault();
    const geometry = this.geometry();
    if (!geometry) return;
    const thumbCenter = event.clientY - geometry.trackTop - geometry.thumbHeight / 2;
    this.view.scrollTop = clamp01(thumbCenter / geometry.maxThumbTop) * geometry.scrollable;
    this.reveal();
  };

  /** 拖拽所需的几何量；当前不可滚动（或尺寸为 0）时返回 null。 */
  private geometry(): {
    trackTop: number;
    thumbHeight: number;
    maxThumbTop: number;
    scrollable: number;
  } | null {
    const scrollable = this.view.scrollHeight - this.view.clientHeight;
    const trackHeight = this.track.clientHeight;
    const thumbHeight = this.thumb.offsetHeight;
    const maxThumbTop = trackHeight - thumbHeight;
    if (scrollable <= 0 || maxThumbTop <= 0) return null;
    return {
      trackTop: this.track.getBoundingClientRect().top,
      thumbHeight,
      maxThumbTop,
      scrollable,
    };
  }
}

function clamp01(value: number): number {
  if (!Number.isFinite(value)) return 0;
  return Math.min(1, Math.max(0, value));
}
