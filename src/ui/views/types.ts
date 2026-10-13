// 分区视图的统一契约。
//
// 每个分区只声明「它是什么、标题写什么、右上角放什么动作、主体长什么样」，
// 不负责挂载、重绘、滚动或事件绑定 —— 那些由外壳统一处理。
// 结果是新增一个分区只需加一个文件 + 在注册表里登记一行，不必改任何既有代码。
import type { AppState, ViewId } from '../../state/app-state';

export interface ViewSpec {
  id: ViewId;
  /** 左侧导航里的名字（求短）。 */
  label: string;
  /** 内容区标题。 */
  title: string;
  /** 标题下的一句话说明，回答「这一页是干什么的」。 */
  desc: string;
  /** 标题右侧的动作区；不需要就返回空串。返回的是已转义或受控的 HTML。 */
  head(state: AppState): string;
  /** 视图主体。返回的是已转义或受控的 HTML。 */
  body(state: AppState): string;
}
