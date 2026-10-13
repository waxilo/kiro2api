// 分区注册表。
//
// 类型写成 `Record<ViewId, ViewSpec>` 而不是数组或松散对象：新增一个 ViewId 却忘了实现视图、
// 或者视图对象的 id 与键不一致，都会在编译期报错。运行时「点了导航什么都没发生」
// 这类问题在桌面应用里特别难查（没有 devtools 的话连 console 都看不到），
// 交给类型系统最划算。
import type { ViewId } from '../../state/app-state';
import { endpointsView } from './endpoints';
import { maintenanceView } from './maintenance';
import { modelsView } from './models';
import { overviewView } from './overview';
import { updatesView } from './updates';
import { usageView } from './usage';
import type { ViewSpec } from './types';

export const VIEWS: Record<ViewId, ViewSpec> = {
  overview: overviewView,
  endpoints: endpointsView,
  models: modelsView,
  usage: usageView,
  updates: updatesView,
  maintenance: maintenanceView,
};

export type { ViewSpec };
