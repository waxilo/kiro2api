// 应用更新：版本对照 + 进度 + 发布说明 + 下载源 + 两个动作。
//
// 发布说明的 Markdown 只做「标题 / 列表」两种结构的轻量格式化 —— 为此引入一个渲染库
// 不划算，而且任何解析器都是新的 XSS 面；其余内容一律转义后当纯文本输出。
import { esc, megabytes, percent } from '../../core/format';
import type { UpdateSourceState } from '../../core/types';
import type { AppState, Tone } from '../../state/app-state';
import { kv, kvList, note, tonePill } from '../shared/render';
import type { ViewSpec } from './types';

function summary(state: AppState): { tone: Tone; label: string } {
  const update = state.update;
  if (update.phase === 'checking') return { tone: 'off', label: '检查中' };
  // 检查失败不能报「已是最新」：那是「问过了、没有问题」，而这里根本没问成。
  // 区块里那两条同义提示已经移除，这颗 pill 现在是唯一的状态出口，必须说准。
  if (update.phase === 'error') return { tone: 'err', label: '检查失败' };
  if (update.available) return { tone: 'warn', label: '有新版本' };
  return { tone: 'ok', label: '已是最新' };
}

function renderNotes(raw: string): string {
  const lines = raw.split('\n').map((line) => line.trimEnd());
  return lines
    .map((line) => {
      const heading = line.match(/^#{1,6}\s+(.*)$/);
      if (heading) return `<div class="notes-heading">${esc(heading[1])}</div>`;
      const bullet = line.match(/^\s*[-*]\s+(.*)$/);
      if (bullet) return `<div class="notes-item">${esc(bullet[1])}</div>`;
      return line.trim() ? `<div>${esc(line)}</div>` : '';
    })
    .join('');
}

/** 当前选择对应的说明；选项清单还没读到时为 null。 */
function currentHint(source: UpdateSourceState): string | null {
  return source.options.find((option) => option.id === source.current)?.hint ?? null;
}

/**
 * 「本次使用」那一行的值。
 *
 * 这一行的意义全在 `fallback` 上：用户固定了某个镜像，它却不可用时，界面必须说出来 ——
 * 否则下拉框写着「ghfast.top」、实际走的是另一个镜像，用户会以为自己选的没生效。
 */
function lastUsed(source: UpdateSourceState): string {
  const last = source.last;
  if (!last) return '<span class="dim">还没检查过</span>';
  if (!last.fallback) return `<code>${esc(last.label)}</code>`;
  const preferred = source.options.find((option) => option.id === source.current)?.label;
  return (
    `<code>${esc(last.label)}</code>` +
    `<span class="dim">（${
      preferred ? `首选 ${esc(preferred)} 不可用，已自动回退` : '已自动回退'
    }）</span>`
  );
}

/**
 * 下载源卡片。
 *
 * 国内直连 GitHub Releases 经常超时，清单代理因此默认会「探测最快的镜像」；
 * 但探测是在几个反代之间挑，用户自己知道哪个线路好 —— 给他一个明确的开关，
 * 比让他反复点「重新检查」碰运气靠谱。
 *
 * 还没读到选项时整块不渲染（首屏那一瞬）：摆一个空下拉框只会让人以为坏了。
 */
function sourceCard(state: AppState, busy: boolean): string {
  const source = state.update.source;
  if (!source) return '';

  const options = source.options
    .map(
      (option) =>
        `<option value="${esc(option.id)}"${option.id === source.current ? ' selected' : ''}>` +
        `${esc(option.label)}</option>`,
    )
    .join('');
  const hint = currentHint(source);

  return `
    <div class="card">
      <div class="card-head">
        <span class="card-title">下载源</span>
        <span class="card-sub">更新包从哪条线路下载</span>
      </div>
      <div class="field-row">
        <label class="field field-inline">
          <span>下载源</span>
          <select id="update-source-input" ${busy ? 'disabled' : ''}>${options}</select>
        </label>
      </div>
      ${kvList([kv('本次使用', lastUsed(source))])}
      ${hint ? note(esc(hint)) : ''}
    </div>`;
}

export const updatesView: ViewSpec = {
  id: 'updates',
  label: '应用更新',
  title: '应用更新',
  desc: '检查更新通道并就地下载安装；安装完成后应用会自动重启',

  head: () => '',

  body: (state) => {
    const update = state.update;
    const pill = summary(state);
    const progress = percent(update.downloaded, update.total);
    const downloading = update.phase === 'downloading';
    const busy = update.phase === 'checking' || downloading;

    // 结果区只放「pill 说不了的事」：进度、错误、发布说明。
    // 「正在检查」「已是最新」这两个结论卡头那颗 pill 已经写了，区内再来一条
    // 同义提示是同一句话说两遍 —— 用户读到的是噪音，不是信息。
    const statusNote = update.statusText ? note(esc(update.statusText)) : '';
    const errorNote = update.error ? note(esc(update.error), 'err') : '';
    const notesBlock =
      update.available && update.notes
        ? `<div class="notes">${renderNotes(update.notes)}</div>`
        : '';
    const progressBlock = downloading
      ? `
        <div class="progress"><div class="progress-bar" style="width:${progress}%"></div></div>
        <div class="card-sub progress-caption">
          已下载 ${progress}%${
            update.total > 0
              ? `（${megabytes(update.downloaded)} / ${megabytes(update.total)} MB）`
              : ''
          }
        </div>`
      : '';
    // 无话可说时整块不渲染：定高区留着就是一片没有解释的空白（卡头已经说明了状态）。
    // 一旦有内容，区仍按 128px 定高 —— 下载中 / 安装中 / 结果之间互相切换依旧不跳。
    const stageItems = [statusNote, progressBlock, errorNote, notesBlock];
    const stage = stageItems.some(Boolean)
      ? `<div class="update-stage">${stageItems.join('')}</div>`
      : '';

    return `
      <div class="card">
        <div class="card-head">
          <span class="card-title">版本</span>
          ${tonePill(pill.tone, pill.label)}
          <span class="card-head-spacer"></span>
        </div>
        ${kvList([
          kv('当前版本', `<code>${esc(update.currentVersion || '—')}</code>`),
          kv('最新版本', `<code>${esc(update.latestVersion || '—')}</code>`),
        ])}
        ${stage}
        <div class="btn-row card-actions">
          <button data-action="update-check" ${busy ? 'disabled' : ''}>重新检查</button>
          <button class="primary" data-action="update-install"
                  ${update.available && !downloading ? '' : ' disabled'}>下载并安装</button>
        </div>
      </div>
      ${sourceCard(state, busy)}`;
  },
};
