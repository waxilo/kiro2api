// 动作编排：把「用户点了什么」翻译成「调哪些后端命令、状态怎么变、提示什么」。
//
// 分层约定（这是本次重构的核心约束之一）：
//   * 本文件**不碰 DOM**，不 import 任何 `ui/` 下的东西 —— 它只读写 store、调后端、发提示。
//   * 提示走 `useFeedback()` 注入的端口，而不是直接 import 一个 toast 实现。
//     这样「状态 → 表现」是单向的：状态层换一个宿主（测试、CLI）也不需要 DOM。
//   * 渲染完全不在这里发生：写入 store 后由外壳单点订阅触发重画。
import { listen } from '@tauri-apps/api/event';

import { englishOnly, errorText } from '../core/format';
import { backend } from '../core/ipc';
import type { CopyKind, ModelsState, RevealTarget, UsageInfo } from '../core/types';
import { updater } from '../services/updater';
import {
  blockingLabel,
  isPending,
  store,
  type PendingAction,
  type ViewId,
} from './app-state';

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
  queryUsage: '查询额度',
  resetKey: '重置密钥',
  savePort: '保存端口',
} as const;

/** 每个复制目标在提示里的说法。集中在这里，避免散落在分支里对不上。 */
const COPY_LABELS: Record<CopyKind, string> = {
  api_key: '代理密钥',
  anthropic_base_url: 'Anthropic 地址',
  openai_base_url: 'OpenAI 地址',
  anthropic_env: 'Anthropic .env',
  anthropic_json: 'Anthropic JSON',
  openai_env: 'OpenAI .env',
  openai_json: 'OpenAI JSON',
};

/**
 * 动作的占用范围。
 *
 * - `global`：会改配置的动作（启停代理、端口、重置密钥、同步模型）。
 *   这类动作同时只允许跑一个 —— 两个都改配置，谁赢取决于执行顺序，不如干脆排队。
 * - `scoped`：只影响自己那一块的动作（刷新凭据、查询额度）。它们不参与全局门控，
 *   可以和别的动作并行；等的是网络，把整页锁住没有任何收益。
 */
type Scope = 'global' | 'scoped';

/**
 * 各动作的占用范围。集中在这里，是因为**视图也要用它** —— 别处有改配置的动作在跑时，
 * 只有同样会改配置的按钮才该被标成「暂时不能做」；只读的那两个照常可用。
 * 让视图各自去记「我这个动作是什么范围」，改一处就会漂一半。
 *
 * 没登记的动作按 `global` 处理：宁可多挡一下，也不要让两个改配置的动作并排跑。
 */
const ACTION_SCOPE: Record<string, Scope> = {
  [ACTION.startProxy]: 'global',
  [ACTION.stopProxy]: 'global',
  [ACTION.syncModels]: 'global',
  [ACTION.resetKey]: 'global',
  [ACTION.savePort]: 'global',
  [ACTION.refreshToken]: 'scoped',
  [ACTION.queryUsage]: 'scoped',
};

/** 这个动作的占用范围。 */
export function actionScope(label: string): Scope {
  return ACTION_SCOPE[label] ?? 'global';
}

/**
 * 单个动作最多占用界面多久。超过就解锁并报错，而不是一直灰着。
 *
 * 后端命令各自都有超时（最长的凭据刷新是 30 秒的 HTTP），所以 45 秒只会在**响应回不来**
 * 时才触发 —— 实测出现过这种情况：后端早就把新凭据写好并 emit 了事件，页面上的 promise
 * 却迟迟不落地（窗口被最小化/遮挡时 Chromium 会节流任务投递）。没有这道闸，一次没落地的
 * invoke 会把 `pending` 永久钉住：所有门控的按钮一起变灰、再点也被静默吞掉，
 * 只能重启应用。
 *
 * 局限也说清楚：页面处于后台时 `setTimeout` 本身也会被节流，所以「45 秒」在窗口最小化
 * 期间不精确 —— 它是保底闸门，不是精确定时器。窗口重新可见时另有一次状态补偿
 * （见 `ui/shell/shell.ts` 的 visibilitychange）。
 */
