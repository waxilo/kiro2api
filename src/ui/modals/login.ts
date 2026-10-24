// 登录（概览弹窗）：AWS SSO OIDC 设备码流程，Builder ID / 企业 IAM Identity Center。
//
// 两个阶段同一块卡片：
// * 没有进行中的登录 → 选方式（IdC 还要填 Start URL 与区域），点「打开浏览器登录」；
// * 等待确认 → 大字显示验证码，后台轮询；成功由 `login://done` 事件关掉弹窗。
//
// 凭据写进 kiro2api 自己的数据目录，不覆盖 Kiro IDE / kiro-cli 的登录。
import { esc, localTime } from '../../core/format';
import { ACTION } from '../../state/actions';
import { isPending, type AppState } from '../../state/app-state';
import { ICON } from '../shared/icons';
import { actionAttrs, note, swapLabel } from '../shared/render';
import type { ModalSpec } from './types';

const METHODS = [
  { id: 'builder', label: 'Builder ID' },
  { id: 'idc', label: 'IAM Identity Center' },
] as const;

function methodTabs(state: AppState): string {
  return `
    <div class="tabs" role="tablist" aria-label="登录方式">
      ${METHODS.map((m) => {
        const active = state.loginMethod === m.id;
        return (
          `<button class="tab${active ? ' is-active' : ''}" role="tab" type="button"` +
          ` aria-selected="${active}" data-action="login-method" data-method="${m.id}">` +
          `${esc(m.label)}</button>`
        );
      }).join('')}
    </div>`;
}

function formBody(state: AppState): string {
  const pending = isPending(state, ACTION.login);
  const startText = swapLabel(pending ? '正在发起…' : '打开浏览器登录', pending ? '打开浏览器登录' : '正在发起…');
  const fields =
    state.loginMethod === 'idc'
      ? `
        <label class="field">
          <span>Start URL</span>
          <input id="login-start-url" type="text" spellcheck="false" autocomplete="off"
                 placeholder="https://d-xxxxxxxxxx.awsapps.com/start"
                 value="${esc(state.loginStartUrl)}" />
        </label>
        <label class="field">
          <span>区域</span>
          <input id="login-region" type="text" spellcheck="false" autocomplete="off"
                 placeholder="us-east-1" value="${esc(state.loginRegion)}" />
        </label>`
      : note('使用个人 AWS Builder ID 登录（与 Kiro IDE 里「Sign in with Builder ID」相同）。');

  return `
    ${methodTabs(state)}
    ${fields}
    <div class="btn-row">
      <button class="primary" data-action="login-start"${actionAttrs(state, ACTION.login)}>${startText}</button>
    </div>`;
}

function waitingBody(state: AppState): string {
  const session = state.loginSession!;
  return `
    <div class="login-wait">
      <span class="login-wait-label">在浏览器里确认这个验证码</span>
      <code class="login-code">${esc(session.userCode)}</code>
      <span class="login-wait-hint">
        ${session.browserOpened ? '已打开浏览器' : '没能自动打开浏览器，请点「重新打开浏览器」'}
        · 验证码 ${esc(localTime(session.expiresAt))} 前有效
      </span>
    </div>
    <div class="btn-row">
      <button data-action="login-reopen">重新打开浏览器</button>
      <button data-action="login-cancel">取消</button>
    </div>`;
}

export const loginModal: ModalSpec = {
  id: 'login',
  title: '登录 Kiro',
  desc: '设备码登录，凭据只保存在 Kiro2API 自己的数据目录',
  icon: ICON.open,
  size: 'compact',
  head: () => '',
  body: (state) => `
    ${state.loginSession ? waitingBody(state) : formBody(state)}
    <div class="modal-foot">
      登录后 Kiro2API 优先使用这份凭据，不会改动 Kiro IDE / kiro-cli 的登录；
      退出登录后回到读取它们的凭据。
    </div>`,
};
