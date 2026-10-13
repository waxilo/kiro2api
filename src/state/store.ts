// 极简订阅式 store：一处状态，多处读取，单向数据流。
//
// 为什么不用「渲染函数里随手改全局变量」：改完之后谁该重画、按什么顺序重画，
// 全靠人肉记着。收敛成 store 之后，写入方只管 `set`，重画由外壳单点订阅完成 ——
// 新增一个分区视图不需要在任何别处补一行重绘调用。
//
// 只做浅比较：本应用的状态是「几个大对象整体替换」的形状（status / models / update …），
// 深比较的收益抵不上每帧一次递归遍历的成本。

export type Listener<T> = (state: Readonly<T>, previous: Readonly<T>) => void;

export interface Store<T extends object> {
  /** 读取当前状态（只读视图）。 */
  get(): Readonly<T>;
  /** 合并一个补丁；所有字段引用相等时不触发通知。 */
  set(patch: Partial<T>): void;
  /** 订阅变化，返回退订函数。 */
  subscribe(listener: Listener<T>): () => void;
}

export function createStore<T extends object>(initial: T): Store<T> {
  let state: T = initial;
  const listeners = new Set<Listener<T>>();

  return {
    get: () => state,

    set(patch) {
      let changed = false;
      for (const key of Object.keys(patch) as (keyof T)[]) {
        if (!Object.is(state[key], patch[key])) {
          changed = true;
          break;
        }
      }
      // 没有实际变化就安静返回：定时刷新与后台事件经常带来内容相同的补丁，
      // 每次都重画会让正在输入的表单闪烁、也会把滚动条反复重置。
      if (!changed) return;

      const previous = state;
      state = { ...state, ...patch };
      for (const listener of listeners) listener(state, previous);
    },

    subscribe(listener) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
  };
}
