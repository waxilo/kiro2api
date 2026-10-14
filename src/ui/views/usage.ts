// 账户额度：把「还剩多少」做成一条可读的用量条。
//
// 只用数字说明用量时，用户必须心算 `currentUsage / usageLimit`；给一条带刻度的条，
// 一眼就知道是不是快用完了。颜色在 75% / 90% 两档变调，对应「该留意了」与「要断了」。
import { esc } from '../../core/format';
import type { UsageInfo } from '../../core/types';
import { ACTION } from '../../state/actions';
import { isPending } from '../../state/app-state';
import { actionAttrs, emptyState, kv, kvList, note } from '../shared/render';
import type { ViewSpec } from './types';

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
        <span class="meter-percent">${percent}%</span>
      </div>
      <div class="meter-track">
        <div class="meter-fill" style="width:${percent}%"></div>
      </div>
      <div class="meter-foot">
        <span>剩余 ${esc(usage.remaining)}</span>
        ${
          usage.nextResetAt
            ? `<span>下次重置 ${esc(usage.nextResetAt)}${
                usage.daysUntilReset != null ? `（${usage.daysUntilReset} 天）` : ''
              }</span>`
            : ''
        }
      </div>
    </div>`;
}

export const usageView: ViewSpec = {
  id: 'usage',
  label: '账户额度',
  title: '账户额度',
  desc: '来自 Kiro getUsageLimits，反映当前订阅的用量与剩余',

  head: () => '',

  body: (state) => {
    const usage = state.usage;
    const loading = isPending(state, ACTION.queryUsage);

    const details = usage
      ? [
          usage.subscriptionTitle ? kv('订阅', esc(usage.subscriptionTitle)) : '',
          usage.email ? kv('账户', esc(usage.email)) : '',
          usage.overageStatus ? kv('超额状态', esc(usage.overageStatus)) : '',
        ]
          .filter(Boolean)
          .join('')
      : '';

    return `
      <div class="card">
        <div class="card-head">
          <span class="card-title">用量</span>
          <span class="card-head-spacer"></span>
          <button class="small" data-action="usage"${actionAttrs(state, ACTION.queryUsage)}>
            ${loading ? '查询中…' : '查询额度'}
          </button>
        </div>
        ${
          usage
            ? `${renderMeter(usage)}${details ? kvList([details]) : ''}`
            : emptyState('还没有查询过', '点击「查询额度」读取当前账户的用量与剩余')
        }
      </div>
      ${note(
        '额度会在应用启动时自动刷新一次，结果本地缓存 —— 所以重新打开应用时这里直接就是上次的值，' +
          '稍后自己更新。手动点击只是立刻再向上游查一次。',
      )}`;
  },
};
