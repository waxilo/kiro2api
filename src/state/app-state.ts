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
  usage: UsageInfo | null;
  /** 额度查询进行中（与全局 `busy` 分开：它只影响额度那一个视图）。 */
  usageLoading: boolean;
  /** 有动作在执行：期间禁用会改配置的控件。 */
  busy: boolean;
  /**
   * 正在执行的动作名（如「刷新凭据」），`busy` 为假时是 null。
   *
   * 光有一个布尔量只能让按钮变灰 —— 用户看到的是「点了没反应」。带上名字之后，
   * 那个按钮自己就能写「刷新中…」，状态栏也能挂一条全局的进行态。
   */
  busyLabel: string | null;
  /** 最近一次失败的说明。 */
  error: string | null;
  update: UpdateState;
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
};

export const store = createStore<AppState>({
  view: 'overview',
  status: null,
  access: null,
  models: null,
  usage: null,
  usageLoading: false,
  busy: false,
  busyLabel: null,
  error: null,
  update: { ...INITIAL_UPDATE },
});

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

/** 服务地址，未运行时为 null。 */
export function serviceAddress(state: AppState): string | null {
  const port = state.status?.port;
  return state.status?.running && port ? `http://127.0.0.1:${port}` : null;
}
