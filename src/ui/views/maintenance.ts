// 维护：本地目录入口 + 退出应用。
//
// 分区依据是「会不会改变现有状态」：打开目录只读，放上边；
// 退出应用会打断正在使用的客户端，单独放进危险区并在按钮上标红，
// 而不是混在一排同款按钮里等人点错。
//
// 重置密钥曾经在这里，现在搬到了「双协议接入」—— 它影响的是那一页给出的地址与密钥，
// 放在被影响的东西旁边，用户重新拿一份配置就不用跨分区跑。
import { esc } from '../../core/format';
import type { RevealTarget } from '../../core/types';
import { ICON } from '../shared/icons';
import { note } from '../shared/render';
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
  desc: '打开本地目录、退出应用',

  head: () => '',

  body: (state) => {
    const status = state.status;

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
          <span class="card-title">退出应用</span>
        </div>
        <div class="btn-row">
          <button class="danger" data-action="quit">退出应用</button>
        </div>
        ${note('退出会同时停止代理，正在使用它的客户端会立即断开。关闭窗口只是隐藏到托盘，代理继续服务。')}
      </div>`;

    return `${directories}${danger}`;
  },
};
