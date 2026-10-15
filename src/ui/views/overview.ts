// 概览：一屏之内回答「现在能不能用、接入地址是什么、还剩多少」。
//
// 这一页是唯一允许汇总别处分区信息的地方 —— 它是入口，用户第一眼就在这里。
// 因此刻意只放**结论**（跑了没有、接入地址、两个关键数字），细节都留在各自分区，
// 并用可点击的指标块把跳转做出来，避免这一页长成第二个「什么都有一点」的长列表。
import { esc, localTime, percent4, remainingUntil } from '../../core/format';
import { ACTION } from '../../state/actions';
import { isPending, summarizeStatus } from '../../state/app-state';
import { actionAttrs, note, statTile, statusPill, kvList, kv } from '../shared/render';
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

  // 这一页不再有标题右侧的「刷新」。
  //
  // 它做的只是「从本地后端重读一遍状态」（四个不发网络的 IPC），而这件事四条路径都在自动做：
  // 首屏、每个动作收尾、后端推送事件、窗口重新可见 / 重新获得焦点。留一颗按钮在那里，
  // 用户只会得到一个问题 ——「它到底刷新了什么」，答不上来就说明它不该在。
  //
  // 唯一没被自动覆盖的场景（应用一直开着、用户在窗口外面登录了 Kiro）现在由
  // 「Kiro 凭据」卡片里的「重新检测」接手：它出现在凭据不可用的那一刻，
  // 正好在「请先登录 Kiro IDE / 执行 kiro-cli login」那句提示旁边。
  head: () => '',

  body: (state) => {
    const status = state.status;
    const summary = summarizeStatus(state);
    const running = Boolean(status?.running);
    const access = state.access;
    const hasKey = Boolean(access?.hasApiKey);
    const models = state.models;
    const usage = state.usage;
    const available = Boolean(status?.available);
    // 进行态文案：`pending` 里那条记录只负责把按钮标成不可用，名字才说明「是谁在跑」。
    // 只有正在跑的那一个按钮改文案，其余按钮保持原名（只是灰着），避免张冠李戴。
    const startText = isPending(state, ACTION.startProxy) ? '启动中…' : '启动代理';
    const stopText = isPending(state, ACTION.stopProxy) ? '停止中…' : '停止代理';
    const refreshText = isPending(state, ACTION.refreshToken) ? '刷新中…' : '刷新凭据';
    const expiryNote = status?.expiresAt ? remainingUntil(status.expiresAt) : '';

    const serviceCard = `
      <div class="card">
        <div class="card-head">
          <span class="card-title">本地服务</span>
          ${statusPill(summary)}
          <span class="card-head-spacer"></span>
          ${
            running
              ? `<button class="danger" data-action="proxy-stop"
                         ${actionAttrs(state, ACTION.stopProxy)}>${stopText}</button>`
              : `<button class="primary" data-action="proxy-start"
                         ${available ? actionAttrs(state, ACTION.startProxy) : ' disabled'}>${startText}</button>`
          }
        </div>

        <label class="field is-row">
          <span>端口</span>
          <input id="port-input" type="number" min="1024" max="65535"
                 value="${status?.port ?? 5680}" ${running ? 'disabled' : ''} />
        </label>
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
          value: usage ? `${percent4(usage.percentUsed)}%` : '—',
          hint: usage ? `剩余 ${usage.remaining}` : '点击查询',
          goto: 'usage',
        })}
      </div>`;

    // 凭据这一格有两种完全不同的处境，动作也不同 —— 所以按钮跟着处境走：
    //
    // * 凭据可用 → 「刷新凭据」，真去上游 OIDC 换一次 token（它等网络，但不锁页面）。
    // * 凭据不可用 → 「重新检测」，只重读本地。此时「刷新凭据」必然失败（没有 refreshToken
    //   可刷），摆在那里只会把人引到一条死路上；而人真正会做的事是去窗口外面登录一次，
    //   再回来问「好了没」—— 这正是「重新检测」回答的问题。
    const credentialAction = !status
      ? ''
      : available
        ? `<button class="small" data-action="refresh-token"
                   ${actionAttrs(state, ACTION.refreshToken)}>${refreshText}</button>`
        : `<button class="small" data-action="refresh">重新检测</button>`;

    const credentialBody = !status
      ? note('正在读取凭据状态…')
      : available
        ? kvList([
            kv('来源', esc(status.authSource)),
            kv(
              '到期',
              status.expiresAt
                ? `<code>${esc(localTime(status.expiresAt))}</code>${expiryNote ? `（${expiryNote}）` : ''}`
                : '—',
            ),
          ])
        : note(
            '未找到凭据。请先登录 Kiro IDE，或执行 <code>kiro-cli login</code>，' +
              '确认 <code>~/.aws/sso/cache/kiro-auth-token.json</code> 存在；' +
              '登录完成后点右上角「重新检测」。',
            'warn',
          );

    const credentialCard = `
      <div class="card">
        <div class="card-head">
          <span class="card-title">Kiro 凭据</span>
          <span class="card-head-spacer"></span>
          ${credentialAction}
        </div>
        ${credentialBody}
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
