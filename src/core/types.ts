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
  /** 当前凭据的一句话描述（由账号推导，含到期时间）。 */
  authSource: string;
  expiresAt: string | null;
  profileArn: string | null;
  configDir: string;
  debugLogPath: string;
  /** 当前生效账号的 id（见 `AccountsView`）；没有任何账号时为 null。 */
  activeAccountId: string | null;
  /** 当前账号来源；只有环境变量凭据时是 `env`，都没有是 `none`。 */
  activeAccountSource: AccountSource | 'env' | 'none';
  /** 当前账号展示名（识别到邮箱就是邮箱）。 */
  activeAccountLabel: string | null;
}

/** 账号来源：本应用登录 / 本地 kiro-cli 导入 / Kiro 桌面端共享缓存。 */
export type AccountSource = 'login' | 'cli' | 'desktop';

/**
 * 一个 Kiro 账号。
 *
 * 三个来源的令牌格式完全一致，区别只在「归谁管」：登录与 CLI 是本应用的独立副本，
 * 桌面端就地使用 IDE 的共享缓存。后端不向前端暴露任何令牌内容。
 */
export interface AccountInfo {
  /** 稳定 id（`login:<hash>` / `cli:<hash>` / `desktop:default`），续期后不变。 */
  id: string;
  source: AccountSource;
  /** 展示主标题：识别到邮箱就是邮箱，否则是「Builder ID 账号 · 3f2a」。 */
  label: string;
  email: string | null;
  /** 套餐名（上游 `subscriptionTitle`），识别过才有。 */
  plan: string | null;
  provider: string | null;
  authMethod: string | null;
  region: string | null;
  expiresAt: string | null;
  profileArn: string | null;
  /** 凭据目录（CLI 账号给的是导入副本目录）。 */
  path: string;
  /** 当前代理正在用它。 */
  selected: boolean;
  /** 现在就能用（令牌未过期，或持有可续期的 refreshToken）。 */
  usable: boolean;
  /**
   * 能否在本应用里退出登录。只有「登录的账号」为 true —— 桌面端归 IDE、CLI 归 kiro-cli，
   * 在这边删目录既不会让它们退出，也会被下一次扫描原样扫回来。
   */
  removable: boolean;
  /** 补充说明（如「kiro-cli 中有同一账号」）。 */
  note: string | null;
}

/** `k2a_accounts_list` 的返回。 */
export interface AccountsView {
  accounts: AccountInfo[];
  /** 实际生效的账号 id（没手动选过就是自动挑出来的那个）。 */
  activeId: string | null;
}

/** 设备码登录方式：AWS Builder ID / 企业 IAM Identity Center。 */
export type LoginMethod = 'builder' | 'idc';

/** `k2a_login_start` 的返回：等待用户在浏览器里确认的那次设备授权。 */
export interface LoginSession {
  userCode: string;
  verificationUri: string;
  verificationUriComplete: string;
  /** 验证码失效时刻（UTC，RFC3339）。 */
  expiresAt: string;
  /** 是否已自动打开浏览器；false 时界面提示手动打开。 */
  browserOpened: boolean;
}

