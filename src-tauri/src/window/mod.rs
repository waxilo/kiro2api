//! 主窗口装配：跨平台一致对外，平台差异收在本模块内。
//!
//! 窗口装饰走全自绘：Windows 由 `tauri.conf.json` 的 `decorations: false` 去掉系统标题栏与
//! 三个系统按钮（tao 保留了 RESIZABLE 的命中测试与 DWM 阴影，所以拖动边缩放、最大化不压
//! 任务栏、窗口阴影都还在）；macOS 则由 `tauri.macos.conf.json` 换成 `titleBarStyle: Overlay`，
//! 保留原生红绿灯与圆角，标题栏本身的像素由前端绘制。
//!
//! 调用方只看到 `show_main_window` 这类与平台无关的入口，平台专属代码（Win11 贴靠命中）
//! 一律关在 `#[cfg]` 内，避免散落到调用点。
//!
//! 三份平台配置的一致性由 `tests/platform_config.rs` 守着 —— macOS 那两份在 Windows 上
//! 根本不会被读取，靠人眼比对迟早出错。

mod snap;

use tauri::{AppHandle, Manager, Runtime};

use crate::kiro::{self, KiroProxyState};

pub(crate) const MAIN_WINDOW: &str = "main";

/// 唤起主窗口：把「隐藏在托盘 / 已最小化 / 被其它窗口挡住」三种情形统一恢复到前台。
///
/// 顺序不可交换：tao 的 `set_focus` 会先看窗口标志，只要窗口处于隐藏或最小化就**直接跳过**
/// 聚焦动作（只改状态、不置前），所以 `unminimize` / `show` 必须排在 `set_focus` 之前。
///
/// 这是「用户想打开应用」的统一落点，被三处复用：托盘菜单与左键、单实例转交（重复启动）、
/// macOS 点击 Dock 图标。
pub(crate) fn show_main_window<R: Runtime>(app: &AppHandle<R>) {
    let Some(window) = app.get_webview_window(MAIN_WINDOW) else {
        eprintln!("[window] 未找到主窗口 {MAIN_WINDOW}，无法唤起");
        return;
    };
    if window.is_minimized().unwrap_or(false) {
        let _ = window.unminimize();
    }
    // 隐藏到托盘时靠这一步回来；窗口本就可见时是幂等的。
    let _ = window.show();
    let _ = window.set_focus();
}

/// 自绘标题栏的收尾工作：装 Windows 专属的窗口过程钩子（Win11 贴靠布局）。
///
/// 必须在 `setup` 里调用 —— 配置里的窗口在用户 `setup` 之前就已经建好，此时才有句柄可用。
pub(crate) fn setup_self_drawn_titlebar<R: Runtime>(app: &AppHandle<R>) {
    let Some(window) = app.get_webview_window(MAIN_WINDOW) else {
        eprintln!("[window] 未找到主窗口 {MAIN_WINDOW}，跳过贴靠布局支持");
        return;
    };
    snap::install(&window);
}

/// 前端上报最大化按钮矩形（CSS 像素、相对客户区），供贴靠命中使用。
///
/// 每次窗口尺寸变化都会重报：按钮的横坐标依赖窗口宽度，缓存错了命中区就会偏。
#[tauri::command]
pub fn k2a_snap_rect(x: f64, y: f64, width: f64, height: f64) {
    snap::set_button_rect(x, y, width, height);
}

/// 前端自报状态，用于排查「窗口全白 / 界面没起来」这类只能从页面里看出来的故障。
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrontendReport {
    href: String,
    ready_state: String,
    html_length: i64,
    app_length: i64,
    titlebar: bool,
    max_button: bool,
    scripts: String,
}

/// 页面自检通道：前端挂完外壳之后调一次，后端只负责落一行日志。
///
/// 为什么需要它：窗口一片白时，从操作系统那一侧只能看到「没有内容」——脚本是被 CSP 拦了、
/// 还是 bundle 没加载、还是页面停在 about:blank，全都看不见。让页面把 `href`、`readyState`、
/// `#app` 的长度、有没有 `.titlebar` / `#wc-max` 报回来，这几种情形立刻分开。
///
/// 注意方向是**页面主动上报**，不是后端 `window.eval` 进来问：后者在这套 WebView2 上调用会
/// 成功返回、脚本却到不了页面（见 `window::snap` 里那段注释），是一条死路。
#[tauri::command]
pub fn k2a_diag(report: FrontendReport) {
    eprintln!(
        "[frontend] href={} readyState={} html={} app={} titlebar={} maxButton={} scripts={}",
        report.href,
        report.ready_state,
        report.html_length,
        report.app_length,
        report.titlebar,
        report.max_button,
        report.scripts
    );
}

/// 真正退出应用（停代理交给 `RunEvent::Exit` 兜底，避免两处各停一次）。
///
/// 关闭按钮不走这里：自绘的关闭按钮与系统关闭按钮同一条路径（发出 CloseRequested 后
/// 由 `lib.rs` 的事件处理隐藏到托盘），所以「关闭」的语义始终是「隐藏」，不会退出。
#[tauri::command]
pub fn k2a_quit_app<R: Runtime>(app: AppHandle<R>) {
    let state = app.state::<KiroProxyState>();
    let _ = kiro::stop_proxy(&state);
    app.exit(0);
}
