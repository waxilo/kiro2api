// 可用模型：一份只读的清单，回答「这个账户能用哪些模型」。
//
// 这里刻意**没有**模型选择器。本应用是代理服务，具体用哪个模型由客户端在每次请求里决定，
// 应用不替用户挑，也不该让人以为存在一个全局的「当前模型」。
//
// 清单用 chip 而不是表格：模型名长度不齐，表格会逼出一堆留白；
// chip 自动换行，天然适合「一堆等价的候选项」这种形状。
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
    const list = state.models?.models ?? [];

    const inventory = `
      <div class="card">
        <div class="card-head">
          <span class="card-title">可用模型</span>
          <span class="card-head-spacer"></span>
          <span class="card-sub">${list.length ? `共 ${list.length} 个` : ''}</span>
        </div>
        ${
          list.length
            ? `<div class="chips">${list
                .map((id) => `<span class="chip">${esc(id)}</span>`)
                .join('')}</div>`
            : emptyState('还没有模型清单', '点击右上角「从上游同步」向 Kiro 账户读取')
        }
      </div>`;

    return `
      ${inventory}
      ${note(
        '把模型名填在客户端的配置里即可（例如 Claude Code 的 <code>ANTHROPIC_MODEL</code>）；' +
          '不填则由上游按账户情况自行选择。清单以套餐为准，套餐变化后重新同步即可。',
      )}`;
  },
};
