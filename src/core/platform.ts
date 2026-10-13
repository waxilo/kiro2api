// 运行环境识别：平台样式类 + 是否跑在 Tauri 里。
//
// 为什么用 UA 而不是 navigator.platform：WebView2 的 UA 含 "Windows NT"，WKWebView 含
// "Macintosh"，两者都稳；navigator.platform 在隐私模式下会被冻结成固定值。
//
// ⚠️ 平台类不能单独决定「是否渲染自绘窗口按钮」：`npm run dev` 用 Chrome 打开时 UA 同样含
// Windows，光看类名会把按钮画出来，一点就抛错。所以必须叠一层 `isTauri()`。

export type Platform = 'windows' | 'macos' | 'linux';

/** 是否运行在 Tauri 运行时里（而非浏览器直接打开 dev server）。 */
export function isTauri(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

export function detectPlatform(): Platform {
  const ua = (navigator.userAgent || '').toLowerCase();
  if (ua.includes('mac')) return 'macos';
  if (ua.includes('win')) return 'windows';
  return 'linux';
}

/** 给 `<html>` 打平台类名，供 CSS 与渲染分支使用；返回识别结果。 */
export function applyPlatformClass(): Platform {
  const platform = detectPlatform();
  document.documentElement.classList.add(`platform-${platform}`);
  return platform;
}
