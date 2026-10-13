// 入口：挂载外壳。
//
// 只做一件事 —— 找到容器并交出控制权。真正的启动顺序（先本地状态、后联网检查更新）
// 在 `ui/shell/shell` 的 `boot()` 里，这里不重复一遍。
import './styles/tokens.css';
import './styles/base.css';
import './styles/components.css';
import './styles/shell.css';
import './styles/views.css';

import { mountApp } from './ui/shell/shell';

const container = document.getElementById('app');
if (container) {
  mountApp(container);
} else {
  // 走到这里说明 index.html 与入口对不上了（构建/模板问题），是开发期硬故障：
  // 无 UI 可展示，只能打到宿主控制台，让 WebView2 日志留下线索。
  console.error('[kiro2api] 启动失败：缺少 #app 容器');
}
