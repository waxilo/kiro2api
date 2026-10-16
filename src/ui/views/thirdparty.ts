// 三方接入：管理第三方客户端里「指向本应用」的自定义模型。
//
// 这一页只认地址与端口都指向本代理的条目（host+端口匹配在 Rust 侧完成）：
// WorkBuddy 里配给别的服务的模型一概不出现 —— 看不见，也就删不到。
// 扫描在进入分区、焦点回到窗口和每次动作收尾时各刷一次，纯本地读盘，没有加载态可言。
//
// tab 条按浏览器页签的形制设计（在上的横条、激活项与内容区相连），
// 每个客户端一个页签，加新客户端只多一个 tab，不动布局。
//
// 模型区是一张可多选的卡片网格，勾选状态直接表达「在不在客户端里」：
// WorkBuddy 勾的是自定义模型清单，Codex 勾的是 /model 选择器的目录文件
// （kiro2api-model-catalog.json）。勾选都不立即写盘 —— 攒到点「应用」
// 一次性落地：批量操作少等多次写盘，误点也可以「恢复」反悔。
import { esc } from '../../core/format';
import { ACTION } from '../../state/actions';
import { type AppState, type ThirdPartyTab } from '../../state/app-state';
import { renderEnglishField } from '../shared/english-field';
import { actionAttrs, emptyState, note, rateLabel, statusPill, tonePill } from '../shared/render';
import type { ViewSpec } from './types';

interface TabSpec {
  id: ThirdPartyTab;
  label: string;
}

const TABS: TabSpec[] = [
  { id: 'workbuddy', label: 'WorkBuddy' },
  { id: 'codex', label: 'Codex' },
];

/** Codex 桌面端只认 GPT 系模型，网格与目录都只留 gpt- 开头的（与 Rust 侧 ModelFamily::classify 同一规则）。 */
const isGptModel = (id: string) => id.trim().startsWith('gpt-');

export const thirdpartyView: ViewSpec = {
  id: 'thirdparty',
  label: '三方接入',
  title: '三方接入',
  desc: '管理第三方客户端里指向本应用的自定义模型；只认地址与端口都匹配的条目',

  head: () => '',

  body: (state) =>
    `${tabBar(state)}${state.thirdPartyTab === 'workbuddy' ? workbuddyPanel(state) : codexPanel(state)}`,
};

function tabBar(state: AppState): string {
  return `
    <div class="tabs" role="tablist" aria-label="第三方客户端">
      ${TABS.map((tab) => {
        const active = tab.id === state.thirdPartyTab;
        return (
          `<button class="tab${active ? ' is-active' : ''}" role="tab"` +
          ` aria-selected="${active}" type="button"` +
          ` data-action="thirdparty-tab" data-tab="${tab.id}">${esc(tab.label)}</button>`
        );
      }).join('')}
    </div>`;
}

function workbuddyPanel(state: AppState): string {
  const scan = state.workbuddyScan;
  if (!scan) {
    return `<div class="card">${emptyState('正在读取 WorkBuddy 清单')}</div>`;
  }
  if (!scan.installed) {
    return note(
      '未找到 WorkBuddy（<code>~/.workbuddy</code> 不存在）。请确认 WorkBuddy 已安装并启动过一次，再回到这里管理。',
    );
  }
  const scanError = scan.error ? note(esc(scan.error), 'warn') : '';
  return `${scanError}${modelsCard(state)}`;
}

/**
 * Codex 面板：与 WorkBuddy 同一张卡形 —— 卡头（标题 · 计数 · 恢复 · 清空 ·
 * 应用），下面依次是搜索框、网格、说明行。
 *
 * 差异只在语义：WorkBuddy 勾的是自定义模型清单，Codex 勾的是 /model 选择器的
 * 目录文件（kiro2api-model-catalog.json）。provider 的异常状态（密钥不一致、
 * 已配置未激活）不进卡头 —— 用与扫描错误同款的警示条就地说明，健康态不占地方；
 * 目录指针被别家接管时网格退化成一句说明 —— 本应用不碰别家的目录。
 */
function codexPanel(state: AppState): string {
  const scan = state.codexScan;
  if (!scan) {
    return `<div class="card">${emptyState('正在读取 Codex 配置')}</div>`;
  }
  if (!scan.installed) {
    return note(
      '未找到 Codex（<code>~/.codex</code> 不存在）。请确认 Codex CLI 已安装并运行过一次，再回到这里管理。',
    );
  }
  const scanError = scan.error ? note(esc(scan.error), 'warn') : '';
  return `${scanError}${codexStateNote(state)}${codexCard(state)}`;
}

