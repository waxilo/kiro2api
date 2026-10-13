// 动作编排：把「用户点了什么」翻译成「调哪些后端命令、状态怎么变、提示什么」。
//
// 分层约定（这是本次重构的核心约束之一）：
//   * 本文件**不碰 DOM**，不 import 任何 `ui/` 下的东西 —— 它只读写 store、调后端、发提示。
//   * 提示走 `useFeedback()` 注入的端口，而不是直接 import 一个 toast 实现。
//     这样「状态 → 表现」是单向的：状态层换一个宿主（测试、CLI）也不需要 DOM。
//   * 渲染完全不在这里发生：写入 store 后由外壳单点订阅触发重画。
import { listen } from '@tauri-apps/api/event';

import { errorText } from '../core/format';
import { backend } from '../core/ipc';
import type { CopyKind, RevealTarget } from '../core/types';
import { updater } from '../services/updater';
import { store, type ViewId } from './app-state';

/** 表现层向状态层提供的反馈端口。 */
export interface Feedback {
  toast(message: string, kind?: 'info' | 'error'): void;
}

// 默认实现是空操作：动作在挂载反馈之前被调用时静默丢弃提示，而不是抛错。
let feedback: Feedback = { toast: () => undefined };

export function useFeedback(next: Feedback): void {
  feedback = next;
}

/**
 * 动作名。集中在这里，是因为视图也要用它们 —— 按钮在「刷新凭据」进行中要写成
 * 「刷新中…」，它得知道正在跑的是不是自己那一个动作。两边各写中文字面量迟早会对不上。
 */
export const ACTION = {
  startProxy: '启动代理',
  stopProxy: '停止代理',
  refreshToken: '刷新凭据',
  syncModels: '同步模型',
  resetKey: '重置密钥',
  savePort: '保存端口',
  saveAutoStart: '保存自动启动',
  saveDefaultModel: '保存默认模型',
} as const;

/** 每个复制目标在提示里的说法。集中在这里，避免散落在分支里对不上。 */
const COPY_LABELS: Record<CopyKind, string> = {
  api_key: '代理密钥',
  model: '模型名',
  anthropic_base_url: 'Anthropic 地址',
  openai_base_url: 'OpenAI 地址',
  anthropic_env: 'Anthropic .env',
  anthropic_json: 'Anthropic JSON',
  openai_env: 'OpenAI .env',
  openai_json: 'OpenAI JSON',
};

/**
 * 单个动作最多占用界面多久。超过就解锁并报错，而不是一直灰着。
 *
 * 后端命令各自都有超时（最长的凭据刷新是 30 秒的 HTTP），所以 45 秒只会在**响应回不来**
 * 时才触发 —— 实测出现过这种情况：后端早就把新凭据写好并 emit 了事件，页面上的 promise
 * 却迟迟不落地（窗口被最小化/遮挡时 Chromium 会节流任务投递）。没有这道闸，一次没落地的
 * invoke 会把 `busy` 永久钉住：所有 busy 门控的按钮一起变灰、再点也被静默吞掉，
 * 只能重启应用。
 *
 * 局限也说清楚：页面处于后台时 `setTimeout` 本身也会被节流，所以「45 秒」在窗口最小化
 * 期间不精确 —— 它是保底闸门，不是精确定时器。窗口重新可见时另有一次状态补偿
 * （见 `ui/shell/shell.ts` 的 visibilitychange）。
 */
const ACTION_TIMEOUT_MS = 45_000;

/** 正在执行的动作名，用于「忙碌时又点了一次」的提示文案。 */
let currentLabel: string | null = null;

// ── 基础 ────────────────────────────────────────────────────────────────

export function navigate(view: ViewId): void {
  store.set({ view });
}

/** 拉取首屏所需的三个状态。失败只记进 `error`，不清空已有数据。 */
export async function refresh(): Promise<void> {
  try {
    const [status, access, models] = await Promise.all([
      backend.status(),
      backend.access(),
      backend.models(),
    ]);
    store.set({ status, access, models, error: null });
  } catch (error) {
    store.set({ error: errorText(error) });
  }
}

/**
 * 执行一个会改配置的动作。
 *
 * `busy` 期间不再静默返回 —— 那样用户会以为「点了没反应」，而真相是上一个动作还没结束。
 * 现在会明确提示在忙什么。
 *
 * 无论成功失败都在最后刷新一次：凭据刷新、模型同步、代理启停都会连带改变多项状态。
 * 唯一例外是调用方已经拿到了完整的权威状态（见 `resync: false`）。
 */
export async function run(
  label: string,
  action: () => Promise<void>,
  options: { resync?: boolean } = {},
): Promise<void> {
  if (store.get().busy) {
    feedback.toast(`正在${currentLabel ?? '处理上一个操作'}，请稍候`, 'info');
    return;
  }

  currentLabel = label;
  store.set({ busy: true, busyLabel: label });

  let timedOut = false;
  const timer = setTimeout(() => {
    timedOut = true;
    store.set({
      busy: false,
      busyLabel: null,
      error: `${label}超时：${ACTION_TIMEOUT_MS / 1000} 秒内没有等到后端响应，已解锁界面，可以直接重试。`,
    });
    feedback.toast(`${label}超时，已解锁界面`, 'error');
  }, ACTION_TIMEOUT_MS);

  try {
    await action();
    if (!timedOut) store.set({ error: null });
  } catch (error) {
    if (!timedOut) {
      store.set({ error: errorText(error) });
      feedback.toast(`${label}失败`, 'error');
    }
  } finally {
    clearTimeout(timer);
    currentLabel = null;
    if (!timedOut) store.set({ busy: false, busyLabel: null });
    // 超时后也要同步一次：命令可能其实已经落地，只是「回话」没回来
    if (options.resync !== false) await refresh();
  }
}

