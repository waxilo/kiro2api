// 左侧导航：把五个分区按「运行 / 接入 / 系统」分组，分组标签给出分区之间的关系。
//
// 分组而不是平铺五个入口，是因为这五件事的归属本来就不同：概览是「现在怎么样」，
// 接入两项是「谁在用、怎么接」，系统两项是「这台机器上的应用本身」。
// 平铺会让人以为它们是同一层的东西。
//
// 「可用模型」与「账户额度」**不在这张表里**：它们是概览页两块指标卡点开的弹窗，
// 只读、看一眼就关，不值得为它们各留一个常驻入口。
//
// 结构与状态分离：`renderRail()` 只建一次，`updateRail()` 只切当前项与角标 ——
// 每次渲染都重建导航会让激活项的高亮过渡被重置，看上去像闪一下。
import { esc } from '../../core/format';
import type { AppState, ViewId } from '../../state/app-state';
import { ICON } from '../shared/icons';

interface RailItem {
  id: ViewId;
  label: string;
  icon: string;
}

interface RailGroup {
  label: string;
  items: RailItem[];
}

const GROUPS: RailGroup[] = [
  {
    label: '运行',
    items: [{ id: 'overview', label: '概览', icon: ICON.overview }],
  },
  {
    label: '接入',
    items: [
      { id: 'endpoints', label: '协议接入', icon: ICON.endpoints },
      { id: 'thirdparty', label: '三方接入', icon: ICON.thirdparty },
    ],
  },
  {
    label: '系统',
    items: [
      { id: 'updates', label: '应用更新', icon: ICON.updates },
      { id: 'maintenance', label: '维护', icon: ICON.maintenance },
    ],
  },
];

/**
 * 导航提示点：哪些分区在什么状态下亮、亮什么色（ok = 绿，alert = 红）。
 * 提示点的职责是「那个分区有事看了」，不必进分区也能知道：
 * 代理在跑（概览 = 一切正常的入口）、有可装的更新。
 */
const BADGES: Partial<
  Record<ViewId, { on: (state: AppState) => boolean; tone: 'ok' | 'alert' }>
> = {
  overview: { on: (state) => state.status?.running === true, tone: 'ok' },
  updates: { on: (state) => state.update.available, tone: 'alert' },
};

function renderItem(item: RailItem): string {
  return `
    <button class="rail-item" data-nav="${item.id}" type="button">
      <span class="rail-icon">${item.icon}</span>
      <span class="rail-label">${esc(item.label)}</span>
      <span class="rail-badge" data-badge="${item.id}"></span>
    </button>`;
}

export function renderRail(): string {
  const groups = GROUPS.map(
    (group) => `
      <div class="rail-group">
        <div class="rail-eyebrow">${esc(group.label)}</div>
        ${group.items.map(renderItem).join('')}
      </div>`,
  ).join('');

  return `<nav class="rail-nav" aria-label="分区导航">${groups}</nav>`;
}

/** 切当前项高亮，并按状态点亮导航提示点（见 `BADGES`）。 */
export function updateRail(state: AppState): void {
  for (const group of GROUPS) {
    for (const item of group.items) {
      const node = document.querySelector<HTMLElement>(`.rail-item[data-nav="${item.id}"]`);
      if (!node) continue;
      const active = item.id === state.view;
      node.classList.toggle('is-active', active);
      if (active) node.setAttribute('aria-current', 'page');
      else node.removeAttribute('aria-current');
    }
  }

  for (const [id, badge] of Object.entries(BADGES)) {
    const node = document.querySelector<HTMLElement>(`.rail-badge[data-badge="${id}"]`);
    if (!node) continue;
    node.classList.toggle('is-ok', badge.tone === 'ok');
    node.classList.toggle('is-on', badge.on(state));
  }
}
