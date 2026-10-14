// 可用模型：一份只读的清单，回答「这个账户能用哪些模型」，并支持搜索与点击复制。
//
// 这里刻意**没有**模型选择器。本应用是代理服务，具体用哪个模型由客户端在每次请求里决定，
// 应用不替用户挑，也不该让人以为存在一个全局的「当前模型」。
// 所以 chip 上的动作是**复制**而不是选中 —— 用户拿到名字之后要做的下一件事，
// 正是把它填进客户端的配置里；而「选中」在这里根本没有对应的语义。
//
// 清单用 chip 而不是表格：模型名长度不齐，表格会逼出一堆留白；
// chip 自动换行，天然适合「一堆等价的候选项」这种形状。
// 多了搜索之后这一点更重要：模型有几十个时靠眼睛找不如直接打字。
//
// 同步走上游控制面，**不需要代理在运行** —— 只是「看看有哪些模型」而已，
// 为它先启动一个监听端口、替用户向上游发请求，代价和收益完全不成比例。
import { esc } from '../../core/format';
import { ACTION } from '../../state/actions';
import { busyAttr, emptyState, note } from '../shared/render';
import type { ViewSpec } from './types';

export const modelsView: ViewSpec = {
  id: 'models',
  label: '可用模型',
  title: '可用模型',
  desc: '当前 Kiro 账户可用的模型清单；具体用哪个由客户端在请求里指定，本应用不做选择',

  head: (state) => {
    const label = state.busyLabel === ACTION.syncModels ? '同步中…' : '从上游同步';
    return `<button class="small" data-action="sync-models"${busyAttr(state.busy)}>${label}</button>`;
  },

  body: (state) => {
    const all = state.models?.models ?? [];
    const query = state.modelFilter.trim();
    const needle = query.toLowerCase();
    const shown = needle ? all.filter((id) => id.toLowerCase().includes(needle)) : all;

    const inventory = `
      <div class="card">
        <div class="card-head">
          <span class="card-title">可用模型</span>
          <span class="card-head-spacer"></span>
          <span class="card-sub">${countLabel(all.length, shown.length, query)}</span>
        </div>
        ${all.length ? filterBar(query) : ''}
        ${inventoryBody(shown, all.length, query)}
      </div>`;

    return `
      ${inventory}
      ${note(
        '点一下模型名即可复制，再把它填在客户端的配置里（例如 Claude Code 的 ' +
          '<code>ANTHROPIC_MODEL</code>）；不填则由上游按账户情况自行选择。' +
          '清单以套餐为准，套餐变化后重新同步即可。',
      )}`;
  },
};

/** 右上角的计数：没搜索时是总数，搜索时是「命中 / 总数」。 */
function countLabel(total: number, matched: number, query: string): string {
  if (!total) return '';
  return query ? `${matched} / ${total}` : `共 ${total} 个`;
}

/**
 * 搜索框 + 清空按钮。
 *
 * 只在「真有清单」时才渲染 —— 空清单下面没有东西可筛，摆一个搜了也没用的输入框
 * 只会让人怀疑是不是自己搜错了。
 *
 * 输入框必须带 `id`：外壳重绘时靠它把焦点与光标位置放回去（见 `ui/shell/shell.ts`），
 * 没有 id 的话边打字边筛选的体验是「每敲一个字就丢一次焦点」。
 */
function filterBar(query: string): string {
  const clear = query
    ? '<button class="filter-clear" type="button" data-action="clear-model-filter"' +
      ' data-refocus="model-filter" aria-label="清空搜索" title="清空搜索">×</button>'
    : '';
  return `
    <div class="filter-bar">
      <input class="filter-input" id="model-filter" type="text" value="${esc(query)}"
             placeholder="搜索模型名" autocomplete="off" spellcheck="false"
             aria-label="搜索模型名" />
      ${clear}
    </div>`;
}

function inventoryBody(shown: string[], total: number, query: string): string {
  if (!total) {
    return emptyState('还没有模型清单', '点击右上角「从上游同步」向 Kiro 账户读取');
  }
  if (!shown.length) {
    return emptyState(`没有匹配「${query}」的模型`, '换个关键词，或点右侧「×」清空搜索');
  }
  return `
    <div class="chips">
      ${shown
        .map(
          (id) =>
            `<button class="chip" type="button" data-action="copy-model"` +
            ` data-model="${esc(id)}" title="点击复制「${esc(id)}」">${esc(id)}</button>`,
        )
        .join('')}
    </div>`;
}
