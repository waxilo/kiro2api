// 概览：一屏之内回答「现在能不能用、接入地址是什么、还剩多少」。
//
// 这一页是唯一允许汇总别处分区信息的地方 —— 它是入口，用户第一眼就在这里。
// 因此刻意只放**结论**（跑了没有、接入地址、两个关键数字）。
//
// 唯一写在这里的「设置」是默认推理与上下文：它们不改任何地址，只决定每个请求
// 有多贵、多慢，和「额度已用」是同一类读数 —— 开屏核对一眼、改完看旁边额度在动。
//
// 两个数字的细节不再各占一个分区，而是点开就地的弹窗（见 `ui/modals`）：
// 「可用模型」「额度已用」本质上都是只读的、看一眼就关的东西，为它们单开页面等于让用户
// 看完还得自己点回概览；放进弹窗，上下文（服务跑没跑、地址是什么）始终在旁边。
import { esc, localTime, money2, percent4, remainingUntil } from '../../core/format';
import type { PrefChoice } from '../../core/types';
import { ACTION } from '../../state/actions';
import { isPending, summarizeStatus, type AppState } from '../../state/app-state';
import { actionAttrs, note, statTile, statusPill, kvList, kv, swapLabel } from '../shared/render';
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

/**
 * 一个分段单选组（思考级别 / 上下文大小）。
 *
 * 复用「三方接入」页签的 `.tabs` / `.tab` 造型 —— 全局只有一种「选中」表达，
 * 这里不另造一套。选中项不挂 `data-action`：点它没有意义，也就不必白写一次配置。
 *
 * 上下文有九档，窄窗口下药丸会换行（`.tabs.is-wrap`），而不是横向溢出把卡片撑破。
 */
function prefSeg(
  state: AppState,
  label: string,
  action: 'set-effort' | 'set-context',
  dataKey: 'effort' | 'context',
  choice: PrefChoice<string | number>,
): string {
  const options = choice.options
    .map((option) => {
      const on = String(option.value) === String(choice.value);
      // 英文档位名当注释：小一号灰字跟在中文说明后面，客户端里填的就是这几个字。
      const hint = option.hint ? `<span class="tab-hint">${esc(option.hint)}</span>` : '';
      if (on) {
        return `<button class="tab is-active" role="radio" aria-checked="true">${esc(option.label)}${hint}</button>`;
      }
      return (
        `<button class="tab" role="radio" aria-checked="false"` +
        ` data-action="${action}" data-${dataKey}="${esc(String(option.value))}"` +
        ` ${actionAttrs(state, ACTION.setDefaults)}>${esc(option.label)}${hint}</button>`
      );
    })
    .join('');
  return `<div class="tabs is-wrap" role="radiogroup" aria-label="${esc(label)}">${options}</div>`;
}

/**
 * 默认值卡片。首屏还没读到值时画一颗「读取中」的占位药丸：高度与真实控件一致，
 * 值回来时卡片不会被撑高。
 *
 * 它留在概览而不是某个设置分区：这两个值决定的是「接下来每个请求贵不贵、快不快」，
 * 和「额度已用」是同一类结论 —— 用户开屏就会核对一眼，改完也立刻看到旁边额度在动。
 *
 * 说明文案挂在标题旁的感叹号上，不做脚注：两行控件已经把卡片填满，脚注白占一行高度，
 * 而它只在「想搞清楚为什么」时才有人读 —— 收进悬浮提示，需要的人自己去找。
 */
function prefsCard(state: AppState): string {
  const prefs = state.prefs;
  const placeholder = '<div class="tabs is-wrap"><button class="tab" disabled>读取中</button></div>';
  const tip =
    '思考级别只在客户端没写 effort 时补上：某个模型没有这一档时自动取它能给的最高档，' +
    '完全不接受 effort 的模型则不发。上下文大小写进 Codex 模型目录，重启 Codex 后生效。';
  return `
    <div class="card">
      <div class="card-head">
        <span class="card-title">默认推理与上下文</span>
        <span class="pref-tip" tabindex="0" role="note"
              aria-label="${esc(tip)}" data-tip="${esc(tip)}">!</span>
        <span class="card-sub">客户端没指定时生效</span>
      </div>
      <div class="pref-row">
        <span class="pref-label">思考级别</span>
        ${
          prefs
            ? prefSeg(state, '默认思考级别', 'set-effort', 'effort', prefs.defaultEffort)
            : placeholder
        }
      </div>
      <div class="pref-row">
        <span class="pref-label">上下文</span>
        ${
          prefs
            ? prefSeg(state, '默认上下文大小', 'set-context', 'context', prefs.contextWindow)
            : placeholder
        }
      </div>
    </div>`;
}

