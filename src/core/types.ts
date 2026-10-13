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
  autoStart: boolean;
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
}
