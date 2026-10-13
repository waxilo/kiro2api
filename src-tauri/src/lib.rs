//! kiro2api 应用装配：插件、状态、命令注册、托盘常驻、自动启动。

use std::path::Path;
use std::sync::OnceLock;
use std::thread;

use tauri::{AppHandle, Manager};

mod paths;
mod prefs;
mod proc_guard;
mod protocol_guard;
mod tray;
mod updater_accel;
mod window;

mod kiro;

use kiro::KiroProxyState;
use window::MAIN_WINDOW;

/// 全局 AppHandle：后台线程（凭据刷新循环）需要它向前端广播事件。
/// 在 `setup` 里写入一次，之后只读。
pub(crate) static APP_HANDLE: OnceLock<AppHandle> = OnceLock::new();

/// 在系统文件管理器里定位某个文件或目录。
pub(crate) fn reveal_in_file_manager(target: &Path) -> Result<(), String> {
    if !target.exists() {
        return Err(format!("路径不存在: {}", target.display()));
    }
    let is_dir = target.is_dir();

    #[cfg(target_os = "windows")]
    {
        let mut cmd = std::process::Command::new("explorer");
        // 目录直接打开；文件用 /select, 定位高亮（参数必须写成 `/select,<path>` 单参数形式）
        if is_dir {
            cmd.arg(target);
        } else {
            cmd.arg(format!("/select,{}", target.display()));
        }
        proc_guard::suppress_console_window(&mut cmd);
        cmd.spawn()
            .map_err(|e| format!("打开文件管理器失败: {e}"))?;
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    {
        let mut cmd = std::process::Command::new("open");
        if !is_dir {
            cmd.arg("-R");
        }
        cmd.arg(target);
        cmd.spawn()
            .map_err(|e| format!("打开访达失败: {e}"))?;
        return Ok(());
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = is_dir;
        std::process::Command::new("xdg-open")
            .arg(target)
            .spawn()
            .map_err(|e| format!("打开文件管理器失败: {e}"))?;
        return Ok(());
    }

    #[allow(unreachable_code)]
    Err("当前平台不支持打开文件管理器".to_string())
}

/// 应用启动时按用户偏好自动拉起代理（不阻塞窗口显示）。
fn spawn_autostart(app: AppHandle, state: KiroProxyState) {
    thread::spawn(move || {
        let config = prefs::load_config();
        if !config.auto_start {
            eprintln!("[proxy] 跳过自动启动：未开启自动启动");
            return;
        }
        if !kiro::is_locally_available() {
            eprintln!("[proxy] 跳过自动启动：本机无 Kiro 凭据");
            return;
        }
        let _guard = match state.op_lock.lock() {
            Ok(guard) => guard,
            Err(_) => {
                eprintln!("[proxy] 跳过自动启动：操作锁已损坏");
                return;
            }
        };
        match kiro::start_proxy(&state, None) {
            Ok(status) => {
                eprintln!(
                    "[proxy] 自动启动成功: running={} port={:?}",
                    status.running, status.port
                );
                let _ = app.emit_to_all("proxy://ready", &status);
            }
            Err(error) => eprintln!("[proxy] 自动启动失败: {error}"),
        }
    });
}

/// 向前端统一广播事件的小工具（避免每处都写 `use tauri::Emitter`）。
trait EmitToAll {
    fn emit_to_all<S: serde::Serialize + Clone>(&self, event: &str, payload: S) -> tauri::Result<()>;
}

impl EmitToAll for AppHandle {
    fn emit_to_all<S: serde::Serialize + Clone>(
        &self,
        event: &str,
        payload: S,
    ) -> tauri::Result<()> {
        use tauri::Emitter;
        self.emit(event, payload)
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // 单实例必须是第一个插件：它要在窗口与其它插件建立之前判定本进程是否「重复启动」。
        // 重复启动时第二个进程在这里就把命令行交出去并自行退出，不会走到窗口创建。
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            window::show_main_window(app);
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(KiroProxyState::default())
        .setup(|app| {
            let handle = app.handle().clone();
            let _ = APP_HANDLE.set(handle.clone());

            // 本地更新清单代理：解决国内直连 GitHub 超时
            updater_accel::start_manifest_proxy();

            // 自绘标题栏的收尾：Windows 上装窗口过程钩子恢复 Win11 贴靠布局。
            // 必须在这里做 —— 配置里的窗口在本闭包之前就已建好，此时才有句柄。
            window::setup_self_drawn_titlebar(&handle);

            // 托盘失败不致命：应用照常可用，只是关窗后无处唤回，因此只告警。
            if let Err(error) = tray::setup_tray(&handle) {
                eprintln!("[tray] 托盘初始化失败: {error}");
            }

            let state = app.state::<KiroProxyState>().inner().clone();
            spawn_autostart(handle, state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            kiro::k2a_status,
            kiro::k2a_models,
            kiro::k2a_access,
            kiro::k2a_start,
            kiro::k2a_stop,
            kiro::k2a_sync_models,
            kiro::k2a_save_models,
            kiro::k2a_set_default_model,
            kiro::k2a_set_auto_start,
            kiro::k2a_set_port,
            kiro::k2a_copy_access,
            kiro::k2a_reset_key,
            kiro::k2a_usage,
            kiro::k2a_refresh_token,
            kiro::k2a_reveal,
            window::k2a_snap_rect,
            window::k2a_diag,
            window::k2a_quit_app,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| match event {
            // 关闭主窗口不退出：隐藏到托盘，代理继续服务；真正退出走托盘菜单或前端「退出」。
            tauri::RunEvent::WindowEvent {
                label,
                event: tauri::WindowEvent::CloseRequested { api, .. },
                ..
            } => {
                if label == MAIN_WINDOW {
                    api.prevent_close();
                    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
                        let _ = window.hide();
                    }
                }
            }
            // macOS 点 Dock 图标走 Reopen（系统不会给第二个进程）
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Reopen { .. } => window::show_main_window(app),
            tauri::RunEvent::Exit => {
                eprintln!("[app] 退出，正在停止代理");
                let state = app.state::<KiroProxyState>();
                let _ = kiro::stop_proxy(&state);
            }
            _ => {}
        })
}
