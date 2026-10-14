// 应用状态的形状、初值与派生结论。
//
// 这里只放「数据与从数据得出的结论」（例如状态徽标该是什么颜色、什么文案），
// 不放 HTML —— 拼 HTML 属于表现层。这样状态语义只有一个来源，
// 状态栏、侧边栏、卡片三处显示的运行状态永远不会各说各话。
import type {
  ModelsState,
  ProxyAccess,
  ProxyStatus,
  UpdateState,
  UsageInfo,
} from '../core/types';
import { createStore } from './store';

/** 分区视图的标识。顺序即左侧导航的默认顺序。 */
export const VIEW_IDS = [
  'overview',
  'endpoints',
  'models',
  'usage',
  'updates',
  'maintenance',
] as const;

export type ViewId = (typeof VIEW_IDS)[number];

export interface AppState {
  /** 当前分区。 */
  view: ViewId;
  status: ProxyStatus | null;
  access: ProxyAccess | null;
  models: ModelsState | null;
  /**
   * 「可用模型」分区的搜索词。
   *
   * 放在全局状态而不是视图内部，是因为视图是「状态进、HTML 出」的纯函数 —— 它没地方存东西。
   * 写进 store 还有两个好处：后台推来新的模型清单时会连带重绘整个分区，过滤条件不会因此
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
}

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
  status: null,
  access: null,
  models: null,
  modelFilter: '',
  usage: null,
  pending: [],
  error: null,
  update: { ...INITIAL_UPDATE },
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
  return { tone: 'off', label: '已停止', detail: '代理未运行，双协议接口不可用' };
}