/** provider 的两种异常状态：与扫描错误同一条警示条通道，健康态不占地方。 */
function codexStateNote(state: AppState): string {
  const scan = state.codexScan;
  if (!scan || !scan.entries.length) return '';
  if (!scan.entries.every((entry) => entry.keyMatches)) {
    return note(
      'auth.json 里的密钥与当前代理密钥不一致，Codex 的请求会被 401 拒绝；点「应用」用当前密钥覆盖。',
      'warn',
    );
  }
  if (!scan.entries.some((entry) => entry.active)) {
    return note(
      'provider 已写入但 Codex 没在用它（顶层 model_provider 指向别家）；点「应用」切到本应用。',
      'warn',
    );
  }
  return '';
}

function codexCard(state: AppState): string {
  const scan = state.codexScan;
  if (!scan) return '';
  const entries = scan.entries;
  const imported = entries.length > 0;
  // 单供应商正常只有一条；多条按最坏情况算密钥状态，与 WorkBuddy 面板同一口径
  const keyOk = entries.every((entry) => entry.keyMatches);
  const active = imported && entries.some((entry) => entry.active);
  const foreign = scan.catalogState === 'foreign';
  const catalogIds = scan.catalogState === 'ours' ? scan.catalogModels : [];
  const catalogSet = new Set(catalogIds);
  const rates = state.models?.rates ?? {};

  // GPT 系之外不上网格（Codex 桌面端只认 gpt- 开头的模型）；目录里已有的
  // 非 GPT 条目仍占一张卡（标「Codex 不支持」）—— 看得见才删得掉
  const available = (state.models?.models ?? []).filter(isGptModel);
  const availableSet = new Set(available);
  const universe = [...new Set([...available, ...catalogIds])];
  const raw = state.codexFilter;
  const query = raw.trim();
  const needle = query.toLowerCase();
  const shown = needle ? universe.filter((id) => id.toLowerCase().includes(needle)) : universe;
  const checked = new Set(state.codexChecked ?? catalogIds);
  const dirty =
    [...checked].some((id) => !catalogSet.has(id)) || catalogIds.some((id) => !checked.has(id));
  // Codex /model 里用户当前的选择（目录是我们的才上报），并入计数一行
  const current = scan.catalogState === 'ours' ? scan.activeModel : null;

  // 还没有模型清单：与 WorkBuddy 同款 —— 只有标题的卡头 + 引导去同步；
  // 清单里有模型但全是非 GPT 系时，说明是过滤后为空而不是没同步过
  if (!universe.length) {
    const hasModels = (state.models?.models ?? []).length > 0;
    return `
      <div class="card">
        <div class="card-head"><span class="card-title">Codex 模型</span></div>
        ${emptyState(
          hasModels ? '清单里没有 GPT 系模型' : '还没有可用模型清单',
          hasModels ? 'Codex 桌面端只支持 gpt- 开头的模型' : '先到「可用模型」分区从上游同步',
        )}
      </div>`;
  }

  const clear = raw
    ? `<button class="filter-clear" type="button" data-action="clear-codex-filter"` +
      ` data-refocus="codex-filter" aria-label="清空搜索" title="清空搜索">×</button>`
    : '';

  return `
    <div class="card">
      <div class="card-head">
        <span class="card-title">Codex 模型</span>
        <span class="card-head-spacer"></span>
        <span class="card-sub">${countLabel(universe.length, shown.length, query, catalogIds.length, current)}</span>
        <button class="small" type="button" data-action="codex-reset"` +
        `${dirty ? '' : ' disabled'} title="放弃未应用的勾选，恢复当前配置">恢复</button>
        ${imported
          ? `<button class="small" type="button" data-action="codex-remove"` +
            `${actionAttrs(state, ACTION.codexRemove)}>清空</button>`
          : ''}
        <button class="small primary" type="button" data-action="codex-apply"` +
        `${!dirty && (checked.size === 0 || (imported && keyOk && active)) ? ' disabled' : ''}` +
        `${!dirty && checked.size === 0 ? ' title="先勾选要接入的模型"' : ''}${actionAttrs(state, ACTION.codexApply)}>应用</button>
      </div>
      ${foreign
        ? note(
            '模型目录由其他工具接管（<code>model_catalog_json</code> 指向别的文件），本应用不会改动它；如需在这里挑选模型，先在 Codex 配置里移除那条指针。',
            'warn',
          )
        : `
      <div class="filter-bar">
        ${renderEnglishField({
          id: 'codex-filter',
          value: raw,
          placeholder: '搜索模型名',
          className: 'filter-input',
        })}
        ${clear}
      </div>
      <div class="model-card-list">
        ${shown.length
          ? shown.map((model) =>
              codexModelCard(
                model,
                checked.has(model),
                availableSet.has(model),
                isGptModel(model),
                rates[model],
              ),
            ).join('')
          : `<div class="model-card-empty">没有匹配「${esc(query)}」的模型，换个关键词试试</div>`}
      </div>`}
    </div>`;
}