/** `k2a_login_sites` 的一条：登录过的企业域。 */
export interface LoginSite {
  /** IAM Identity Center 的 Start URL（`https://d-xxxxxxxxxx.awsapps.com/start`）。 */
  startUrl: string;
  /** 那次登录用的 OIDC 区域；选中时与 URL 一起填回去。 */
  region: string;
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
  /** 公共模型 id → 上游消耗倍率（rateMultiplier，1 = 基准，0 = 不计费）。空表 = 没同步过。 */
  rates: Record<string, number>;
  /**
   * 曾经同步到、这次上游不再提供的模型。
   *
   * 存在意义是「说清楚」而不是「藏起来」：套餐或组织权限变化后模型会整批消失，
   * 直接不显示会让人以为程序坏了；灰态展示 + 一句「当前套餐不可用」才是可核对的。
   */
  unavailable: string[];
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

/** `k2a_workbuddy_import` 的返回：按「地址 + 模型 id」合并进 WorkBuddy 后的摘要。 */
export interface WorkbuddyImportSummary {
  added: number;
  updated: number;
  unchanged: number;
}

/** 一条指向本应用的 WorkBuddy 自定义模型。 */
export interface WorkbuddyEntry {
  id: string;
  /** 条目里写的完整地址（host+端口必然与代理一致，路径可能各不相同）。 */
  url: string;
  /** 条目密钥与当前代理密钥是否一致；false 时界面提示重置密钥已让它过期。 */
  keyMatches: boolean;
}

/** `k2a_workbuddy_list` 的返回。读盘失败不作为命令错误，就地放在 error 里。 */
export interface WorkbuddyScan {
  /** ~/.workbuddy 目录不存在 = WorkBuddy 未安装。 */
  installed: boolean;
  entries: WorkbuddyEntry[];
  error: string | null;
}

/** 一条指向本应用的 Codex provider（`~/.codex/config.toml` 的 model_providers 条目）。 */
export interface CodexEntry {
  id: string;
  baseUrl: string;
  /** 顶层 `model_provider` 是否指向它 —— 即 Codex 当前是否正在用它。 */
  active: boolean;
  /** provider 配置里的密钥与当前代理密钥是否一致；false 时界面提示重置密钥已让它过期。 */
  keyMatches: boolean;
}

/** Codex 顶层 `model_catalog_json` 指针的状态。 */
export type CodexCatalogState =
  /** 没有指针 —— 从没接过模型目录。 */
  | 'none'
  /** 指向本应用的目录文件，勾选的模型就写在里面。 */
  | 'ours'
  /** 指向其他工具的目录文件 —— 本应用一概不碰。 */
  | 'foreign';

/** `k2a_codex_list` 的返回。读盘失败不作为命令错误，就地放在 error 里。 */
export interface CodexScan {
  /** ~/.codex 目录不存在 = Codex 未安装。 */
  installed: boolean;
  entries: CodexEntry[];
  error: string | null;
  /** 模型目录指针状态。 */
  catalogState: CodexCatalogState;
  /** 目录里的模型（仅 ours 时有值）；Codex 的 /model 选择器从这里取。 */
  catalogModels: string[];
  /** Codex 顶层 `model` —— 用户在 /model 里的当前选择（仅 ours 时上报）。 */
  activeModel: string | null;
}

/** `k2a_codex_import` 的返回：接入 Codex 后哪些东西真的写了。 */
export interface CodexImportSummary {
  configWritten: boolean;
  /** 密钥是否写进 provider 配置（`experimental_bearer_token`）；auth.json 一概不碰。 */
  keyWritten: boolean;
  /** 模型目录文件被写入或删除。 */
  catalogWritten: boolean;
  /** Codex 顶层默认模型被写入或撤除。 */
  modelWritten: boolean;
  /** 写入那一刻 Codex 桌面应用在不在跑：跑着才需要重启确认。 */
  codexRunning: boolean;
}

/** `k2a_codex_remove` 的结果。 */
export interface CodexRemoveResult {
  /** 是否确有改动（指向其它服务的配置一概不动）。 */
  removed: boolean;
  /**
   * 是否走了「整份还原接管前配置」这条路。
   * false = 没有接管时的快照（改动之前就接入过的），退回逐键删除。
   */
  restored: boolean;
  /** 移除那一刻 Codex 桌面应用在不在跑：跑着才需要重启确认。 */
  codexRunning: boolean;
}

/** `k2a_codex_restart` 的结果。 */
export interface CodexRestartResult {
  /** 动手之前应用是否正在运行（不在运行时无需重启）。 */
  running: boolean;
  /** 是否完成了「结束进程 + 重新拉起」。 */
  restarted: boolean;
  /** 主进程名（ChatGPT / Codex），提示语里点名重启的是哪个应用。 */
  app: string | null;
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
  /** 数据取得时刻（UTC ISO）。后端在查询 / 读缓存时按落盘时间戳补上；拿不到时为 null。 */
  fetchedAt: string | null;
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

/**
 * 一个单选组的候选项。
 *
 * 选项表由后端给（`settings::EFFORT_OPTIONS` / `CONTEXT_OPTIONS`）：档位与文案只在那里
 * 存在一份，前端不抄第二份 —— 少一档、多一档、改个说法都只改一处。
 */
export interface PrefOption<T> {
  value: T;
  label: string;
  /** 跟在中文说明后面的英文标识（思考级别的 `high` 这类）；上下文档位没有。 */
  hint?: string | null;
}

/** 一个单选组：当前值 + 候选。 */
export interface PrefChoice<T> {
  value: T;
  options: PrefOption<T>[];
}

/** `k2a_preferences_get` 的返回：代理侧的两个默认值。 */
export interface PreferencesState {
  /** 客户端没指定思考级别时补上的那一档。 */
  defaultEffort: PrefChoice<string>;
  /** 写进 Codex 模型目录的上下文大小（token）。 */
  contextWindow: PrefChoice<number>;
}
