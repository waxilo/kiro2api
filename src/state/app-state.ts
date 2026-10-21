// 应用状态的形状、初值与派生结论。
//
// 这里只放「数据与从数据得出的结论」（例如状态徽标该是什么颜色、什么文案），
// 不放 HTML —— 拼 HTML 属于表现层。这样状态语义只有一个来源，
// 状态栏、侧边栏、卡片三处显示的运行状态永远不会各说各话。
import type {
  CodexScan,
  CopyKind,
  ModelsState,
  ProxyAccess,
  ProxyStatus,
  UpdateState,
  UsageInfo,
  WorkbuddyScan,
} from '../core/types';
import { createStore } from './store';

/** 分区视图的标识。顺序即左侧导航的默认顺序。 */
export const VIEW_IDS = [
  'overview',
  'endpoints',
  'thirdparty',
  'updates',
  'maintenance',
] as const;

export type ViewId = (typeof VIEW_IDS)[number];

/**
 * 概览页弹窗的标识。
 *
 * 「可用模型」与「账户额度」曾经是两个独立分区，现在是概览页上两块指标卡点开的弹窗。
 * 理由：它们是同一类东西 —— **只读的数字**，既不改配置也不产生动作分支，用户看它们
 * 只为了回答「账上有什么、还剩多少」，看完就回去。为这类内容单开一个页面，等于把
 * 「一眼就能看完的东西」摊成一屏，还要用户自己点回概览。放进弹窗之后，上下文（本地服务
 * 跑没跑、接入地址是什么）仍然在旁边，看完关掉就回到了原来那一屏。
 */
export const MODAL_IDS = ['models', 'usage'] as const;

export type ModalId = (typeof MODAL_IDS)[number];

export interface AppState {
  /** 当前分区。 */
  view: ViewId;
  /**
   * 当前打开的概览弹窗；null = 没有弹窗。
   *
   * 弹窗状态进 store 而不是留在视图里，理由与 `modelFilter` 相同：视图是纯函数，
   * 而且它必须能被「点一下打开」这个动作驱动（视图自己不接事件）。
   */
  modal: ModalId | null;
  status: ProxyStatus | null;
  access: ProxyAccess | null;
  models: ModelsState | null;
  /**
   * 「可用模型」弹窗的搜索词。
   *
   * 放在全局状态而不是视图内部，是因为视图是「状态进、HTML 出」的纯函数 —— 它没地方存东西。
   * 写进 store 还有两个好处：后台推来新的模型清单时会连带重绘弹窗，过滤条件不会因此
   * 被悄悄清掉；以及它天然可以被测试当作一个普通状态来断言。
   *
   * 只是界面状态，不落盘：重开应用应该回到「看到全部模型」。
   */
  modelFilter: string;
  usage: UsageInfo | null;
  /**
   * 正在执行的动作（同名动作同时至多一个）。
   *
   * 这里从前是 `busy: boolean` + `busyLabel: string | null` 两个字段。拆成列表是为了区分
   * **占用范围**：光有一个布尔量，任何动作一开跑就把整页会改配置的按钮一起灰掉，
   * 而「刷新凭据」等的是最长 30 秒的上游 HTTP —— 用户在这 30 秒里想启动代理、复制地址、
   * 查额度，全都被静默吞掉，看起来就是「页面锁住了」。
   *
   * 现在每个动作自己声明它占多大地方：会改配置的（启停代理、端口、自启动、重置密钥、
   * 同步模型）互相阻塞；只碰自己那一块的（刷新凭据、查询额度）不参与全局门控。
   */
  pending: PendingAction[];
  /**
   * 最近一次失败的说明。
   *
   * 界面**不**为它留位置 —— 曾经是一块顶部错误横幅，会把内容整体推下去、把按钮挪位。
   * 现在由外壳订阅它，在它变成新值时播一条浮层提示（`ui/shell/shell.ts` 的
   * `announceError`），仅此而已。所以这个字段的语义是「有一条新失败待播报」：
   * 动作成功时会被清空，不会长期停在某个值上。
   */
  error: string | null;
  update: UpdateState;
  /**
   * 「三方接入」分区里当前的 tab。tab 结构先于内容存在：
   * 现在只有 WorkBuddy 一个，但接入方式（tab 条）已经按多个客户端设计。
   */
  thirdPartyTab: ThirdPartyTab;
  /**
   * 「三方接入」分区的 WorkBuddy 扫描结果；null 表示还没扫过（首次进入分区时拉取）。
   *
   * 只含「指向本应用」的条目 —— host+端口匹配才进得来，WorkBuddy 里配给别的服务的
   * 条目一概不出现。进分区与焦点回到窗口时各刷一次，用户在 WorkBuddy 侧手工改动后
   * 切回来就能看到。
   */
  workbuddyScan: WorkbuddyScan | null;
  /**
   * 「三方接入」分区的搜索词。语义与 `modelFilter` 相同：只影响显示，
   * 不碰勾选；纯界面状态，不落盘。
   */
  workbuddyFilter: string;
  /**
   * 三方接入卡片的勾选状态（模型 id 集合）。勾选即语义：在集合里 = 该模型
   * 应写进 WorkBuddy，不在 = 应从 WorkBuddy 移除。
   *
   * null 表示用户还没动过，勾选跟随扫描结果；点过一张卡后物化成完整集合，
   * 此后焦点刷新（会重扫清单）不再覆盖未应用的改动，直到「应用变更」落地
   * 或把勾选拨回与扫描一致。
   */
  workbuddyChecked: string[] | null;
  /**
   * 「三方接入」分区的 Codex 扫描结果；null 表示还没扫过（进分区时拉取）。
   *
   * 含两类事实：provider（`model_providers` 表 + 顶层 `model_provider` 选择，
   * 单供应商模型）与模型目录（顶层 `model_catalog_json` 指针 + 目录文件内容）。
   * 只含指向本应用的条目与文件，其余 provider / 别家的目录看不见也就动不到。
   */
  codexScan: CodexScan | null;
  /**
   * Codex 模型网格的勾选状态（模型 id 集合）。勾选即语义：在集合里 = 该模型
   * 应写进 Codex 的 /model 模型目录，不在 = 应从目录移除（全部取消 = 连目录
   * 指针一起撤）。null = 用户还没动过，勾选跟随扫描结果 —— 语义与
   * `workbuddyChecked` 完全一致，包括「物化后不被焦点刷新覆盖」。
   */
  codexChecked: string[] | null;
  /** 「三方接入」分区 Codex 网格的搜索词。语义与 `workbuddyFilter` 相同。 */
  codexFilter: string;
  /**
   * Codex 配置变更后待确认的重启。Codex（现名 ChatGPT）只在启动时读一次配置，
   * 接入 / 清空之后不重启不生效 —— 弹窗只问「现在重启吗」，值记录这次变更是
   * 「写入」还是「清空」，文案据此区分；null = 没有待确认的重启。
   */
  codexRestartPrompt: 'applied' | 'cleared' | null;
  /**
   * 刚复制成功的复制类型；对应的复制按钮把文案短暂换成「已复制」。
   *
   * 反馈长在按钮上而不是 toast：地址 / .env / JSON 三颗按钮挨在一起，每点一下都弹
   * 浮层会很吵；按钮自己是「按了哪里哪里亮」。1.6 秒后由 actions 里的定时器清空，
   * 连续复制不同种类时以最后一次为准。
   */
  copiedKind: CopyKind | null;
}