/** 一张模型卡：整卡一颗按钮，选中就是整卡的主色描边 + 底色（与 WorkBuddy 同款）。
 *  非 GPT 的目录残留单独说明 —— 不是清单里没有，而是 Codex 不支持。 */
function codexModelCard(model: string, checked: boolean, known: boolean, gpt: boolean, rate: number | undefined): string {
  // 目录里有、网格可选清单里没有：多半是模型从清单移除后的残留，看得见才删得掉
  const pill = checked && !known ? tonePill('off', gpt ? '不在模型列表' : 'Codex 不支持') : '';
  return (
    `<button class="model-card${checked ? ' is-checked' : ''}" type="button"` +
    ` data-action="codex-toggle" data-model="${esc(model)}"` +
    ` aria-pressed="${checked}">` +
    `<span class="model-card-name" title="${esc(model)}">${esc(model)}</span>` +
    rateLabel(rate) +
    pill +
    `</button>`
  );
}

/**
 * 模型卡片网格：可用模型 ∪ 已接入条目，一张卡一个模型，勾选即「在不在 WorkBuddy 里」。
 *
 * 已接入但不在可用模型清单里的条目也占一张卡（标「不在模型列表」）—— 多半是模型从
 * 清单移除后的残留，看得见才删得掉。
 */
function modelsCard(state: AppState): string {
  const entries = state.workbuddyScan?.entries ?? [];
  const importedIds = [...new Set(entries.map((entry) => entry.id))];
  const importedSet = new Set(importedIds);
  // 同一 id 有多条时按最坏情况算：有一条密钥不一致就按不一致提示
  const keyOk = new Map<string, boolean>();
  for (const entry of entries) {
    keyOk.set(entry.id, (keyOk.get(entry.id) ?? true) && entry.keyMatches);
  }
  const available = state.models?.models ?? [];
  const availableSet = new Set(available);
  const universe = [...new Set([...available, ...importedIds])];

  if (!universe.length) {
    return `
      <div class="card">
        <div class="card-head"><span class="card-title">WorkBuddy 模型</span></div>
        ${emptyState('还没有可用模型清单', '先到「可用模型」分区从上游同步')}
      </div>`;
  }

  const raw = state.workbuddyFilter;
  const query = raw.trim();
  const needle = query.toLowerCase();
  const shown = needle ? universe.filter((id) => id.toLowerCase().includes(needle)) : universe;

  const checked = new Set(state.workbuddyChecked ?? importedIds);
  const dirty =
    [...checked].some((id) => !importedSet.has(id)) ||
    importedIds.some((id) => !checked.has(id));
  const clear = raw
    ? `<button class="filter-clear" type="button" data-action="clear-workbuddy-filter"` +
      ` data-refocus="workbuddy-filter" aria-label="清空搜索" title="清空搜索">×</button>`
    : '';

  return `
    <div class="card">
      <div class="card-head">
        <span class="card-title">WorkBuddy 模型</span>
        <span class="card-head-spacer"></span>
        <span class="card-sub">${countLabel(universe.length, shown.length, query, importedIds.length)}</span>
        <button class="small" type="button" data-action="workbuddy-reset"` +
        `${dirty ? '' : ' disabled'} title="放弃未应用的勾选，恢复当前配置">恢复</button>
        ${importedIds.length
          ? `<button class="small" type="button" data-action="workbuddy-clear"` +
            `${actionAttrs(state, ACTION.workbuddyClear)}>清空</button>`
          : ''}
        <button class="small primary" type="button" data-action="workbuddy-apply"` +
        `${dirty ? '' : ' disabled'}${actionAttrs(state, ACTION.workbuddyApply)}>应用</button>
      </div>
      <div class="filter-bar">
        ${renderEnglishField({
          id: 'workbuddy-filter',
          value: raw,
          placeholder: '搜索模型名',
          className: 'filter-input',
        })}
        ${clear}
      </div>
      <div class="model-card-list">
        ${cardGrid(shown, checked, keyOk, availableSet, query, state.models?.rates ?? {})}
      </div>
      <div class="wb-hint">勾选写入、取消勾选移除，点「应用」一次生效；「清空」一次移除全部指向本应用的条目，取消全部勾选再「应用」效果相同；「恢复」放弃未应用的改动。写入使用当前代理地址与密钥，WorkBuddy 侧已有条目只更新密钥，其余设置不动</div>
    </div>`;
}