const ACTION_TIMEOUT_MS = 45_000;

// ── 基础 ────────────────────────────────────────────────────────────────

export function navigate(view: ViewId): void {
  store.set({ view });
}

/**
 * 更新「可用模型」分区的搜索词。
 *
 * 不走 `run()`：它是一次纯本地的重绘触发，没有后端命令、没有耗时、也不该有 `busy` 语义 ——
 * 被上一个动作的忙碌状态挡住、连打字都被吞掉，那才是真的糟。
 *
 * 「只收英文」这条现在**有两道**：搜索框本身是自绘的、不是可编辑元素，输入法压根不会启动
 * （中文候选词无从产生，单个按键在字段模块里就被 `englishOnly` 拒了，见
 * `ui/shared/english-field.ts`）；这里这道兜的是**粘贴**——整段文本是原样交上来的，
 * 由这里一把尺子量完，也由这里负责告诉用户裁掉了什么。
 */
export function setModelFilter(query: string): void {
  const ascii = englishOnly(query);
  // 裁掉过东西就说一句：否则用户看到自己刚贴进去的字凭空消失，只会以为是坏了
  if (ascii !== query) {
    feedback.toast('模型名是英文，搜索框只接受 ASCII 字符');
  }
  store.set({ modelFilter: ascii });
}

/**
 * 拉取首屏所需的本地状态。失败只记进 `error`，不清空已有数据。
 *
 * 四个读取全是**本地**的（不发网络请求）：代理状态、接入信息、模型缓存、额度缓存。
 * 联网的部分（上游同步模型、查额度）由后端在启动后台刷新里做，拿到结果再用事件推过来 ——
 * 这样「打开应用」永远不会卡在网络上，界面先是缓存里的旧值，随后自己更新。
 */
export async function refresh(): Promise<void> {
  try {
    const [status, access, models, cachedUsage] = await Promise.all([
      backend.status(),
      backend.access(),
      backend.models(),
      backend.cachedUsage(),
    ]);
    store.set({
      status,
      access,
      models,
      // 缓存只用来**填空白**，绝不覆盖已经拿到的新值：`refresh()` 会被每个动作在收尾时
      // 调用一次，而它读的是磁盘上那一份（可能比内存里的旧）。用 `??` 而不是直接赋值，
      // 语义就变成「首屏没有值时才拿缓存顶上」，不会把刚查回来的额度倒回旧数。
      usage: store.get().usage ?? cachedUsage,
      error: null,
    });
  } catch (error) {
    store.set({ error: errorText(error) });
  }
}

/**
 * 执行一个动作：登记「它在跑」→ 调后端 → 收尾。
 *
 * 占用范围由**动作名**查表得出（见上面的 `ACTION_SCOPE`），而不是由调用方传进来 ——
 * 视图那边判断「这颗按钮该不该标灰」用的也是同一张表，一处定义两处同源。
 *
 * 同名的动作不会重叠执行 ——「又点了一次」会被明确提示，而不是静默返回：用户以为「点了
 * 没反应」，真相是上一个还在跑。
 *
 * 无论成功失败都在最后刷新一次：凭据刷新、模型同步、代理启停都会连带改变多项状态。
 * 唯一例外是调用方已经拿到了完整的权威状态（见 `resync: false`）。
 */
