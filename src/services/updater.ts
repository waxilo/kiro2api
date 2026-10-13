// 应用内更新：检查 → 下载（带进度与停滞检测）→ 安装 → 重启。
//
// 后端已把 `plugins.updater.endpoints` 的第一个地址指向本机清单代理（127.0.0.1:47866），
// 该代理会把清单里的下载地址改写为最快的 GitHub 镜像，因此这里的逻辑只管
// 「重试 / 停滞中断 / 安装后重启」三件事，不需要感知镜像细节。
//
// 与旧实现的差别：不再持有 `onChange` 回调、也不再自己拼 HTML，只把 UpdateState 写进 store。
// 这样「更新中」这个状态和别的状态走同一条渲染通路，浮层/状态栏想显示它就伸手去 store 取。
//
// 日志约定：重试 / 停滞 / 兜底重启这些**中间态**只写 `console`，不弹 toast ——
// 它们对用户不可操作（用户能做的只有等），弹出来只是噪声；但排查线上问题时必须留痕。
// 用户可见的失败一律走 `patch({ phase: 'error', error })`，由 UI 呈现。
import { getVersion } from '@tauri-apps/api/app';
import { relaunch } from '@tauri-apps/plugin-process';
import { check, type Update } from '@tauri-apps/plugin-updater';

import { errorText } from '../core/format';
import type { UpdateState } from '../core/types';
import { store } from '../state/app-state';

/** 镜像下载可能在「已结束」后长时间不吐字节 —— 15s 无增长即判定该镜像失效，中断重试。 */
const DOWNLOAD_STALL_MS = 15_000;
const MAX_DOWNLOAD_ATTEMPTS = 3;
/** macOS 上 `install()` 偶发不返回，超时后强制重启。 */
const MAC_INSTALL_STALL_MS = 12_000;

const isMac = /mac/i.test(navigator.platform) || /mac os/i.test(navigator.userAgent);

/** 更新句柄只在有可用更新时存在；失败/超时后置空，下次点击会重新取清单。 */
let handle: Update | null = null;
let inFlight: Promise<void> | null = null;

function patch(next: Partial<UpdateState>): void {
  store.set({ update: { ...store.get().update, ...next } });
}

function current(): UpdateState {
  return store.get().update;
}

/** 单次下载：上报进度；长时间无字节增长则 close() 中断，交给外层换镜像重试。 */
function downloadOnce(update: Update): Promise<void> {
  return new Promise((resolve, reject) => {
    let settled = false;
    let lastProgressAt = Date.now();

    const watchdog = setInterval(() => {
      if (Date.now() - lastProgressAt <= DOWNLOAD_STALL_MS || settled) return;
      clearInterval(watchdog);
      settled = true;
      console.warn('[updater] 下载停滞，中断本次并重试');
      update.close().catch(() => undefined);
      reject(new Error('下载停滞（当前镜像不可用），正在换镜像重试'));
    }, 5000);

    const finish = (fn: () => void): void => {
      clearInterval(watchdog);
      if (settled) return;
      settled = true;
      fn();
    };

    update
      .download(
        (event) => {
          lastProgressAt = Date.now();
          if (event.event === 'Started') {
            patch({ downloaded: 0, total: event.data.contentLength ?? 0 });
          } else if (event.event === 'Progress') {
            patch({ downloaded: current().downloaded + event.data.chunkLength });
          }
          // Finished 不需要处理：进度事件已经逐个上报过，这里再补一次渲染是多余的
        },
        { timeout: 300_000 },
      )
      .then(
        () => finish(resolve),
        (error) => finish(() => reject(error)),
      );
  });
}

