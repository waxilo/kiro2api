// 账号：选择代理使用哪个 Kiro 账号。
//
// 三张分组卡对应三个**来源**，而不是三个「状态」：
//   * 登录的账号 —— 本应用发起的设备码登录，凭据存在自己的数据目录，由本应用续期；
//   * 本地 kiro-cli —— 从 CLI 的凭据库导入副本，续期**写回原库**，两边始终同一份；
//   * Kiro 桌面端 —— 就地使用 IDE 的共享缓存，不复制、也不代续（代续会让两边互相顶掉）。
//
// 为什么按来源分组而不是一张大表：三者的「归谁管」不同，用户需要知道的不是
// 「有几个账号」，而是「这个账号是谁在维护、我在这里删掉会不会影响 IDE / CLI」。
// 分组标题旁边那行小字就是答案，逐行重复一遍反而是噪音。
import { esc, localTime, remainingUntil } from '../../core/format';
import type { AccountInfo, AccountSource } from '../../core/types';
import { ACTION } from '../../state/actions';
import type { AppState } from '../../state/app-state';
import { ICON } from '../shared/icons';
import { actionAttrs, emptyState, note } from '../shared/render';
import type { ViewSpec } from './types';

interface GroupSpec {
  source: AccountSource;
  title: string;
  /** 卡片头右侧那句话：这组账号从哪来。 */
  origin: string;
  /** 一个都没有时的引导。 */
  emptyTitle: string;
  emptyHint: string;
  /** 卡片底部的一句话说明（可省略）。 */
  foot?: string;
}

const GROUPS: GroupSpec[] = [
  {
    source: 'login',
    title: '登录的账号',
    origin: '独立凭据目录',
    emptyTitle: '还没有登录的账号',
    emptyHint: '点右上角「登录账号」，用 Builder ID 或 IAM Identity Center 登录',
    foot: '每次设备码登录生成一个独立账号，可以同时保留多个；由本应用续期，未选中的也会在后台自动续。',
  },
  {
    source: 'cli',
    title: '本地 kiro-cli',
    origin: '本机凭据库',
    emptyTitle: '未检测到 kiro-cli 账号',
    emptyHint: '在终端执行 kiro-cli login 后回到本页点「重新扫描」',
    foot: '凭据从 kiro-cli 的库导入，续期会写回原库 —— 两边共用同一份，不会互相顶掉。',
  },
  {
    source: 'desktop',
    title: 'Kiro 桌面端',
    origin: '~/.aws/sso/cache',
    emptyTitle: '未检测到桌面端登录',
    emptyHint: '打开 Kiro IDE 登录一次，回到本页点「重新扫描」即可看到',
    foot: '就地使用 IDE 的共享缓存，不复制、不迁移，续期交给 IDE；这里也不能删除。',
  },
];

function providerName(account: AccountInfo): string {
  switch (account.provider) {
    case 'BuilderId':
      return 'Builder ID';
    case 'Enterprise':
      return 'IAM Identity Center';
    default:
      return account.authMethod === 'social' ? '社交登录' : 'IdC';
  }
}

function accountRow(account: AccountInfo, state: AppState): string {
  const meta = [
    providerName(account),
    account.region ? `区域 ${account.region}` : '',
    account.plan ?? '',
  ].filter(Boolean);
  const expiry = account.expiresAt
    ? `到期 ${localTime(account.expiresAt)}（${remainingUntil(account.expiresAt)}）`
    : '';

  // 选中标记只有一种写法（与导航、模型卡同一个「选中」语义）；
  // 不可用则单独一颗警示胶囊 —— 它解释的是「为什么这颗按钮点了可能失败」。
  const badges = [
    account.selected ? '<span class="pill ok"><span class="pill-dot"></span>使用中</span>' : '',
    !account.usable ? '<span class="pill warn"><span class="pill-dot"></span>不可用</span>' : '',
  ].join('');

  // 「退出登录」只出现在登录账号上：桌面端归 IDE、CLI 归 kiro-cli，
  // 在这边删掉目录不会让它们退出登录，下一次扫描又会扫回来。
  const remove = account.removable
    ? `<button class="small ghost" data-action="account-remove" data-id="${esc(account.id)}"
               ${actionAttrs(state, ACTION.removeAccount)}>退出登录</button>`
    : '';
  const select = account.selected
    ? ''
    : `<button class="small" data-action="account-select" data-id="${esc(account.id)}"
               ${actionAttrs(state, ACTION.selectAccount)}>使用此账号</button>`;

  return `
    <div class="account-row${account.selected ? ' is-selected' : ''}${account.usable ? '' : ' is-stale'}">
      <span class="account-mark">${ICON.account}</span>
      <div class="account-text">
        <div class="account-name">
          <span class="account-label" title="${esc(account.label)}">${esc(account.label)}</span>
          ${badges}
        </div>
        <div class="account-meta">
          ${meta.map((item) => `<span>${esc(item)}</span>`).join('<span class="dot">·</span>')}
          ${expiry ? `<span class="dot">·</span><span>${esc(expiry)}</span>` : ''}
        </div>
        ${account.note ? `<div class="account-note">${esc(account.note)}</div>` : ''}
      </div>
      <div class="account-tools">${select}${remove}</div>
    </div>`;
}

function groupCard(group: GroupSpec, state: AppState): string {
  const accounts = (state.accounts?.accounts ?? []).filter(
    (account) => account.source === group.source,
  );
  const body = accounts.length
    ? `<div class="account-list">${accounts
        .map((account) => accountRow(account, state))
        .join('')}</div>`
    : emptyState(group.emptyTitle, group.emptyHint, ICON.account);

  return `
    <div class="card">
      <div class="card-head">
        <span class="card-title">${esc(group.title)}</span>
        <span class="card-head-spacer"></span>
        <span class="card-sub">${accounts.length ? `${accounts.length} 个 · ` : ''}${esc(group.origin)}</span>
      </div>
      ${body}
      ${group.foot ? `<p class="account-foot">${esc(group.foot)}</p>` : ''}
    </div>`;
}

export const accountsView: ViewSpec = {
  id: 'accounts',
  label: '账号',
  title: '账号',
  desc: '选择代理使用哪个 Kiro 账号；三类来源分开列出',

  // 刷新放在这里而不是自动轮询：用户在 IDE / 终端里登录完，回来点一下就能看到。
  // 首屏那次扫描由分区导航触发（见 `state/actions` 的 `navigate`）。
  head: () =>
    `<button class="small ghost" data-action="refresh">重新扫描</button>` +
    `<button class="primary" data-action="open-modal" data-modal="login">登录账号</button>`,

  body: (state) => {
    if (!state.accounts) {
      return `${note('正在扫描本机的账号…')}`;
    }
    return GROUPS.map((group) => groupCard(group, state)).join('');
  },
};
