// 领域模型与跨层共享类型。
//
// 约定：这里的字段名与 Rust 侧 `serde(rename_all = "camelCase")` 一一对应，
// 改动任何一处都要同步 `src-tauri` 的同名结构体，否则界面会静默显示 `undefined`。

/** `k2a_status` 的返回。 */
export interface ProxyStatus {
  running: boolean;
  available: boolean;
  port: number | null;
  hasApiKey: boolean;
  authSource: string;
  expiresAt: string | null;
  profileArn: string | null;
  configDir: string;
  debugLogPath: string;
}

/** `k2a_models` 的返回。 */
export interface ModelsState {
  /**
   * 账户可用的模型 ID（上一次同步的结果，落盘缓存）。
   *
   * 这里**没有**「当前选中的模型」：本应用是代理服务，具体用哪个模型由客户端在
   * 每次请求里决定，应用不替用户挑。
   */
  models: string[];
}

/** `k2a_access` 的返回。 */
export interface ProxyAccess {
  running: boolean;
  port: number | null;
  anthropicBaseUrl: string;
  openaiBaseUrl: string;
  apiKeyMasked: string;
  hasApiKey: boolean;
}

/** `k2a_usage` 的返回。 */
export interface UsageInfo {
  subscriptionTitle: string | null;
  subscriptionType: string | null;
  currentUsage: number;
  usageLimit: number;
  remaining: number;
  percentUsed: number;
  nextResetAt: string | null;
  daysUntilReset: number | null;
  overageStatus: string | null;
  currency: string | null;
  email: string | null;
}

/** 复制类型：与 Rust 侧 `format_access_text` 支持的种类保持一致。 */
export type CopyKind =
  | 'anthropic_base_url'
  | 'openai_base_url'
  | 'api_key'
  | 'anthropic_env'
  | 'anthropic_json'
  | 'openai_env'
  | 'openai_json';

/** 可被「打开」的本地目录 / 文件（对应 `k2a_reveal` 的 path 参数）。 */
export type RevealTarget = 'config' | 'log' | 'sso';

/** 应用内更新的阶段。 */
export type UpdatePhase = 'idle' | 'checking' | 'downloading' | 'error';

/** 「应用更新」分区里的一个下载源选项（对应 Rust 侧那份源清单）。 */
export interface UpdateSourceOption {
  id: string;
  label: string;
  /** 下拉框下面那句补充说明。 */
  hint: string;
}

/**
 * 最近一次检查更新实际走的下载源。
 *
 * 存在的理由只有一个：用户固定了某个镜像、它却不可用时要**如实说出来**，
 * 而不是让他以为自己在用首选源。
 */
export interface UpdateSourcePick {
  sourceId: string;
  label: string;
  /** 首选源没能用上（选了固定镜像却回退了）。 */
  fallback: boolean;
}

/** `k2a_update_source_get` 的返回。 */
export interface UpdateSourceState {
  /** 当前选择（`auto` / `direct` / 某个镜像 id）。 */
  current: string;
  options: UpdateSourceOption[];
  /** 还没检查过时为 null。 */
  last: UpdateSourcePick | null;
}

/** 应用内更新的完整状态（由 `services/updater` 写入 store）。 */
export interface UpdateState {
  currentVersion: string;
  latestVersion: string;
  available: boolean;
  phase: UpdatePhase;
  statusText: string | null;
  error: string | null;
  notes: string | null;
  downloaded: number;
  total: number;
  /** 下载源：当前选择、可选项、本次实际走的源。读取失败时为 null。 */
  source: UpdateSourceState | null;
}
