// 概览弹窗注册表与外壳片段。
//
// 与分区注册表同理：`Record<ModalId, ModalSpec>` 让「加了弹窗 id 却没实现」在编译期就报错。
//
// 外壳负责三件弹窗之间必然一致的事：遮罩、卡片外形、右上角关闭按钮、aria 语义。
// 弹窗自己只提供标题 / 说明 / 动作按钮 / 主体，不各写一遍壳子 —— 否则关闭按钮的位置、
// 宽度、滚动行为迟早各说各话。
import { esc } from '../../core/format';
import type { AppState, ModalId } from '../../state/app-state';
import { ICON } from '../shared/icons';
import { modelsModal } from './models';
import { usageModal } from './usage';
import type { ModalSpec } from './types';

export const MODALS: Record<ModalId, ModalSpec> = {
  models: modelsModal,
  usage: usageModal,
};

export type { ModalSpec };

/**
 * 渲染当前弹窗；没有就返回空串。
 *
 * 几条与交互相关的约定都落在这里：
 * * 遮罩带 `data-modal-dismiss`，点卡片外面关掉（外壳判断点击目标不在 `.modal-card` 里）；
 * * 卡片带 `id="modal-card"` 与 `tabindex="-1"`：打开时外壳把焦点放到它身上，Escape
 *   才有接收者，读屏也会把标题念出来（`aria-labelledby`）；同时这个 id 让外壳的焦点快照
 *   在弹窗内的后台重绘之后还能把焦点放回来；
 * * 关闭按钮是弹窗自己的事，不放给各视图 —— 位置与形状必须一致。
 */
export function renderModal(state: AppState): string {
  const id = state.modal;
  if (!id) return '';
  const spec = MODALS[id];
  const actions = spec.head(state);

  return `
    <div class="modal-backdrop" data-modal-dismiss>
      <div class="modal-card is-wide${spec.size === 'compact' ? ' is-compact' : ''}" id="modal-card" role="dialog" aria-modal="true"
           aria-labelledby="modal-title" tabindex="-1">
        <div class="modal-head">
          <span class="modal-badge">${spec.icon}</span>
          <div class="modal-head-text">
            <h2 class="modal-title" id="modal-title">${esc(spec.title)}</h2>
            <p class="modal-desc">${esc(spec.desc)}</p>
          </div>
          <div class="modal-head-actions">
            ${actions}
            <button class="modal-close" type="button" data-action="close-modal"
                    aria-label="关闭" title="关闭（Esc）">${ICON.close}</button>
          </div>
        </div>
        <div class="modal-body${spec.variant ? ` is-${spec.variant}` : ''}">${spec.body(state)}</div>
      </div>
    </div>`;
}
