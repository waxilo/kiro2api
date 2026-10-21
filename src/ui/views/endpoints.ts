// 协议接入：两套协议并存是产品核心，所以这一页把两套地址并排摊开。
//
// 两张协议块上下堆叠、同一套结构渲染（只是数据不同），块内只有两段：
// 身份（图标 + 名称 + 路由）与「地址条 + 复制地址」。身份用主题色的圆角图标底区分协议。
// 复制反馈长在按钮上（「已复制」短暂替换文案），失败仍走 toast 给真实原因。
//
// 这一块做减法的顺序：先把「适用客户端」的一排描边胶囊压成一句灰字，再整句去掉；
// 「复制 .env / 复制 JSON」两颗次要按钮也去掉 —— 地址条 + 一颗「复制地址」已经把
// 「怎么连上来」说完了，其余都是同一件事的啰嗦版本。复制按钮挪到地址条右边同一行，
// 于是整块只剩两段、两行高。
//
// 把模型写进第三方客户端的操作不在这里 —— 那是「三方接入」分区的职责，
// 这一页只回答「客户端该怎么连上来」。
//
// 为什么「重置密钥」在这一页而不是「维护」：重置会立刻让已配置的客户端失配，
// 而客户端配的就是这一页给出的地址与密钥。把它放在被它影响的东西旁边，
// 用户重新拿一份配置的动作就在手边，不必在两个分区之间来回跑。
import { esc } from '../../core/format';
import { copyKindNeedsKey } from '../../core/ipc';
import type { CopyKind } from '../../core/types';
import { ACTION } from '../../state/actions';
import { isBlocking, isPending, type AppState } from '../../state/app-state';
import { actionAttrs, kv, kvList, mono, statusPill, swapLabel } from '../shared/render';
import { ICON } from '../shared/icons';
import type { ViewSpec } from './types';

interface ProtoSpec {
  name: string;
  /** 协议类型标签：客户端真正请求的路由。 */
  typeLabel: string;
  /** 图标底色与选中标记的主题色：青绿 = Anthropic，蓝紫 = OpenAI。 */
  color: string;
  glyph: string;
  url: string;
  baseKind: CopyKind;
}

function protoSpecs(state: AppState): ProtoSpec[] {
  const access = state.access;
  return [
    {
      name: 'Anthropic Messages',
      typeLabel: '/anthropic/v1/messages',
      color: '#0D9488',
      glyph: ICON.anthropic,
      url: access?.anthropicBaseUrl ?? '',
      baseKind: 'anthropic_base_url',
    },
    {
      name: 'OpenAI Chat Completions',
      typeLabel: '/openai/v1',
      color: '#6366F1',
      glyph: ICON.openai,
      url: access?.openaiBaseUrl ?? '',
      baseKind: 'openai_base_url',
    },
  ];
}

/**
 * 复制地址按钮。成功反馈直接把文案换成「已复制」（状态在 `AppState.copiedKind`）。
 *
 * 文案走 `swapLabel`：「复制地址」四字换成「已复制」三字，按钮会缩 13px，地址条于是
 * 跟着变宽一下、再缩回去。「已复制」1.6 秒后复原，整行来回抖两次。
 * 两段都渲染、只显示当前那段，按钮宽度就锁在最宽的一份上。
 */
function copyButton(
  kind: CopyKind,
  label: string,
  state: AppState,
  enabled: boolean,
): string {
  const copied = state.copiedKind === kind;
  return (
    `<button class="primary${copied ? ' is-copied' : ''}" data-action="copy" data-kind="${kind}"` +
    `${enabled ? '' : ' disabled'}>${swapLabel(copied ? '已复制' : label, copied ? label : '已复制')}</button>`
  );
}

function renderProto(proto: ProtoSpec, state: AppState): string {
  const hasKey = Boolean(state.access?.hasApiKey);
  const urlMissing = !proto.url;
  // 复制本身很快，但它读的是同一份配置 —— 有动作在改配置时先别读，
  // 否则可能复制到「改到一半」的地址，用户拿到的配置和界面上显示的对不上。
  const baseDisabled = isBlocking(state) || urlMissing;
  // 需要密钥的种类在无密钥时禁用：与后端 `kind_needs_key` 共用同一判据
  const canCopy = (kind: CopyKind): boolean => !baseDisabled && (!copyKindNeedsKey(kind) || hasKey);

  // 状态标签只在应用真能确认服务在跑时出现（代理运行中），绝不伪造在线状态。
  const status = state.status?.running ? statusPill({ tone: 'ok', label: '运行中', detail: '' }) : '';

  return `
    <div class="proto" style="--proto-color:${proto.color}">
      <div class="proto-head">
        <span class="proto-badge">${proto.glyph}</span>
        <span class="proto-id">
          <span class="proto-name">${esc(proto.name)}</span>
          <span class="proto-type">${esc(proto.typeLabel)}</span>
        </span>
        ${status}
      </div>
      <div class="proto-endpoint">
        <div class="proto-url${urlMissing ? ' is-empty' : ''}" aria-label="API 地址">
          <span class="proto-url-text">${urlMissing ? '地址读取中' : esc(proto.url)}</span>
        </div>
        <div class="proto-actions">
          ${copyButton(proto.baseKind, '复制地址', state, canCopy(proto.baseKind))}
        </div>
      </div>
    </div>`;
}

export const endpointsView: ViewSpec = {
  id: 'endpoints',
  label: '协议接入',
  title: '协议接入',
  desc: '同一端口按路径分流，同一套密钥对两套协议同时生效',

  head: () => '',

  body: (state) => {
    const access = state.access;
    const hasKey = Boolean(access?.hasApiKey);
    const running = Boolean(state.status?.running);

    const credentialCard = `
      <div class="card">
        <div class="card-head">
          <span class="card-title">代理凭据</span>
          <span class="card-head-spacer"></span>
          <button class="small" data-action="copy" data-kind="api_key"
                  ${isBlocking(state) || !hasKey ? 'disabled' : ''}>复制密钥</button>
          <button class="small danger" data-action="reset-key"
                  ${running ? 'disabled' : actionAttrs(state, ACTION.resetKey)}>
            ${swapLabel(
              isPending(state, ACTION.resetKey) ? '重置中…' : '重置密钥',
              isPending(state, ACTION.resetKey) ? '重置密钥' : '重置中…',
            )}
          </button>
        </div>
        ${kvList([kv('代理密钥', mono(access?.apiKeyMasked))])}
      </div>`;

    const protoCard = `
      <div class="card">
        <div class="card-head">
          <span class="card-title">协议地址</span>
        </div>
        <div class="proto-grid">
          ${protoSpecs(state).map((proto) => renderProto(proto, state)).join('')}
        </div>
      </div>`;

    return `
      ${credentialCard}
      ${protoCard}`;
  },
};
