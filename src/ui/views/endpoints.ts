// 双协议接入：两套协议并存是产品核心，所以这一页把「地址 + 三种复制格式」摊开。
//
// 两块协议面板用同一套结构渲染（只是数据不同），左右并排便于逐项比对；
// 窄窗口下自动落回单列 —— 用 grid 而不是 flex 是为了不必手写两份媒体查询。
//
// 为什么「重置密钥」在这一页而不是「维护」：重置会立刻让已配置的客户端失配，
// 而客户端配的就是这一页给出的地址与密钥。把它放在被它影响的东西旁边，
// 用户重新拿一份配置的动作就在手边，不必在两个分区之间来回跑。
import { esc } from '../../core/format';
import { copyKindNeedsKey } from '../../core/ipc';
import type { CopyKind } from '../../core/types';
import { ACTION } from '../../state/actions';
import { isBlocking, isPending, type AppState } from '../../state/app-state';
import { actionAttrs, kv, kvList, mono, note } from '../shared/render';
import type { ViewSpec } from './types';

interface ProtoSpec {
  name: string;
  path: string;
  desc: string;
  /** 面板左侧竖条与路径标签的颜色：并排时靠颜色最快分辨是哪一套。 */
  color: string;
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
      path: '/v1/messages',
      desc: 'Claude Code、Anthropic SDK、各类 Anthropic 兼容客户端',
      color: '#0f8f86',
      url: access?.anthropicBaseUrl ?? '',
      baseKind: 'anthropic_base_url',
      envKind: 'anthropic_env',
      jsonKind: 'anthropic_json',
    },
    {
      name: 'OpenAI Chat Completions',
      path: '/openai/v1',
      desc: 'OpenAI SDK、LangChain、Cherry Studio 等兼容客户端',
      color: '#5e6ad2',
      url: access?.openaiBaseUrl ?? '',
      baseKind: 'openai_base_url',
      envKind: 'openai_env',
      jsonKind: 'openai_json',
    },
  ];
}

/**
 * 复制按钮。
 *
 * 禁用态用「追加一个属性」表达，而不是在模板里塞 `${cond ? 'disabled' : ''}`：
 * 后者会把每个按钮标签行推过可读宽度，也让每个按钮的写法都略有不同。
 */
function copyButton(kind: CopyKind, label: string, enabled = true): string {
  return (
    `<button class="small" data-action="copy" data-kind="${kind}"` +
    `${enabled ? '' : ' disabled'}>${esc(label)}</button>`
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

  return `
    <div class="proto" style="--proto-color:${proto.color}">
      <div class="proto-head">
        <span class="proto-name">${esc(proto.name)}</span>
        <span class="proto-path">${esc(proto.path)}</span>
      </div>
      <div class="proto-desc">${esc(proto.desc)}</div>
      <div class="proto-url${urlMissing ? ' is-empty' : ''}">
        ${urlMissing ? '启动代理后显示' : esc(proto.url)}
      </div>
      <div class="btn-row">
        ${copyButton(proto.baseKind, '复制地址', canCopy(proto.baseKind))}
        ${copyButton(proto.envKind, '复制 .env', canCopy(proto.envKind))}
        ${copyButton(proto.jsonKind, '复制 JSON', canCopy(proto.jsonKind))}
      </div>
    </div>`;
}

export const endpointsView: ViewSpec = {
  id: 'endpoints',
  label: '双协议接入',
  title: '双协议接入',
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
        ${
          running
            ? note('重置密钥需要先停止代理，避免正在使用的客户端突然失配。', 'warn')
            : note('重置后旧密钥立即失效，已配置的客户端需要重新粘贴。')
        }
      </div>`;

    const protoCard = `
      <div class="card">
        <div class="card-head">
          <span class="card-title">协议地址</span>
          <span class="card-sub">按客户端支持的协议二选一即可</span>
        </div>
        <div class="proto-grid">
          ${protoSpecs(state).map((proto) => renderProto(proto, state)).join('')}
        </div>
      </div>`;

    return `
      ${credentialCard}
      ${protoCard}
      ${note(
        '模型由客户端在请求里自己指定（本应用不代选），可选项见「可用模型」分区；' +
          '复制的配置片段里因此不含模型名。',
      )}`;
  },
};
