//! 托盘常驻：关窗隐藏而非退出，代理在后台持续对外提供接口。
//!
//! 这是一个「后台服务型」应用 —— 用户打开界面配置完，之后可以关掉窗口继续用接口。
//! 因此关闭按钮的语义是「隐藏」，真正退出走托盘菜单或界面里的退出按钮。

use std::thread;

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Runtime};

use crate::kiro::{self, KiroProxyState};
use crate::window::show_main_window;

const MENU_SHOW: &str = "show";
const MENU_START: &str = "start-proxy";
const MENU_STOP: &str = "stop-proxy";
const MENU_QUIT: &str = "quit";

pub(crate) fn setup_tray<R: Runtime>(app: &AppHandle<R>) -> Result<(), Box<dyn std::error::Error>> {
    let show = MenuItem::with_id(app, MENU_SHOW, "显示主窗口", true, None::<&str>)?;
    let start = MenuItem::with_id(app, MENU_START, "启动代理", true, None::<&str>)?;
    let stop = MenuItem::with_id(app, MENU_STOP, "停止代理", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, MENU_QUIT, "退出 kiro2api", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &start, &stop, &separator, &quit])?;

    let mut builder = TrayIconBuilder::with_id("main-tray")
        .menu(&menu)
        // 左键单击直接唤出窗口，不弹菜单（菜单留给右键）
        .show_menu_on_left_click(false)
        .tooltip("kiro2api — 本地 Kiro 反代")
        .on_menu_event(|app, event| match event.id().as_ref() {
            MENU_SHOW => show_main_window(app),
            MENU_START => spawn_toggle_proxy(app, true),
            MENU_STOP => spawn_toggle_proxy(app, false),
            MENU_QUIT => {
                let state = app.state::<KiroProxyState>();
                let _ = kiro::stop_proxy(&state);
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        });

    // 复用窗口图标，省一份独立资源；缺失时托盘仍可创建（只是没有图标）。
    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    }

    builder.build(app)?;
    Ok(())
}

/// 托盘菜单的启停走后台线程：阻塞式启停不能占用事件循环。
fn spawn_toggle_proxy<R: Runtime>(app: &AppHandle<R>, start: bool) {
    let app = app.clone();
    thread::spawn(move || {
        let state = app.state::<KiroProxyState>().inner().clone();
        let _guard = match state.op_lock.lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };
        let result = if start {
            kiro::start_proxy(&state, None).map(|_| "已启动")
        } else {
            kiro::stop_proxy(&state).map(|_| "已停止")
        };
        match result {
            Ok(message) => eprintln!("[tray] {message}"),
            Err(error) => eprintln!("[tray] 操作失败: {error}"),
        }
    });
}
