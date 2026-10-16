//! 托盘常驻：关窗隐藏而非退出，代理在后台持续对外提供接口。
//!
//! 这是一个「后台服务型」应用 —— 用户打开界面配置完，之后可以关掉窗口继续用接口。
//! 因此关闭按钮的语义是「隐藏」，真正退出走托盘菜单或界面里的退出按钮。

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;
use std::thread;

use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Runtime};

use crate::kiro::{self, KiroProxyState};
use crate::window::show_main_window;

const TRAY_ID: &str = "main-tray";
const TOOLTIP_IDLE: &str = "kiro2api — 本地 Kiro 反代";

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

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        // 左键单击直接唤出窗口，不弹菜单（菜单留给右键）
        .show_menu_on_left_click(false)
        .tooltip(TOOLTIP_IDLE)
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

// ── 运行态图标 ──────────────────────────────────────────────────────────

/// 0=已应用「停止」图标 1=已应用「运行」图标 2=尚未应用过（首次必刷新）。
static LAST_TRAY_STATE: AtomicU8 = AtomicU8::new(2);
/// (停止态图标, 运行态图标)。首次用到时才从默认窗口图标合成。
static TRAY_ICONS: OnceLock<(Option<Image<'static>>, Option<Image<'static>>)> = OnceLock::new();

/// 让托盘图标与提示跟随代理运行状态；状态没变就不动托盘。
///
/// 在「启停完成」与「状态查询」路径上都会被调：前者保证用户操作立即反映到托盘，
/// 后者兜住代理异常退出后图标滞后的场景（下一次任何状态查询都会纠正）。
/// 没有应用句柄（单元测试）或托盘不存在（初始化失败）时静默跳过。
pub(crate) fn sync_running(running: bool, port: Option<u16>) {
    let target = u8::from(running);
    if LAST_TRAY_STATE.swap(target, Ordering::Relaxed) == target {
        return;
    }
    let Some(app) = crate::APP_HANDLE.get() else {
        LAST_TRAY_STATE.store(2, Ordering::Relaxed);
        return;
    };
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    let Some((idle, running_icon)) = tray_icon_variants(app) else {
        return;
    };

    if running {
        if let Some(icon) = running_icon {
            let _ = tray.set_icon(Some(icon.clone()));
        }
        let tooltip = match port {
            Some(port) => format!("kiro2api — 运行中 · 127.0.0.1:{port}"),
            None => "kiro2api — 代理运行中".to_string(),
        };
        let _ = tray.set_tooltip(Some(&tooltip));
    } else if let Some(icon) = idle {
        let _ = tray.set_icon(Some(icon.clone()));
        let _ = tray.set_tooltip(Some(TOOLTIP_IDLE));
    }
}

/// 默认窗口图标的两个变体：原样（停止）与叠加绿色运行圆点（运行中）。
/// 默认图标缺失时返回 (None, None)，托盘保持无图标（与初始化时的行为一致）。
fn tray_icon_variants(
    app: &AppHandle,
) -> Option<&'static (Option<Image<'static>>, Option<Image<'static>>)> {
    let base = app.default_window_icon()?;
    Some(TRAY_ICONS.get_or_init(|| {
        let base = Image::new_owned(base.rgba().to_vec(), base.width(), base.height());
        (Some(base.clone()), Some(with_running_badge(&base)))
    }))
}

/// 在图标右下角叠加一枚亮绿圆点（带 1px 抗锯齿边缘）。
/// 图标右下是深色底，绿点在浅色/深色任务栏上都清晰可辨。
fn with_running_badge(base: &Image) -> Image<'static> {
    let mut rgba = base.rgba().to_vec();
    let (w, h) = (i64::from(base.width()), i64::from(base.height()));
    if w == 0 || h == 0 || rgba.len() != (w * h * 4) as usize {
        return Image::new_owned(rgba, base.width(), base.height());
    }
    let (wf, hf) = (f64::from(base.width()), f64::from(base.height()));
    let (cx, cy) = (wf * 0.75, hf * 0.75);
    let radius = wf.min(hf) * 0.21;
    // 运行绿：取 #22C55E，与界面「运行中」状态色一致
    let (gr, gg, gb) = (34.0_f64, 197.0_f64, 94.0_f64);

    let x0 = ((cx - radius - 1.0).ceil() as i64).max(0);
    let x1 = ((cx + radius + 1.0).floor() as i64).min(w - 1);
    let y0 = ((cy - radius - 1.0).ceil() as i64).max(0);
    let y1 = ((cy + radius + 1.0).floor() as i64).min(h - 1);
    for y in y0..=y1 {
        for x in x0..=x1 {
            let dx = x as f64 + 0.5 - cx;
            let dy = y as f64 + 0.5 - cy;
            // 像素中心到圆心的距离决定覆盖率，边缘一圈自然过渡
            let coverage = (radius + 0.5 - (dx * dx + dy * dy).sqrt()).clamp(0.0, 1.0);
            if coverage <= 0.0 {
                continue;
            }
            let idx = ((y * w + x) * 4) as usize;
            let src_a = f64::from(rgba[idx + 3]) / 255.0;
            // 标准 over 合成：绿点盖在原像素上，双方都按覆盖率贡献
            let out_a = coverage + src_a * (1.0 - coverage);
            if out_a <= 0.0 {
                continue;
            }
            let blend = |src: f64, dot: f64| -> u8 {
                // dot/src 都是 0-255 刻度，out_a 是 0-1：除回去就回到 0-255，不能再乘 255
                ((dot * coverage + src * src_a * (1.0 - coverage)) / out_a)
                    .round()
                    .clamp(0.0, 255.0) as u8
            };
            rgba[idx] = blend(f64::from(rgba[idx]), gr);
            rgba[idx + 1] = blend(f64::from(rgba[idx + 1]), gg);
            rgba[idx + 2] = blend(f64::from(rgba[idx + 2]), gb);
            rgba[idx + 3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
    Image::new_owned(rgba, base.width(), base.height())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_badge_paints_green_center_and_keeps_untouched_area() {
        // 32x32 全透明底：圆心 (24,24) 应被涂成纯绿，远角 (2,2) 保持透明
        let base = Image::new_owned(vec![0u8; 32 * 32 * 4], 32, 32);
        let badged = with_running_badge(&base);

        let pixel = |x: usize, y: usize| -> [u8; 4] {
            let idx = (y * 32 + x) * 4;
            [
                badged.rgba()[idx],
                badged.rgba()[idx + 1],
                badged.rgba()[idx + 2],
                badged.rgba()[idx + 3],
            ]
        };
        assert_eq!(pixel(24, 24), [34, 197, 94, 255], "圆心必须是纯 #22C55E");
        assert_eq!(pixel(2, 2), [0, 0, 0, 0], "圆外的像素不应被改动");
        // 边缘应有抗锯齿过渡：半径 6.72、圆心 (24,24)，(24,17) 落在圆周上
        let edge = pixel(24, 17);
        assert!(
            edge[3] > 0 && edge[3] < 255,
            "圆边缘应有半透明过渡像素，实际 alpha={}",
            edge[3]
        );
    }
}