// ── 动作实现 ────────────────────────────────────────────────────────────

async function copyToClipboard(kind: CopyKind): Promise<void> {
  try {
    const copied = await backend.copyAccess(kind);
    feedback.toast(
      copied ? `已复制${COPY_LABELS[kind]}` : '没有可复制的内容',
      copied ? 'info' : 'error',
    );
  } catch (error) {
    feedback.toast(errorText(error), 'error');
  }
}

async function queryUsage(): Promise<void> {
  store.set({ usageLoading: true });
  try {
    store.set({ usage: await backend.usage(), error: null });
  } catch (error) {
    store.set({ error: errorText(error) });
    feedback.toast('额度查询失败', 'error');
  } finally {
    store.set({ usageLoading: false });
  }
}

async function reveal(path: RevealTarget): Promise<void> {
  try {
    await backend.reveal(path);
  } catch (error) {
    feedback.toast(errorText(error), 'error');
  }
}

/**
 * 点击事件的统一分发。
 *
 * 走 `data-action` 属性而不是给每个按钮单独挂监听：视图每次渲染都是整块换 innerHTML，
 * 逐按钮绑监听意味着每渲染一次就要重新绑一遍，漏绑一处就是「按钮没反应」。
 */
export async function dispatch(action: string, dataset: DOMStringMap): Promise<void> {
  switch (action) {
    case 'refresh':
      await refresh();
      return;

    case 'proxy-start': {
      const input = document.getElementById('port-input') as HTMLInputElement | null;
      const port = input ? Number.parseInt(input.value, 10) : Number.NaN;
      await run(ACTION.startProxy, async () => {
        if (Number.isFinite(port)) await backend.setPort(port);
        await backend.start(Number.isFinite(port) ? port : undefined);
      });
      return;
    }

    case 'proxy-stop':
      await run(ACTION.stopProxy, async () => {
        await backend.stop();
      });
      return;

    case 'refresh-token':
      // 后端已经把刷新后的 `ProxyStatus` 作为返回值送回来了，直接写进 store：
      // invoke 一落地，界面上的到期时间就是新的，不用再打一轮 IPC 去重新读。
      // 凭据刷新不会改动 access / models，所以这一次跳过最终的整体同步。
      await run(
        ACTION.refreshToken,
        async () => {
          store.set({ status: await backend.refreshToken() });
          feedback.toast('凭据已刷新');
        },
        { resync: false },
      );
      return;

    case 'sync-models':
      await run(ACTION.syncModels, async () => {
        const models = await backend.syncModels();
        store.set({ models });
        feedback.toast(`已同步 ${models.displayModels.length} 个模型`);
      });
      return;

    case 'copy':
      await copyToClipboard(dataset.kind as CopyKind);
      return;

    case 'usage':
      await queryUsage();
      return;

    case 'reset-key':
      await run(ACTION.resetKey, async () => {
        store.set({ access: await backend.resetKey() });
        feedback.toast('已生成新密钥，旧密钥立即失效');
      });
      return;

    case 'reveal':
      await reveal(dataset.path as RevealTarget);
      return;

    case 'quit':
      await backend.quit();
      return;

    case 'update-check':
      // 唯一入口是「应用更新」分区里的「重新检查」，点它的人本来就在这个分区里，
      // 检查结果直接由该分区就地呈现，不需要再导航一次。
      await updater.check(true);
      return;

    case 'update-install':
      await updater.install();
      return;

    case 'dismiss-error':
      store.set({ error: null });
      return;

    case 'goto': {
      const view = dataset.view as ViewId | undefined;
      if (view) navigate(view);
      return;
    }

    default:
      return;
  }
}

// ── 表单变更 ────────────────────────────────────────────────────────────

export async function savePort(raw: string): Promise<void> {
  const port = Number.parseInt(raw, 10);
  if (!Number.isFinite(port)) {
    feedback.toast('端口需为数字', 'error');
    await refresh();
    return;
  }
  await run(ACTION.savePort, async () => {
    await backend.setPort(port);
    feedback.toast(`端口已设置为 ${port}（下次启动生效）`);
  });
}

export async function saveAutoStart(enabled: boolean): Promise<void> {
  await run(ACTION.saveAutoStart, async () => {
    await backend.setAutoStart(enabled);
    feedback.toast(enabled ? '已开启随应用自动启动' : '已关闭自动启动');
  });
}

export async function saveDefaultModel(model: string): Promise<void> {
  if (!model) return;
  await run(ACTION.saveDefaultModel, async () => {
    store.set({ models: await backend.setDefaultModel(model) });
    feedback.toast(`默认模型：${model}`);
  });
}

// ── 后端事件 ────────────────────────────────────────────────────────────

/**
 * 后端主动推送的事件。
 *
 * 由外壳在挂载时调用一次 —— 事件订阅是窗口级的，重复挂会越积越多，
 * 所以这里不做幂等保护，而是把「只调一次」的责任交给调用方（外壳只挂载一次）。
 * 收到事件后仍然只写 store：重画由订阅者完成，这里不需要知道界面长什么样。
 */
export async function bindBackendEvents(): Promise<void> {
  // 后台凭据刷新完成 / 自动启动完成时刷新界面，让到期时间与状态实时更新
  for (const name of ['proxy://token-refreshed', 'proxy://ready']) {
    await listen(name, () => {
      void refresh();
    });
  }
  await listen<string>('proxy://token-refresh-failed', (event) => {
    store.set({ error: `凭据刷新失败：${event.payload}` });
  });
}
