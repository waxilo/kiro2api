// 应用更新：版本对照 + 进度 + 发布说明 + 两个动作。
//
// 发布说明的 Markdown 只做「标题 / 列表」两种结构的轻量格式化 —— 为此引入一个渲染库
// 不划算，而且任何解析器都是新的 XSS 面；其余内容一律转义后当纯文本输出。
import { esc, megabytes, percent } from '../../core/format';
import type { AppState, Tone } from '../../state/app-state';
import { kv, kvList, note, tonePill } from '../shared/render';
import type { ViewSpec } from './types';

function summary(state: AppState): { tone: Tone; label: string } {
  const update = state.update;
  if (update.phase === 'checking') return { tone: 'off', label: '检查中' };
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

    const upToDate =
      !update.available && update.currentVersion && update.latestVersion && !update.error
        ? note('已是最新版本。', 'ok')
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
        ${update.statusText ? note(esc(update.statusText)) : ''}
        ${progressBlock}
        ${upToDate}
        ${update.error ? note(esc(update.error), 'err') : ''}
        ${
          update.available && update.notes
            ? `<div class="notes">${renderNotes(update.notes)}</div>`
            : ''
        }
        <div class="btn-row card-actions">
          <button data-action="update-check" ${busy ? 'disabled' : ''}>重新检查</button>
          ${
            update.available && !downloading
              ? '<button class="primary" data-action="update-install">下载并安装</button>'
              : ''
          }
        </div>
      </div>`;
  },
};
