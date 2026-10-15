// 协议接入：两套协议并存是产品核心，所以这一页把「地址 + 三种复制格式」摊开。
//
// 两张协议卡片上下堆叠、同一套结构渲染（只是数据不同）：标题 / 图标 / 地址 /
// 客户端标签 / 操作行五段严格对齐，靠 36px 主题色图标底区分协议，不再用整条彩边。
// 复制反馈长在按钮上（「已复制」短暂替换文案），失败仍走 toast 给真实原因。
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
import { actionAttrs, kv, kvList, mono, statusPill } from '../shared/render';
import { ICON } from '../shared/icons';
import type { ViewSpec } from './types';

interface ProtoSpec {
  name: string;
  /** 协议类型标签：客户端真正请求的路由。 */
  typeLabel: string;
  /** 适用客户端（浅灰胶囊展示）。 */
  clients: string[];
  /** 图标底色与选中标记的主题色：青绿 = Anthropic，蓝紫 = OpenAI。 */
  color: string;
  glyph: string;
  url: string;
  baseKind: CopyKind;
  envKind: CopyKind;
  jsonKind: CopyKind;
}

function protoSpecs(state: AppState): ProtoSpec[] {
  const access = state.access;
  return [
    {
      name: 'Anthropic Messages',
      typeLabel: '/anthropic/v1/messages',
      clients: ['Claude Code', 'Anthropic SDK', 'Anthropic 兼容客户端'],
      color: '#0D9488',
      glyph: ICON.anthropic,
      url: access?.anthropicBaseUrl ?? '',
      baseKind: 'anthropic_base_url',
      envKind: 'anthropic_env',
      jsonKind: 'anthropic_json',
    },
    {
      name: 'OpenAI Chat Completions',
      typeLabel: '/openai/v1',
      clients: ['OpenAI SDK', 'LangChain', 'Cherry Studio', 'OpenAI 兼容客户端'],
      color: '#6366F1',
      glyph: ICON.openai,
      url: access?.openaiBaseUrl ?? '',
      baseKind: 'openai_base_url',
      envKind: 'openai_env',
      jsonKind: 'openai_json',
    },
  ];
}

/**
 * 复制按钮。成功反馈直接把文案换成「已复制」（状态在 `AppState.copiedKind`），
 * 主按钮只给「复制地址」——它是这一行里唯一的主操作。
 */
function copyButton(
  kind: CopyKind,
  label: string,
  state: AppState,
  enabled: boolean,
  main = false,
): string {
  const copied = state.copiedKind === kind;
  const cls = [main ? 'primary' : '', copied ? 'is-copied' : ''].filter(Boolean).join(' ');
  return (
    `<button${cls ? ` class="${cls}"` : ''} data-action="copy" data-kind="${kind}"` +
    `${enabled ? '' : ' disabled'}>${esc(copied ? '已复制' : label)}</button>`
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
        <span class="proto-name">${esc(proto.name)}</span>
        <span class="proto-type">${esc(proto.typeLabel)}</span>
        ${status}
      </div>
      <div class="proto-url${urlMissing ? ' is-empty' : ''}" aria-label="API 地址">
        <span class="proto-url-icon">${ICON.terminal}</span>
        <span class="proto-url-text">${urlMissing ? '地址读取中' : esc(proto.url)}</span>
      </div>
      <div class="proto-tags">
        ${proto.clients.map((client) => `<span class="proto-tag">${esc(client)}</span>`).join('')}
      </div>
      <div class="proto-actions">
        ${copyButton(proto.baseKind, '复制地址', state, canCopy(proto.baseKind), true)}
        ${copyButton(proto.envKind, '复制 .env', state, canCopy(proto.envKind))}
        ${copyButton(proto.jsonKind, '复制 JSON', state, canCopy(proto.jsonKind))}
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
            ${isPending(state, ACTION.resetKey) ? '重置中…' : '重置密钥'}
          </button>
        </div>
        ${kvList([kv('代理密钥', mono(access?.apiKeyMasked))])}
      </div>`;

    const protoCard = `
      <div class="card">
        <div class="card-head">
          <span class="card-title">协议地址</span>
          <span class="proto-count">2 种协议</span>
        </div>
        <div class="proto-intro">根据客户端选择对应协议，一键复制即可使用</div>
        <div class="proto-grid">
          ${protoSpecs(state).map((proto) => renderProto(proto, state)).join('')}
        </div>
      </div>`;

    return `
      ${credentialCard}
      ${protoCard}`;
  },
};