async function downloadWithRetry(): Promise<void> {
  for (let attempt = 1; attempt <= MAX_DOWNLOAD_ATTEMPTS; attempt++) {
    if (!handle) throw new Error('未找到可安装的更新包，请重新检查更新');
    try {
      patch({
        statusText:
          attempt > 1
            ? `正在重试下载（第 ${attempt}/${MAX_DOWNLOAD_ATTEMPTS} 次）…`
            : '正在下载更新包…',
        downloaded: 0,
        total: 0,
      });
      await downloadOnce(handle);
      return;
    } catch (error) {
      console.warn(`[updater] 第 ${attempt} 次下载失败:`, error);
      // 释放句柄并重新取清单，让本机清单代理重新探测可用镜像
      await handle.close().catch(() => undefined);
      handle = null;
      if (attempt >= MAX_DOWNLOAD_ATTEMPTS) throw error;
      await updater.check(true);
    }
  }
}

async function installAndRelaunch(): Promise<void> {
  const update = handle;
  if (!update) throw new Error('更新句柄已失效，请重新检查更新');

  let installSettled = false;
  let stallTimer: ReturnType<typeof setTimeout> | null = null;
  if (isMac) {
    // bundle 可能已替换完成，但 install() 偶发不返回；超时后强制重启
    stallTimer = setTimeout(() => {
      if (installSettled) return;
      console.warn('[updater] macOS 安装在预期时间内未返回，强制重启');
      void relaunch().catch(() => undefined);
    }, MAC_INSTALL_STALL_MS);
  }

  try {
    await update.install();
    installSettled = true;
    if (stallTimer) clearTimeout(stallTimer);
    // 重启前释放 updater 资源，避免 macOS 上 close 与 relaunch 竞态
    void update.close().catch(() => undefined);
    handle = null;
    patch({ statusText: '更新已安装，应用即将重启…' });
    await relaunch();
  } finally {
    installSettled = true;
    if (stallTimer) clearTimeout(stallTimer);
  }
}

export const updater = {
  /** 首屏调用：取当前版本并静默检查一次。 */
  async init(): Promise<void> {
    try {
      patch({ currentVersion: await getVersion() });
    } catch {
      /* 取版本失败不阻塞更新检查 */
    }
    void this.check(false);
  },

  get busy(): boolean {
    return current().phase === 'checking' || current().phase === 'downloading';
  },

  async check(force = false): Promise<void> {
    if (inFlight) {
      if (!force) return inFlight;
      await inFlight.catch(() => undefined);
      // 等待期间可能已有新的 force 检查在跑，直接复用，避免并发
      if (inFlight) return inFlight;
    }

    patch({ phase: 'checking', statusText: '正在检查更新…', error: null });

    inFlight = (async () => {
      try {
        // 每个 endpoint 各自套用该超时；本地清单代理 + 3 个镜像最多耗时可接受。
        const update = await check({ timeout: 25_000 });
        handle = update;
        patch({
          available: Boolean(update),
          latestVersion: update?.version ?? current().currentVersion,
          notes: update?.body ?? null,
          phase: 'idle',
          statusText: null,
        });
      } catch (error) {
        const raw = errorText(error);
        patch({
          phase: 'error',
          statusText: null,
          error: /timed?\s*out|timeout|连接超时|error sending request/i.test(raw)
            ? `检查更新超时（无法访问更新通道）。请确认网络或稍后重试。\n${raw}`
            : raw,
        });
      } finally {
        inFlight = null;
      }
    })();

    return inFlight;
  },

  /** 下载并安装。Windows 由安装器接管退出，macOS/Linux 需显式重启。 */
  async install(): Promise<void> {
    if (this.busy) return;

    // 句柄可能在超时/失败后被清空，此时先重新获取，避免点击毫无反应
    if (!handle) {
      await this.check(true);
      if (!handle) {
        patch({ phase: 'error', error: '未找到可安装的更新包，请点击「重新检查」后再试。' });
        return;
      }
    }

    patch({ phase: 'downloading', error: null, statusText: '开始下载更新包…' });

    try {
      await downloadWithRetry();
      patch({ statusText: '下载完成，正在安装…' });
      await installAndRelaunch();
    } catch (error) {
      const latest = current().latestVersion;
      void handle?.close().catch(() => undefined);
      handle = null;
      // 失败后保留「有更新」状态，下次点击会重新获取后再装
      patch({
        phase: 'error',
        statusText: null,
        error: errorText(error),
        available: Boolean(latest),
      });
    }
  },
};