export async function run(
  label: string,
  action: () => Promise<void>,
  options: { resync?: boolean } = {},
): Promise<void> {
  const { resync = true } = options;
  const blocking = actionScope(label) === 'global';

  const before = store.get();
  // 挡下这一次的两种理由：它自己已经在跑；或者它是改配置的动作、而另一个改配置的动作
  // 正在跑。提示里要说清楚是谁在跑，否则用户只知道「点了没反应」。
  const running = isPending(before, label)
    ? label
    : blocking
      ? blockingLabel(before)
      : null;
  if (running) {
    feedback.toast(`正在${running}，请稍候`, 'info');
    return;
  }

  const task: PendingAction = { label, blocking };
  // 顺手把上一次的错误清掉：它已经以 toast 播过了。不清的话，「同样的错误再来一次」
  // 会因为值没变而被 store 判定为「无变化」、连通知都不发，于是提示一声不响地消失。
  store.set({ pending: [...store.get().pending, task], error: null });

  let settled = false;
  let timedOut = false;
  // 出列只做一次：超时与正常结束都会走到这里。
  const depart = (): void => {
    if (settled) return;
    settled = true;
    store.set({ pending: store.get().pending.filter((item) => item !== task) });
  };

  const timer = setTimeout(() => {
    timedOut = true;
    depart();
    store.set({
      error: `${label}超时：${ACTION_TIMEOUT_MS / 1000} 秒内没有等到后端响应。命令可能仍在后台执行，可以直接重试。`,
    });
  }, ACTION_TIMEOUT_MS);

  try {
    await action();
    if (!timedOut) store.set({ error: null });
  } catch (error) {
    if (!timedOut) {
      // 文案里带上动作名：「重置密钥失败：请先停止代理…」比只有后半截清楚得多 ——
      // 失败现在只有 toast 这一个出口，信息缺一块就没别处可看了。
      store.set({ error: `${label}失败：${errorText(error)}` });
    }
  } finally {
    clearTimeout(timer);
    depart();
    // 超时后也要同步一次：命令可能其实已经落地，只是「回话」没回来
    if (resync) await refresh();
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

/**
 * 复制模型名。
 *
 * 提示里带上名字本身，是因为「已复制」这句话在模型名长得很像的时候帮助有限
 * （`claude-opus-4-6` / `claude-opus-4-7`）—— 把名字念出来，用户能一眼确认没点错。
 */
async function copyModel(model: string): Promise<void> {
  if (!model) return;
  try {
    const copied = await backend.copyText(model);
    feedback.toast(copied ? `已复制模型名 ${model}` : '没有可复制的内容', copied ? 'info' : 'error');
  } catch (error) {
    feedback.toast(errorText(error), 'error');
  }
}

/**
 * 查询账户额度。
 *
 * 走 `run()`，但它在 `ACTION_SCOPE` 里登记为 `scoped`：只影响额度那一块
 * （自己的按钮写「查询中…」），不会把别处的按钮一起灰掉，也不会被别处的忙碌挡住。
 *
 * `resync: false` —— 结果本身就是权威状态，不需要再整体拉一遍本地状态。
 */
async function queryUsage(): Promise<void> {
  await run(
    ACTION.queryUsage,
    async () => {
      store.set({ usage: await backend.usage() });
    },
    { resync: false },
  );
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
      //
      // `scoped`（见 ACTION_SCOPE）：它等的是最长 30 秒的上游 HTTP，只碰凭据文件、
      // 不改配置，因此不占用全局门控 —— 这 30 秒里用户想启动代理、复制地址、查额度都照常。
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
        feedback.toast(
          models.models.length ? `已同步 ${models.models.length} 个模型` : '上游没有返回模型',
          models.models.length ? 'info' : 'error',
        );
      });
      return;

    case 'copy':
      await copyToClipboard(dataset.kind as CopyKind);
      return;

    case 'copy-model':
      await copyModel(dataset.model ?? '');
      return;

    case 'clear-model-filter':
      setModelFilter('');
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

  // 缓存被后台刷新替换：直接把新值写进 store，不重新拉一遍。
  //
  // 这是「持久化 + 静默更新」的后半截：首屏的数字来自本地缓存，这个事件把它换成最新的。
  // 整条链路对用户是无感的 —— 没有加载态、没有闪烁，就是数字自己变了。
  await listen<ModelsState>('proxy://models-updated', (event) => {
    store.set({ models: event.payload });
  });
  await listen<UsageInfo>('proxy://usage-updated', (event) => {
    // 只换数值。后台刷新与「用户手动查一次」是两件事：手动那次有自己的 pending 记录，
    // 由它自己在收尾时出列 —— 这里顺手清掉别人的进行态，会让那颗按钮在查询还没回来时
    // 就先变回可点。
    store.set({ usage: event.payload });
  });
}
