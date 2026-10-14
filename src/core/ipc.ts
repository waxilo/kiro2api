// 后端命令的类型化封装 —— 前端访问 Rust 侧的唯一入口。
//
// 这一层只做「名字 → 命令」的映射与类型标注，不含任何业务逻辑与 DOM 操作：
// 界面代码永远不直接写 `invoke('k2a_xxx')`，否则命令改名时会在各处静默失效
// （`invoke` 的字符串参数没有编译期检查）。窗口按钮与贴靠上报的命令名不在这里，
// 它们属于 `ui/shell/titlebar` 的窗口契约。
import { invoke } from '@tauri-apps/api/core';

import type {
  CopyKind,
  ModelsState,
  ProxyAccess,
  ProxyStatus,
  RevealTarget,
  UpdateSourceState,
  UsageInfo,
} from './types';

export const backend = {
  // ── 读取 ──────────────────────────────────────────────────────────────
  status: () => invoke<ProxyStatus>('k2a_status'),
  models: () => invoke<ModelsState>('k2a_models'),
  access: () => invoke<ProxyAccess>('k2a_access'),
  usage: () => invoke<UsageInfo>('k2a_usage'),
  /**
   * 上次查询到的额度（纯本地读取，不发网络请求）。从未查过时为 null。
   *
   * 首屏用它把数值先画出来，随后后端在启动刷新里推来新值 —— 用户看到的是
   * 数字自己更新了一下，而不是一片空白等联网。
   */
  cachedUsage: () => invoke<UsageInfo | null>('k2a_cached_usage'),

  // ── 代理生命周期 ──────────────────────────────────────────────────────
  start: (port?: number) => invoke<ProxyStatus>('k2a_start', { port: port ?? null }),
  stop: () => invoke<ProxyStatus>('k2a_stop'),
  refreshToken: () => invoke<ProxyStatus>('k2a_refresh_token'),

  // ── 模型 ──────────────────────────────────────────────────────────────
  // 只读的账户模型清单，同步走上游控制面，**不需要**代理在运行
  syncModels: () => invoke<ModelsState>('k2a_sync_models'),

  // ── 配置 ──────────────────────────────────────────────────────────────
  setAutoStart: (enabled: boolean) => invoke<boolean>('k2a_set_auto_start', { enabled }),
  setPort: (port: number) => invoke<number>('k2a_set_port', { port }),

  // ── 密钥与复制 ────────────────────────────────────────────────────────
  copyAccess: (kind: CopyKind) => invoke<boolean>('k2a_copy_access', { kind }),
  /**
   * 复制一段任意文本（目前只有「点模型名复制」在用）。
   *
   * 与 `copyAccess` 分开是刻意的：那条路径按 `kind` 在后端组装接入片段、独占明文密钥的读取；
   * 这条只把前端递过来的字符串写进剪贴板，不接受 `kind`，因此也绕不过密钥门控。
   * 两条路都落在后端，是因为 WebView 里的 `navigator.clipboard` 在不同内核 / 权限下
   * 可用性不一致，而后端 `arboard` 这条路已经被接入信息的复制验证过了。
   */
  copyText: (text: string) => invoke<boolean>('k2a_copy_text', { text }),
  resetKey: () => invoke<ProxyAccess>('k2a_reset_key'),

  // ── 更新下载源 ────────────────────────────────────────────────────────
  /**
   * 读当前的下载源设置（纯本地，读配置 + 内存里那条「本次实际使用」记录）。
   */
  updateSource: () => invoke<UpdateSourceState>('k2a_update_source_get'),
  /**
   * 切换更新下载源。后端会校验 id、落盘，并丢掉清单缓存 ——
   * 所以调用方拿到返回值之后应当立刻重新检查一次更新，那时走的才是新线路。
   */
  setUpdateSource: (id: string) => invoke<UpdateSourceState>('k2a_update_source_set', { id }),

  // ── 系统 ──────────────────────────────────────────────────────────────
  reveal: (path: RevealTarget) => invoke<void>('k2a_reveal', { path }),
  quit: () => invoke<void>('k2a_quit_app'),
};

/**
 * 哪些复制类型需要明文密钥。
 *
 * ⚠️ 这是前端决定「按钮是否可点」的唯一判据，必须与 Rust 侧 `kind_needs_key` 完全一致：
 * 一旦漂移就会出现「按钮可点但点了报错」或「按钮禁用但其实能复制」。
 */
const KEY_DEPENDENT_KINDS: ReadonlySet<string> = new Set([
  'api_key',
  'anthropic_env',
  'anthropic_json',
  'openai_env',
  'openai_json',
]);

export function copyKindNeedsKey(kind: CopyKind): boolean {
  return KEY_DEPENDENT_KINDS.has(kind);
}
