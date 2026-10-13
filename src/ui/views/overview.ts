// 概览：一屏之内回答「现在能不能用、接入地址是什么、还剩多少」。
//
// 这一页是唯一允许汇总别处分区信息的地方 —— 它是入口，用户第一眼就在这里。
// 因此刻意只放**结论**（跑了没有、接入地址、两个关键数字），细节都留在各自分区，
// 并用可点击的指标块把跳转做出来，避免这一页长成第二个「什么都有一点」的长列表。
import { esc } from '../../core/format';
import { ACTION } from '../../state/actions';
import { serviceAddress, summarizeStatus } from '../../state/app-state';
import { busyAttr, note, statTile, statusPill, kvList, kv } from '../shared/render';
import { ICON } from '../shared/icons';
import type { ViewSpec } from './types';

/** 一行接入地址：协议名 + 地址 + 复制。 */
function endpointRow(name: string, url: string, kind: string, canCopy: boolean): string {
  return `
    <li class="endpoint-row">
      <span class="endpoint-name">${esc(name)}</span>
      ${
        url
          ? `<code class="endpoint-url">${esc(url)}</code>`
          : '<span class="endpoint-url is-empty">启动代理后显示</span>'
      }
      <button class="small" data-action="copy" data-kind="${kind}"
              ${canCopy && url ? '' : 'disabled'}>复制</button>
    </li>`;
}

export const overviewView: ViewSpec = {
  id: 'overview',
  label: '概览',
  title: '概览',
  desc: '本地服务、凭据与接入地址的当前快照',

  // 「刷新」是只读的补一次同步，不参与 busy 门控：别处卡住时它就是用户手边的一根救命绳
  // （真正的兜底是 run() 的超时，这里只是让用户不必干等）。
  head: () => `<button class="small" data-action="refresh">刷新</button>`,

  body: (state) => {
    const status = state.status;
    const summary = summarizeStatus(state);
    const address = serviceAddress(state);
    const running = Boolean(status?.running);
    const access = state.access;
    const hasKey = Boolean(access?.hasApiKey);
    const models = state.models;
    const usage = state.usage;
    // 进行态文案：`busy` 只负责把按钮标成不可用，`busyLabel` 才说明「是谁在跑」。
    // 只有正在跑的那一个按钮改文案，其余按钮保持原名（只是灰着），避免张冠李戴。
    const startText = state.busyLabel === ACTION.startProxy ? '启动中…' : '启动代理';
    const stopText = state.busyLabel === ACTION.stopProxy ? '停止中…' : '停止代理';
    const refreshText = state.busyLabel === ACTION.refreshToken ? '刷新中…' : '刷新凭据';

    const serviceCard = `
      <div class="card">
        <div class="card-head">
          <span class="card-title">本地服务</span>
          ${statusPill(summary)}
          <span class="card-head-spacer"></span>
          ${
            running
              ? `<button class="danger" data-action="proxy-stop"
                         ${busyAttr(state.busy)}>${stopText}</button>`
              : `<button class="primary" data-action="proxy-start"
                         ${status?.available ? busyAttr(state.busy) : ' disabled'}>${startText}</button>`
          }
        </div>

        ${kvList([
          kv(
            '服务地址',
            address ? `<code>${esc(address)}</code>` : '<span class="dim">未启动</span>',
          ),
          kv('监听端口', portValue(status?.port)),
          kv('随应用启动', status?.autoStart ? '已开启' : '已关闭'),
        ])}

        <div class="field-row">
          <label class="field">
            <span>端口</span>
            <input id="port-input" type="number" min="1024" max="65535"
                   value="${status?.port ?? 5680}" ${running ? 'disabled' : ''} />
          </label>
          <label class="switch">
            <input id="autostart-input" type="checkbox" ${status?.autoStart ? 'checked' : ''} />
            <span>随应用自动启动</span>
          </label>
        </div>

        ${running ? '' : note('代理未运行时，双协议接口均不可用。启动后地址会自动填入下方。')}
      </div>`;

    const tiles = `
      <div class="stat-grid">
        ${statTile({
          label: '可用模型',
          value: String(models?.models.length ?? 0),
          hint: models?.models.length ? '来自 Kiro 账户' : '尚未同步',
          goto: 'models',
        })}
        ${statTile({
          label: '额度已用',
          value: usage ? `${usage.percentUsed}%` : '—',
          hint: usage ? `剩余 ${usage.remaining}` : '点击查询',
          goto: 'usage',
        })}
      </div>`;

    const credentialCard = `
      <div class="card">
        <div class="card-head">
          <span class="card-title">Kiro 凭据</span>
          <span class="card-head-spacer"></span>
          <button class="small" data-action="refresh-token"
                  ${busyAttr(state.busy)}>${refreshText}</button>
        </div>
        ${
          status?.available
            ? kvList([
                kv('来源', esc(status.authSource)),
                kv('到期', status.expiresAt ? `<code>${esc(status.expiresAt)}</code>` : '—'),
              ])
            : note(
                '未找到凭据。请先登录 Kiro IDE，或执行 <code>kiro-cli login</code>，' +
                  '确认 <code>~/.aws/sso/cache/kiro-auth-token.json</code> 存在。',
                'warn',
              )
        }
      </div>`;

    const accessCard = `
      <div class="card">
        <div class="card-head">
          <span class="card-title">接入地址</span>
          <span class="card-head-spacer"></span>
          <button class="small" data-action="goto" data-view="endpoints">
            查看接入明细<span class="btn-arrow">${ICON.chevron}</span>
          </button>
        </div>
        <ul class="endpoint-list">
          ${endpointRow('Anthropic', access?.anthropicBaseUrl ?? '', 'anthropic_base_url', hasKey)}
          ${endpointRow('OpenAI', access?.openaiBaseUrl ?? '', 'openai_base_url', hasKey)}
        </ul>
      </div>`;

    return `
      ${serviceCard}
      ${tiles}
      <div class="grid-2">
        ${credentialCard}
        ${accessCard}
      </div>`;
  },
};

function portValue(port: number | null | undefined): string {
  return port ? `<code>${port}</code>` : '<span class="dim">未启动</span>';
}
