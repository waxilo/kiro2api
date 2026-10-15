// 三方接入：管理第三方客户端里「指向本应用」的自定义模型。
//
// 这一页只认地址与端口都指向本代理的条目（host+端口匹配在 Rust 侧完成）：
// WorkBuddy 里配给别的服务的模型一概不出现 —— 看不见，也就删不到。
// 扫描在进入分区、焦点回到窗口和每次动作收尾时各刷一次，纯本地读盘，没有加载态可言。
//
// tab 条按浏览器页签的形制设计（在上的横条、激活项与内容区相连），
// 每个客户端一个页签，加新客户端只多一个 tab，不动布局。
//
// 模型区是一张可多选的卡片网格，勾选状态直接表达「在不在 WorkBuddy 里」：
// 勾选 = 写入（用当前地址与密钥），取消勾选 = 移除。勾选不立即写盘 —— 攒到
// 点「应用」一次性落地：批量操作少等多次写盘，误点也可以「恢复」反悔。
import { esc } from '../../core/format';
import { ACTION } from '../../state/actions';
import { type AppState, type ThirdPartyTab } from '../../state/app-state';
import { renderEnglishField } from '../shared/english-field';
import { actionAttrs, emptyState, kv, kvList, mono, note, statusPill, tonePill } from '../shared/render';
import type { ViewSpec } from './types';

interface TabSpec {
  id: ThirdPartyTab;
  label: string;
}

const TABS: TabSpec[] = [
  { id: 'workbuddy', label: 'WorkBuddy' },
  { id: 'codex', label: 'Codex' },
];

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
 * Codex 面板：一键接入卡。
 *
 * Codex 是单供应商 —— 配置就是一对文件（config.toml 的 provider + auth.json 的密钥），
 * 没有 WorkBuddy 那种模型清单可挑，所以不做成网格，而是一张状态卡：
 * 徽标说清现状（未接入 / 已接入 / 密钥不一致 / 已配置未激活），
 * 「接入」一次写齐并激活，「移除」只清掉指向本应用的痕迹。
 */
function codexPanel(state: AppState): string {
  const scan = state.codexScan;
  if (!scan) {
    return `<div class="card">${emptyState('正在读取 Codex 配置')}</div>`;
  }
  if (!scan.installed) {
    return note(
      '未找到 Codex（<code>~/.codex</code> 不存在）。请确认 Codex CLI 已安装并运行过一次，再回到这里接入。',
    );
  }
  const scanError = scan.error ? note(esc(scan.error), 'warn') : '';
  return `${scanError}${codexCard(state)}`;
}

function codexCard(state: AppState): string {
  const entries = state.codexScan?.entries ?? [];
  const imported = entries.length > 0;
  // 单供应商正常只有一条；多条按最坏情况算密钥状态，与 WorkBuddy 面板同一口径
  const keyOk = entries.every((entry) => entry.keyMatches);
  const active = imported && entries.some((entry) => entry.active);

  const pill = !imported
    ? statusPill({ tone: 'off', label: '未接入', detail: '' })
    : !keyOk
      ? statusPill({ tone: 'warn', label: '密钥不一致', detail: '' })
      : active
        ? statusPill({ tone: 'ok', label: '已接入', detail: '' })
        : statusPill({ tone: 'warn', label: '已配置未激活', detail: '' });

  return `
    <div class="card">
      <div class="card-head">
        <span class="card-title">Codex CLI</span>
        <span class="card-head-spacer"></span>
        ${pill}
        ${imported
          ? `<button class="small" type="button" data-action="codex-remove"` +
            `${actionAttrs(state, ACTION.codexRemove)}>移除</button>`
          : ''}
        <button class="small primary" type="button" data-action="codex-apply"` +
        `${imported && keyOk && active ? ' disabled' : ''}${actionAttrs(state, ACTION.codexApply)}>接入</button>
      </div>
      ${kvList([
        kv('接口地址', mono(state.access?.openaiBaseUrl)),
        kv('写入位置', mono('~/.codex/config.toml · auth.json')),
      ])}
      <div class="wb-hint">「接入」把指向本应用的 provider 写入 config.toml 并设为当前，同时把密钥同步进 auth.json；只动与本应用相关的条目，其余配置与 ChatGPT 登录保持原样。模型在 Codex 里用 /model 切换</div>
    </div>`;
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
        ${cardGrid(shown, checked, keyOk, availableSet, query)}
      </div>
      <div class="wb-hint">勾选写入、取消勾选移除，点「应用」一次生效，「恢复」放弃未应用的改动。写入使用当前代理地址与密钥，WorkBuddy 侧已有条目只更新密钥，其余设置不动</div>
    </div>`;
}

function cardGrid(
  shown: string[],
  checked: Set<string>,
  keyOk: Map<string, boolean>,
  availableSet: Set<string>,
  query: string,
): string {
  if (!shown.length) {
    return `<div class="model-card-empty">没有匹配「${esc(query)}」的模型，换个关键词试试</div>`;
  }
  return shown
    .map((model) =>
      modelCard(model, checked.has(model), keyOk.get(model) ?? true, availableSet.has(model)),
    )
    .join('');
}

/** 一张模型卡：整卡一颗按钮，选中就是整卡的主色描边 + 底色，不再画勾选框。 */
function modelCard(model: string, checked: boolean, keyOk: boolean, known: boolean): string {
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
    pill +
    `</button>`
  );
}

/** 卡头的计数：没搜索时是总数，搜索时是「命中 / 总数」；两种都带上已接入数。 */
function countLabel(total: number, matched: number, query: string, imported: number): string {
  const base = query ? `${matched} / ${total}` : `共 ${total} 个`;
  return `${base} · 已接入 ${imported}`;
}
