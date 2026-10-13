// 模型：一个选择器 + 一份全量清单。
//
// 清单用 chip 而不是表格：模型名长度不齐，表格会逼出一堆留白；
// chip 自动换行、也天然承担「当前项高亮」这个状态。
import { esc } from '../../core/format';
import { ACTION } from '../../state/actions';
import { busyAttr, emptyState, note } from '../shared/render';
import type { ViewSpec } from './types';

export const modelsView: ViewSpec = {
  id: 'models',
  label: '模型',
  title: '模型',
  desc: '默认模型在客户端未指定模型时生效；列表来自 Kiro 账户实际可用的模型',

  head: (state) => {
    const label = state.busyLabel === ACTION.syncModels ? '同步中…' : '从上游同步';
    // 代理没运行时是真禁用（原因写在本页正文里）；忙碌期改用 aria-disabled，
    // 点击照常派发，好让 run() 回一句「正在 X，请稍候」而不是无声无息。
    const gate = state.status?.running ? busyAttr(state.busy) : ' disabled';
    return `<button class="small" data-action="sync-models"${gate}>${label}</button>`;
  },

  body: (state) => {
    const models = state.models;
    const list = models?.models ?? [];
    const current = models?.defaultModel ?? '';
    const running = Boolean(state.status?.running);

    const chooser = `
      <div class="card">
        <div class="card-head">
          <span class="card-title">默认模型</span>
          <span class="card-head-spacer"></span>
          <span class="card-sub">${list.length ? `共 ${list.length} 个` : ''}</span>
        </div>
        <label class="field field-inline">
          <span>模型</span>
          <select id="model-select" ${state.busy || list.length === 0 ? 'disabled' : ''}>
            ${
              list.length === 0
                ? '<option value="">（先启动代理并同步）</option>'
                : list
                    .map(
                      (id) =>
                        `<option value="${esc(id)}"` +
                        `${id === current ? ' selected' : ''}>${esc(id)}</option>`,
                    )
                    .join('')
            }
          </select>
        </label>
        ${
          running
            ? ''
            : note('代理未运行，无法从上游同步。启动代理后回到这里点「从上游同步」。', 'warn')
        }
      </div>`;

    const inventory = `
      <div class="card">
        <div class="card-head">
          <span class="card-title">可用模型</span>
          <span class="card-sub">高亮项为当前默认模型</span>
        </div>
        ${
          list.length
            ? `<div class="chips">${list
                .map(
                  (id) =>
                    `<span class="chip${id === current ? ' is-current' : ''}">${esc(id)}</span>`,
                )
                .join('')}</div>`
            : emptyState('还没有模型列表', '启动代理后点击右上角「从上游同步」')
        }
      </div>`;

    return `${chooser}${inventory}`;
  },
};
