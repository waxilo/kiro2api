// 账户额度（概览弹窗）：把「还剩多少」做成一条可读的用量条。
//
// 只用数字说明用量时，用户必须心算 `currentUsage / usageLimit`；给一条带刻度的条，
// 一眼就知道是不是快用完了。颜色在 75% / 90% 两档变调，对应「该留意了」与「要断了」。
//
// 它现在是概览页「额度已用」那块指标卡点开的弹窗：概览上的百分比是结论，
// 这条用量条与订阅信息是它的展开 —— 点开看细节，关掉就回到原来那一屏。
import { esc, localTime, money2, percent4 } from '../../core/format';
import type { UsageInfo } from '../../core/types';
import { ACTION } from '../../state/actions';
import { isPending } from '../../state/app-state';
import { ICON } from '../shared/icons';
import { actionAttrs, emptyState, kv, kvList, swapLabel } from '../shared/render';
import type { ModalSpec } from './types';

type MeterTone = 'ok' | 'warn' | 'err';

function meterTone(percentUsed: number): MeterTone {
  if (percentUsed >= 90) return 'err';
  if (percentUsed >= 75) return 'warn';
  return 'ok';
}

function renderMeter(usage: UsageInfo): string {
  const percent = Math.min(100, Math.max(0, usage.percentUsed));
  const tone = meterTone(percent);
  const money = (value: number): string =>
    `${value}${usage.currency ? ` ${esc(usage.currency)}` : ''}`;

  return `
    <div class="meter is-${tone}">
      <div class="meter-head">
        <span class="meter-usage">
          已用 <strong>${esc(money(usage.currentUsage))}</strong> / ${esc(money(usage.usageLimit))}
        </span>
        <span class="meter-percent">${percent4(usage.percentUsed)}%</span>
      </div>
      <div class="meter-track">
        <div class="meter-fill" style="width:${percent}%"></div>
      </div>
      <div class="meter-foot">
        <span>剩余 ${esc(money2(usage.remaining))}</span>
        ${
          usage.nextResetAt
            ? `<span>下次重置 ${esc(localTime(usage.nextResetAt))}${
                usage.daysUntilReset != null ? `（${usage.daysUntilReset} 天）` : ''
              }</span>`
            : ''
        }
      </div>
    </div>`;
}

export const usageModal: ModalSpec = {
  id: 'usage',
  title: '账户额度',
  desc: '来自 Kiro getUsageLimits，反映当前订阅的用量与剩余',
  icon: ICON.usage,
  // 只有一条用量条 + 两行订阅信息：定高 380 就够，「还没查询过」的空态也落在同一块
  // 尺寸里 —— 点「查询额度」不会让弹窗从矮变高。
  size: 'compact',

  head: (state) => {
    const pending = isPending(state, ACTION.queryUsage);
    // 与「可用模型」同理：「查询额度」四字 ↔「查询中…」四字，但省略号与字距让总宽不同，
    // 右上角那两颗控件会跟着挪。两段都渲染、只显示当前的，宽度就钉住了。
    return `<button class="small" type="button" data-action="usage"${actionAttrs(state, ACTION.queryUsage)}>${swapLabel(
      pending ? '查询中…' : '查询额度',
      pending ? '查询额度' : '查询中…',
    )}</button>`;
  },

  body: (state) => {
    const usage = state.usage;

    const details = usage
      ? [
          usage.subscriptionTitle ? kv('订阅', esc(usage.subscriptionTitle)) : '',
          usage.email ? kv('账户', esc(usage.email)) : '',
          usage.overageStatus ? kv('超额状态', esc(usage.overageStatus)) : '',
        ]
          .filter(Boolean)
          .join('')
      : '';

    const main = usage
      ? `${renderMeter(usage)}${details ? kvList([details]) : ''}`
      : emptyState('还没有查询过', '点击右上角「查询额度」读取当前账户的用量与剩余');

    // 「上次是什么时候查的」放在脚注里而不是标题行：它是给这条数字的可信度做注脚的，
    // 不是用户来这一趟要看的东西。
    const fetched = usage?.fetchedAt ? `上次查询：${esc(localTime(usage.fetchedAt))}。` : '';

    return `
      ${main}
      <div class="modal-foot">
        ${fetched}额度会在应用启动时自动刷新一次，结果本地缓存 —— 重新打开应用时这里直接就是
        上次的值，稍后自己更新。手动点击只是立刻再向上游查一次。
      </div>`;
  },
};