export const overviewView: ViewSpec = {
  id: 'overview',
  label: '概览',
  title: '概览',
  desc: '本地服务、凭据、接入地址与默认参数的当前快照',

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
    //
    // 三处都走 `swapLabel`：文案长短不一样（「停止代理」80px vs「停止中…」76.5px），
    // 直接换字会让按钮宽度跟着变 —— 它是靠右排的，那一排就整体横移一下。
    const startText = swapLabel(
      isPending(state, ACTION.startProxy) ? '启动中…' : '启动代理',
      isPending(state, ACTION.startProxy) ? '启动代理' : '启动中…',
    );
    const stopText = swapLabel(
      isPending(state, ACTION.stopProxy) ? '停止中…' : '停止代理',
      isPending(state, ACTION.stopProxy) ? '停止代理' : '停止中…',
    );
    const refreshText = swapLabel(
      isPending(state, ACTION.refreshToken) ? '刷新中…' : '刷新凭据',
      isPending(state, ACTION.refreshToken) ? '刷新凭据' : '刷新中…',
    );
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

    // 两块可点的指标卡：它们打开的是弹窗，不是切页（见 `ui/modals`）。
    // 脚注写「点开能做什么」，而不是重复左边的标签 —— 用户已经知道自己是「额度」了，
    // 需要知道的是按下去会发生什么。
    const tiles = `
      <div class="stat-grid">
        ${statTile({
          label: '可用模型',
          value: String(models?.models.length ?? 0),
          hint: models?.models.length ? '来自 Kiro 账户 · 点开查看' : '尚未同步 · 点开同步',
          opens: 'models',
        })}
        ${statTile({
          label: '额度已用',
          value: usage ? `${percent4(usage.percentUsed)}%` : '—',
          hint: usage ? `剩余 ${money2(usage.remaining)} · 点开查看` : '还没查过 · 点开查询',
          opens: 'usage',
        })}
      </div>`;

    // 凭据这一格有两种完全不同的处境，动作也不同 —— 所以按钮跟着处境走：
    //
    // * 凭据可用 → 「刷新凭据」，真去上游 OIDC 换一次 token（它等网络，但不锁页面）。
    // * 凭据不可用 → 「重新检测」，只重读本地。此时「刷新凭据」必然失败（没有 refreshToken
    //   可刷），摆在那里只会把人引到一条死路上；而人真正会做的事是去窗口外面登录一次，
    //   再回来问「好了没」—— 这正是「重新检测」回答的问题。
    // 「登录」打开设备码登录弹窗（data-modal 让弹窗关闭时焦点回到这颗按钮）；
    // 已经是 Kiro2API 自己的登录时换成「退出登录」，回到读取 IDE / CLI 的凭据。
    const loginButton = status?.ownLogin
      ? `<button class="small" data-action="logout"${actionAttrs(state, ACTION.logout)}>退出登录</button>`
      : `<button class="small" data-action="open-modal" data-modal="login">登录</button>`;
    const credentialAction = !status
      ? ''
      : available
        ? `${loginButton}<button class="small" data-action="refresh-token"
                   ${actionAttrs(state, ACTION.refreshToken)}>${refreshText}</button>`
        : `${loginButton}<button class="small" data-action="refresh">重新检测</button>`;

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
            '未找到凭据。点右上角「登录」用 Builder ID / IAM Identity Center 直接登录；' +
              '也可以先登录 Kiro IDE 或执行 <code>kiro-cli login</code>，完成后点「重新检测」。',
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
      </div>
      ${prefsCard(state)}`;
  },
};