/** 「三方接入」分区的 tab 标识。接入新的第三方客户端时在这里扩。 */
export type ThirdPartyTab = 'workbuddy' | 'codex';

/** 一个正在执行的动作。 */
export interface PendingAction {
  /** 动作名（取自 `state/actions` 的 `ACTION`）。 */
  label: string;
  /** 是否占用全局门控：会改配置的动作占用，只读 / 局部刷新的不占用。 */
  blocking: boolean;
}

/** 语义色档位。渲染层据此选 class，状态层只负责判断档位。 */
export type Tone = 'ok' | 'off' | 'warn' | 'err';

export interface StatusSummary {
  tone: Tone;
  /** 短标签，如「运行中」。 */
  label: string;
  /** 补充说明，用于状态栏与卡片尾部。 */
  detail: string;
}

const INITIAL_UPDATE: UpdateState = {
  currentVersion: '',
  latestVersion: '',
  available: false,
  phase: 'idle',
  statusText: null,
  error: null,
  notes: null,
  downloaded: 0,
  total: 0,
  source: null,
};

export const store = createStore<AppState>({
  view: 'overview',
  modal: null,
  status: null,
  access: null,
  models: null,
  modelFilter: '',
  usage: null,
  pending: [],
  error: null,
  update: { ...INITIAL_UPDATE },
  thirdPartyTab: 'workbuddy',
  workbuddyScan: null,
  workbuddyFilter: '',
  workbuddyChecked: null,
  codexScan: null,
  codexChecked: null,
  codexFilter: '',
  codexRestartPrompt: null,
  copiedKind: null,
});

/**
 * 有「会改配置」的动作在跑吗 —— 期间其它同为改配置的控件不可用。
 *
 * 判断依据是「动作会不会改配置」，不是「有没有东西在跑」：刷新凭据也在跑，但它只碰
 * 凭据文件，没有理由让别人等着。
 */
export function isBlocking(state: AppState): boolean {
  return state.pending.some((task) => task.blocking);
}

/** 会改配置、正在跑的那一个动作名（用于解释「为什么这颗按钮现在不能点」）。 */
export function blockingLabel(state: AppState): string | null {
  return state.pending.find((task) => task.blocking)?.label ?? null;
}

/** 这个动作正在跑吗 —— 它自己那颗按钮据此写「…中」并禁用，避免点两次。 */
export function isPending(state: AppState, label: string): boolean {
  return state.pending.some((task) => task.label === label);
}

/**
 * 把「服务状态」压成一个语义结论。
 *
 * 四种情形互斥且覆盖完整，判断顺序有讲究：凭据缺失要排在「已停止」前面 ——
 * 缺凭据时点「启动」必然失败，界面必须先把原因摆出来，而不是给一个能点但会报错的按钮。
 */
export function summarizeStatus(state: AppState): StatusSummary {
  const status = state.status;
  if (!status) {
    return { tone: 'off', label: '读取中', detail: '正在读取本地服务状态' };
  }
  if (status.running) {
    return {
      tone: 'ok',
      label: '运行中',
      detail: status.port ? `http://127.0.0.1:${status.port}` : '服务已监听',
    };
  }
  if (!status.available) {
    return { tone: 'warn', label: '缺少凭据', detail: '未找到 Kiro 凭据，无法启动代理' };
  }
  return { tone: 'off', label: '已停止', detail: '代理未运行，协议接口不可用' };
}
