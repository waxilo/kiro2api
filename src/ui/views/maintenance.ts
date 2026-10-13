// 维护：本地目录入口 + 有破坏性的操作。
//
// 分区依据是「会不会改变现有状态」：打开目录只读，放上边；
// 重置密钥、退出应用会打断正在使用的客户端，单独放进危险区并在按钮上直接标红，
// 而不是混在一排同款按钮里等人点错。
import { esc } from '../../core/format';
import type { RevealTarget } from '../../core/types';
import { ACTION } from '../../state/actions';
import { ICON } from '../shared/icons';
import { busyAttr, note } from '../shared/render';
import type { ViewSpec } from './types';

function dirTile(label: string, path: string, target: RevealTarget): string {
  return `
    <button class="dir-tile" data-action="reveal" data-path="${target}" type="button">
      <span class="dir-icon">${ICON.folder}</span>
      <span class="dir-text">
        <span class="dir-label">${esc(label)}</span>
        <span class="dir-path">${esc(path)}</span>
      </span>
      <span class="dir-open">${ICON.open}</span>
    </button>`;
}

export const maintenanceView: ViewSpec = {
  id: 'maintenance',
  label: '维护',
  title: '维护',
  desc: '打开本地目录、重置密钥、退出应用',

  head: () => '',

  body: (state) => {
    const status = state.status;
    const running = Boolean(status?.running);

    const directories = `
      <div class="card">
        <div class="card-head">
          <span class="card-title">本地目录</span>
          <span class="card-sub">点击在文件管理器中打开</span>
        </div>
        <div class="dir-grid">
          ${dirTile('配置目录', status?.configDir || '—', 'config')}
          ${dirTile('Kiro 凭据目录', '~/.aws/sso/cache', 'sso')}
          ${dirTile('诊断日志', status?.debugLogPath || '—', 'log')}
        </div>
      </div>`;

    const danger = `
      <div class="card card-danger">
        <div class="card-head">
          <span class="card-title">危险操作</span>
        </div>
        <div class="btn-row">
          <button class="danger" data-action="reset-key"
                  ${running ? 'disabled' : busyAttr(state.busy)}>
            ${state.busyLabel === ACTION.resetKey ? '重置中…' : '重置代理密钥'}
          </button>
          <button class="danger" data-action="quit">退出应用</button>
        </div>
        ${
          running
            ? note('重置密钥需要先停止代理，避免正在使用的客户端突然失配。', 'warn')
            : note('重置后旧密钥立即失效，已配置的客户端需要重新粘贴。')
        }
      </div>`;

    return `${directories}${danger}`;
  },
};
