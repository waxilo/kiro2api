// 全局后台 job：额度刷新、凭据检查续签、应用更新检查。
//
// 三件事都是「该有人定期做，但不值得用户惦记」的维护动作。已有的机制各管一段：
// 启动刷新（Rust `spawn_startup_refresh`）只在启动时跑一次；凭据刷新循环
// （Rust `spawn_refresh_loop`）只在代理运行期间存在；更新只在 `updater.init()` 查一次。
// 这个 job 补上其余的时间段 —— 应用开着但代理没跑、应用一开一整天的场景。
//
// 全部**静默**：失败只写 console，不弹 toast、不写 store.error。后台维护失败
// 不该每十分钟打断一次用户；状态本身会说话 —— 概览页的到期行会显示「已过期」，
// 「应用更新」分区会显示检查失败，用户真需要处理时自然看得到。
import { backend } from '../core/ipc';
import { isPending, store } from '../state/app-state';
import { ACTION } from '../state/actions';
import { updater } from './updater';

/** 主循环心跳。三个任务各自按「上次执行时间」判断到没到期，tick 只负责挨个问一遍。 */
const TICK_MS = 60_000;
/** 额度刷新周期。上游按订阅周期计算用量，几分钟内的变化没有意义。 */
const USAGE_INTERVAL_MS = 10 * 60_000;
/**
 * 更新检查周期。清单侧有 5 分钟缓存（`MANIFEST_CACHE_TTL`），10 分钟一查最多命中半次缓存，
 * 撞不上 GitHub 限流；换来的是「应用更新」红点不会迟到半天。
 */
const UPDATE_INTERVAL_MS = 10 * 60_000;
/** 剩余寿命短于这个值就主动续签（含已过期）。 */
const CREDENTIAL_LEAD_MS = 10 * 60_000;
/** 续签尝试的冷却期。失败（refreshToken 已失效之类）不该每分钟去敲一次 OIDC。 */
const CREDENTIAL_COOLDOWN_MS = 10 * 60_000;

let timer: ReturnType<typeof setInterval> | null = null;
let inTick = false;
let lastUsageAt = 0;
let lastUpdateAt = 0;
let lastCredentialAt = 0;

export const scheduler = {
  /** 外壳挂载后调用一次。启动刷新与 `updater.init()` 刚推过一轮，计时器从现在起算。 */
  start(): void {
    if (timer !== null) return;
    lastUsageAt = Date.now();
    lastUpdateAt = Date.now();
    timer = setInterval(() => void tick(), TICK_MS);
  },
};

async function tick(): Promise<void> {
  // 上一跳还没回来（网络慢）就跳过这一跳：任务之间没有依赖，晚一个周期无所谓
  if (inTick) return;
  inTick = true;
  try {
    await refreshUsageIfDue();
    await renewCredentialsIfDue();
    await checkUpdateIfDue();
  } finally {
    inTick = false;
  }
}

async function refreshUsageIfDue(): Promise<void> {
  if (Date.now() - lastUsageAt < USAGE_INTERVAL_MS) return;
  lastUsageAt = Date.now();
  try {
    store.set({ usage: await backend.usage() });
  } catch (error) {
    console.warn('[scheduler] 后台额度刷新失败:', error);
  }
}

/**
 * 凭据续签只在**代理停止**时由本 job 负责：代理运行期间 Rust 侧的刷新循环
 * （`spawn_refresh_loop`）会在过期前主动续并广播结果，这里再插手只是白白多打一次 OIDC。
 * 两个刷新同时写凭据文件没有竞态保护，能避开的重复就该避开。
 */
async function renewCredentialsIfDue(): Promise<void> {
  if (Date.now() - lastCredentialAt < CREDENTIAL_COOLDOWN_MS) return;
  const status = store.get().status;
  if (!status?.available || status.running) return;
  // 用户自己点了「刷新凭据」时退让：同一个后端命令并发跑两次没有任何收益
  if (isPending(store.get(), ACTION.refreshToken)) return;

  const expiresAt = status.expiresAt ? new Date(status.expiresAt).getTime() : Number.NaN;
  if (!(Number.isFinite(expiresAt) && expiresAt - Date.now() <= CREDENTIAL_LEAD_MS)) return;

  lastCredentialAt = Date.now();
  try {
    store.set({ status: await backend.refreshToken() });
  } catch (error) {
    console.warn('[scheduler] 后台凭据续签失败:', error);
  }
}

async function checkUpdateIfDue(): Promise<void> {
  if (Date.now() - lastUpdateAt < UPDATE_INTERVAL_MS) return;
  if (updater.busy) return;
  lastUpdateAt = Date.now();
  // check(false) 是静默检查：结果与失败都只落在「应用更新」分区的状态里
  await updater.check(false);
}