function cardGrid(
  shown: string[],
  checked: Set<string>,
  keyOk: Map<string, boolean>,
  availableSet: Set<string>,
  query: string,
  rates: Record<string, number>,
): string {
  if (!shown.length) {
    return `<div class="model-card-empty">没有匹配「${esc(query)}」的模型，换个关键词试试</div>`;
  }
  return shown
    .map((model) =>
      modelCard(model, checked.has(model), keyOk.get(model) ?? true, availableSet.has(model), rates[model]),
    )
    .join('');
}

/** 一张模型卡：整卡一颗按钮，选中就是整卡的主色描边 + 底色，不再画勾选框。 */
function modelCard(model: string, checked: boolean, keyOk: boolean, known: boolean, rate: number | undefined): string {
  // 徽标只挑最要紧的一件事说：密钥不一致是接线错误，优先于「不在模型列表」的提醒
  const pill = !checked
    ? ''
    : !keyOk
      ? statusPill({ tone: 'warn', label: '密钥不一致', detail: '' })
      : known
        ? ''
        : tonePill('off', '不在模型列表');
  return (
    `<button class="model-card${checked ? ' is-checked' : ''}" type="button"` +
    ` data-action="workbuddy-toggle" data-model="${esc(model)}"` +
    ` aria-pressed="${checked}">` +
    `<span class="model-card-name" title="${esc(model)}">${esc(model)}</span>` +
    rateLabel(rate) +
    pill +
    `</button>`
  );
}

/**
 * 卡头的计数：没搜索时是总数，搜索时是「命中 / 总数」；两种都带上已接入数。
 * Codex 的目录是我们的才带上 Codex 当前选中的模型（与 WorkBuddy 共用，多传这一个参数）。
 */
function countLabel(
  total: number,
  matched: number,
  query: string,
  imported: number,
  current?: string | null,
): string {
  const base = query ? `${matched} / ${total}` : `共 ${total} 个`;
  const withImported = `${base} · 已接入 ${imported}`;
  return current ? `${withImported} · 当前 ${current}` : withImported;
}

/**
 * Codex 重启确认弹窗。Codex 只在启动时读一次配置，应用 / 清空后的变更要重启
 * 才生效 —— 弹窗只问「现在重启吗」，按变更类型（写入 / 清空）区分说明。
 * 「稍后」只是关掉弹窗，配置不回退；点遮罩不关（只认按钮），避免误触丢掉提醒。
 */
export function codexRestartModal(state: AppState): string {
  if (!state.codexRestartPrompt) return '';
  const text =
    state.codexRestartPrompt === 'applied'
      ? 'Codex 的模型配置已更新，它只在启动时读取配置。强制重启会直接结束 ChatGPT（Codex）的进程并重新拉起，未保存的会话会丢失。'
      : '本应用的 Codex 配置已移除，它只在启动时读取配置。强制重启会直接结束 ChatGPT（Codex）的进程并重新拉起，让它回到默认模型；未保存的会话会丢失。';
  return `
    <div class="modal-backdrop">
      <div class="modal-card" role="alertdialog" aria-modal="true" aria-label="重启 Codex 应用">
        <div class="modal-title">重启 Codex 应用</div>
        <p class="modal-text">${text}</p>
        <div class="modal-actions">
          <button class="small" type="button" data-action="codex-restart-dismiss">稍后</button>
          <button class="small primary" type="button" data-action="codex-restart-confirm">重启</button>
        </div>
      </div>
    </div>`;
}
