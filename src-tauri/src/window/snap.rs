//! Windows 11「贴靠布局」在自绘标题栏下的恢复。
//!
//! 背景：把鼠标停在最大化按钮上浮出分屏网格，靠的是窗口过程在 `WM_NCHITTEST` 里对那块
//! 区域返回 `HTMAXBUTTON`。系统标题栏由系统自己完成这一步；我们把标题栏改成自绘后，那块
//! 区域对系统来说只是普通客户区，浮出就没了（拖到屏幕边缘的贴靠仍然可用，因为它走的是
//! 拖拽那条路）。
//!
//! 做法是把窗口过程接一层：
//! - 命中「最大化按钮矩形」→ 返回 `HTMAXBUTTON`，交回给系统去浮出网格；
//! - `WM_NCLBUTTONDOWN` 命中同一区域 → 不交给 `DefWindowProc`（那会让系统再最大化一次，
//!   与前端按钮自身的一次点击叠起来就是「点一下闪两下」），改为在这里直接切换最大化。
//!
//! 点击为什么直接在本模块里做、不再转给前端：命中区里的鼠标消息到不了网页，只能由这边
//! 转一手；转给前端的做法中间多出「窗口过程 → IPC → 前端 → 窗口 API」三跳，每一跳都能
//! 静默失败，而失败的表现是「点下去窗口毫无反应」——最难归因的一种。直接调窗口 API 只剩
//! 一跳。副作用是悬停高亮（`window://max-button-hover`）与点击走两条不同的路，这是刻意的：
//! 高亮只是观感，晚一两帧无所谓；点击必须准。
//!
//! 悬停高亮：命中区一旦算作非客户区，网页就收不到鼠标消息，CSS `:hover` 自然失效。所以
//! 由这里把「指针是否停在按钮上」的变化转给前端，前端加类补上高亮。
//!
//! 平台边界：模块本身在所有平台都能编译（`install` / `set_button_rect` 是空实现），
//! 真正的 FFI 关在 `windows_hook` 里 —— 这样调用点不必散落 `#[cfg]`，macOS CI 也能
//! 顺带检查调用点。

/// 指针是否停在最大化按钮上（`bool` 载荷）。
#[cfg(target_os = "windows")]
pub(crate) const EVENT_MAX_BUTTON_HOVER: &str = "window://max-button-hover";

/// 前端上报最大化按钮矩形（CSS 像素、相对客户区左上角）。宽或高为 0 表示不参与贴靠命中。
pub(crate) fn set_button_rect(x: f64, y: f64, width: f64, height: f64) {
    #[cfg(target_os = "windows")]
    windows_hook::set_button_rect(x, y, width, height);

    #[cfg(not(target_os = "windows"))]
    let _ = (x, y, width, height);
}

/// 装窗口过程钩子。非 Windows 平台空实现（macOS 的覆盖式标题栏本来就有系统贴靠支持）。
pub(crate) fn install<R: tauri::Runtime>(window: &tauri::WebviewWindow<R>) {
    #[cfg(target_os = "windows")]
    windows_hook::install(window);

    #[cfg(not(target_os = "windows"))]
    let _ = window;
}

#[cfg(target_os = "windows")]
mod windows_hook {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};

    use tauri::{Emitter, Manager, Runtime};

    use super::EVENT_MAX_BUTTON_HOVER;

    // ── 最小 FFI：只声明用到的那几个 user32 函数 ────────────────────────
    //
    // 没有引 windows-sys / windows：这里要用的面只有四个函数，自己声明既不用跟着 crate
    // 版本迁移，也不会因为特性开关变化而编不过。
    #[link(name = "user32")]
    #[allow(non_snake_case)]
    unsafe extern "system" {
        fn SetWindowLongPtrW(hwnd: *mut c_void, index: i32, value: isize) -> isize;
        fn GetWindowLongPtrW(hwnd: *mut c_void, index: i32) -> isize;
        fn DefWindowProcW(hwnd: *mut c_void, msg: u32, wparam: usize, lparam: isize) -> isize;
        fn ScreenToClient(hwnd: *mut c_void, point: *mut Point) -> i32;
        fn GetDpiForWindow(hwnd: *mut c_void) -> u32;
        fn GetSystemMetricsForDpi(index: i32, dpi: u32) -> i32;
        /// 只用于诊断日志：`WM_NCHITTEST` 里 tao 只在「未最大化」时才做拖边缩放判定，
        /// 记下这一位才能区分「钩子没转发」和「转发了但窗口当时是最大化」。
        fn IsZoomed(hwnd: *mut c_void) -> i32;
    }

    #[repr(C)]
    struct Point {
        x: i32,
        y: i32,
    }

    const GWLP_WNDPROC: i32 = -4;
    const WM_NCHITTEST: u32 = 0x0084;
    const WM_NCLBUTTONDOWN: u32 = 0x00A1;
    const HTMAXBUTTON: isize = 9;
    /// 可缩放窗口边框的宽度/高度（`SM_CYFRAME` 与 `SM_CXFRAME` 同值），
    /// 也就是 tao 认定「上边缘仍属非客户区」的那一条带。
    const SM_CYFRAME: i32 = 32;

    type WndProc = unsafe extern "system" fn(*mut c_void, u32, usize, isize) -> isize;

    /// 原来的窗口过程；0 表示装钩失败，此时一律退回 `DefWindowProc`。
    static PREVIOUS: AtomicIsize = AtomicIsize::new(0);
    static INSTALLED: AtomicBool = AtomicBool::new(false);

    /// 最大化按钮矩形，CSS 像素。用原子量而不是 `Mutex`：`WM_NCHITTEST` 每次指针移动都会
    /// 被问一次，这条路径上不能有阻塞式加锁；四个分量各自无锁读写，最坏只是某一帧的命中区
    /// 略微偏移，不会拿到半个矩形。
    static BUTTON_X: AtomicU32 = AtomicU32::new(0);
    static BUTTON_Y: AtomicU32 = AtomicU32::new(0);
    static BUTTON_WIDTH: AtomicU32 = AtomicU32::new(0);
    static BUTTON_HEIGHT: AtomicU32 = AtomicU32::new(0);

    static HOVER: AtomicBool = AtomicBool::new(false);

    pub(super) fn set_button_rect(x: f64, y: f64, width: f64, height: f64) {
        // 这一行是「上报链路通不通」的唯一证据：命中判定拿不到矩形时，问题要么在这里
        // （值没送到），要么在窗口过程那边（值送到了但消息没来），两种情形光看命中结果分不开。
        trace(&format!("set rect x={x} y={y} w={width} h={height}"));
        if !(width > 0.0 && height > 0.0) {
            // 按钮不存在（例如非 Windows 布局上一次残留的上报）：清掉命中区
            store(&BUTTON_WIDTH, 0.0);
            store(&BUTTON_HEIGHT, 0.0);
            return;
        }
        store(&BUTTON_X, x);
        store(&BUTTON_Y, y);
        store(&BUTTON_WIDTH, width);
        store(&BUTTON_HEIGHT, height);
    }

    fn store(slot: &AtomicU32, value: f64) {
        slot.store((value as f32).to_bits(), Ordering::Relaxed);
    }

    fn load(slot: &AtomicU32) -> f32 {
        f32::from_bits(slot.load(Ordering::Relaxed))
    }

    fn button_rect() -> [f32; 4] {
        [
            load(&BUTTON_X),
            load(&BUTTON_Y),
            load(&BUTTON_WIDTH),
            load(&BUTTON_HEIGHT),
        ]
    }

    pub(super) fn install<R: Runtime>(window: &tauri::WebviewWindow<R>) {
        if INSTALLED.swap(true, Ordering::Relaxed) {
            return;
        }

        let hwnd = match window.hwnd() {
            Ok(hwnd) => hwnd.0,
            Err(error) => {
                eprintln!("[window] 取窗口句柄失败，贴靠布局不可用: {error}");
                return;
            }
        };
        if hwnd.is_null() {
            eprintln!("[window] 窗口句柄为空，贴靠布局不可用");
            return;
        }

        // 函数项要先转成指针再转整数：直接把函数项 `as usize` 是编译器明确不建议的隐式取地址
        let hooked_ptr = hooked as *const () as usize as isize;
        let previous = unsafe { SetWindowLongPtrW(hwnd, GWLP_WNDPROC, hooked_ptr) };
        if previous == 0 {
            eprintln!("[window] 替换窗口过程失败，贴靠布局不可用");
            return;
        }
        PREVIOUS.store(previous, Ordering::Relaxed);
        trace("hook installed");

        // 装钩之后再回读几次窗口过程：如果钩子被谁顶掉了（wry 的无边框缩放处理器、或别处
        // 的 SetWindowLongPtrW），症状是「日志里只有 hook installed」—— 看起来像「系统从没
        // 问过我们」，其实是链路被换了。这两行是唯一能把这两种情形分开的证据。
        //
        // 顺序有讲究：先做**不会阻塞**的事（直接读窗口过程、`eval` 是投递式的），
        // 可能阻塞的（`url()` 要走一趟事件循环）留到最后 —— 上一版把它放最前，
        // 结果整个诊断线程卡住，一行日志都没留下。
        let window = window.clone();
        // 裸指针不能跨线程，转成整数带过去再转回来（纯诊断用，句柄在此期间不会失效）
        let hwnd_value = hwnd as usize;
        std::thread::spawn(move || {
            let hwnd = hwnd_value as *mut c_void;
            for delay in [3u64, 6] {
                std::thread::sleep(std::time::Duration::from_secs(delay));
                let current = unsafe { GetWindowLongPtrW(hwnd, GWLP_WNDPROC) };
                trace(&format!(
                    "diagnose(累计{delay}s) wndproc=0x{current:X} hooked=0x{hooked_ptr:X} still_ours={}",
                    current == hooked_ptr
                ));
            }

            // 这里**不再**用 `window.eval` 去问页面自检：实测在这套 WebView2 环境里
            // `eval` 会成功返回、脚本却到不了页面（`ExecuteScript` 那一步不报错，命令就是
            // 不被调用，`k2a_diag` 里的 `eprintln!` 一行都等不到），整条路是死的。
            // 自检改由页面自己上报（前端 `reportShellReady()` 调 `k2a_diag`）。
            //
            // 剩下这两件事只有后端能问：URL 得从这里读，`GetWindowLongPtrW` 的回读也只有
            // 这里能发起。
            let url_window = window.clone();
            std::thread::spawn(move || {
                let url = url_window
                    .url()
                    .map(|value| value.to_string())
                    .unwrap_or_else(|error| format!("<取不到: {error}>"));
                trace(&format!("diagnose url={url}"));
            });
        });
    }

    /// 接管的窗口过程：只关心贴靠相关的那几条消息，其余原样转发。
    unsafe extern "system" fn hooked(
        hwnd: *mut c_void,
        msg: u32,
        wparam: usize,
        lparam: isize,
    ) -> isize {
        match msg {
            WM_NCHITTEST => {
                let hit = unsafe { hit_context(hwnd, lparam) };
                let over_button = hit.map(|h| h.over).unwrap_or(false);
                publish_hover(over_button);
                // 没命中按钮时走原有过程，并把**它**给出的结果记下来：
                // 上边缘期望 12(HTTOP)，真拿到 1(HTCLIENT) 时，「原过程没被调到」和
                // 「窗口当时是最大化（tao 在这个前提下本来就返回 DefWindowProc）」，
                // 靠 `result=` 与 `zoomed=` 两个字段就能分开。
                let result = if over_button {
                    HTMAXBUTTON
                } else {
                    unsafe { forward(hwnd, msg, wparam, lparam) }
                };
                if let Some(hit) = hit {
                    trace_nchittest(hwnd, hit, result);
                }
                return result;
            }
            WM_NCLBUTTONDOWN if wparam as isize == HTMAXBUTTON => {
                toggle_maximize_from_hook(hwnd);
                return 0;
            }
            _ => {}
        }

        unsafe { forward(hwnd, msg, wparam, lparam) }
    }

    /// 把消息交回原来的窗口过程（装钩失败时退回 `DefWindowProc`）。
    unsafe fn forward(hwnd: *mut c_void, msg: u32, wparam: usize, lparam: isize) -> isize {
        let previous = PREVIOUS.load(Ordering::Relaxed);
        if previous == 0 {
            return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
        }
        let original: WndProc = unsafe { std::mem::transmute(previous) };
        unsafe { original(hwnd, msg, wparam, lparam) }
    }

    /// 一次命中判定的全部中间量。既是判定结果，也是诊断日志要写的东西 ——
    /// 分开算两遍会在「日志显示的坐标」和「真正用到的坐标」之间留下偏差的口子。
    #[derive(Clone, Copy)]
    struct HitContext {
        /// 客户区坐标（物理像素）
        x: f32,
        y: f32,
        /// 是否落在最大化按钮矩形内
        over: bool,
        scale: f32,
        rect: [f32; 4],
    }

    /// 命中判定：把屏幕坐标换成客户区坐标，再与前端上报的矩形（按窗口 DPI 换算成物理像素）比较。
    ///
    /// DPI 每次现算（`GetDpiForWindow`）而不是上报时算 —— 窗口被拖到另一块不同缩放的显示器上，
    /// 缓存下来的物理坐标就全错了。
    ///
    /// 返回 `None` 表示这次没法判定（前端还没上报矩形 / 坐标换算失败），调用方按「未命中」处理。
    unsafe fn hit_context(hwnd: *mut c_void, lparam: isize) -> Option<HitContext> {
        let rect = button_rect();
        if rect[2] <= 0.0 || rect[3] <= 0.0 {
            trace_no_rect();
            return None;
        }

        // lParam 低 16 位是 x、高 16 位是 y，两者都是有符号的 16 位量
        //（等价于 GET_X_LPARAM / GET_Y_LPARAM 宏的展开）
        let mut point = Point {
            x: (lparam as i32) as i16 as i32,
            y: ((lparam >> 16) as i32) as i16 as i32,
        };
        if unsafe { ScreenToClient(hwnd, &mut point) } == 0 {
            return None;
        }

        let scale = unsafe { scale_factor(hwnd) };
        let left = rect[0] * scale;
        let top = rect[1] * scale;
        let (x, y) = (point.x as f32, point.y as f32);
        Some(HitContext {
            x,
            y,
            over: x >= left && x < left + rect[2] * scale && y >= top && y < top + rect[3] * scale,
            scale,
            rect,
        })
    }

    /// 窗口当前的缩放比例。`GetDpiForWindow` 需要 Windows 10 1607+，拿不到时按 100% 处理
    /// （Tauri 的 WebView2 最低就是 Win10，所以这里只是防御性分支）。
    unsafe fn scale_factor(hwnd: *mut c_void) -> f32 {
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        if dpi == 0 {
            1.0
        } else {
            dpi as f32 / 96.0
        }
    }

    /// 只在状态真的翻转时通知前端；`WM_NCHITTEST` 每秒会被问上百次，不能每次都打 IPC。
    fn publish_hover(hover: bool) {
        if HOVER.swap(hover, Ordering::Relaxed) == hover {
            return;
        }
        if let Some(app) = crate::APP_HANDLE.get() {
            let _ = app.emit(EVENT_MAX_BUTTON_HOVER, hover);
        }
    }

    /// 切换最大化状态（命中区里的左键按下）。
    ///
    /// 目标状态用 `IsZoomed` 问操作系统，而不是 tauri 的 `is_maximized()`：后者读的是 tao
    /// 自己缓存的标志位（`window_flags`），窗口若被系统以别的途径改过状态，那个标志可能滞后。
    /// 这里只需要知道「现在到底是不是最大化」，问系统最直接，而且不跨线程、不阻塞。
    ///
    /// 线程边界：本函数跑在窗口过程线程（也就是主线程）上。tauri 的窗口 API 在主线程上会
    /// **内联执行** —— `tauri-runtime-wry` 的 `send_user_message` 先比对线程 id，相同就直接
    /// 跑 `handle_user_message`，不经过事件循环代理。也就是说直接在窗口过程里调它，等于在处理
    /// `WM_NCLBUTTONDOWN` 的中途再进一次事件循环。开一条线程后，消息改由代理投递，落回干净的
    /// 上下文再执行；代价只是几毫秒，而按钮的悬停高亮已经先把「点到了」反馈出来了。
    fn toggle_maximize_from_hook(hwnd: *mut c_void) {
        let zoomed = unsafe { IsZoomed(hwnd) } != 0;
        trace(&format!("maxclick 命中，当前 IsZoomed={zoomed}"));

        let Some(app) = crate::APP_HANDLE.get().cloned() else {
            trace("maxclick 拿不到 AppHandle，放弃");
            return;
        };
        // 裸指针不能跨线程，转成整数带过去（纯诊断用，只读 IsZoomed，不做任何写操作）
        let hwnd_value = hwnd as usize;
        std::thread::spawn(move || {
            let Some(window) = app.get_webview_window(crate::window::MAIN_WINDOW) else {
                trace("maxclick 找不到主窗口，放弃");
                return;
            };
            let outcome = if zoomed {
                window.unmaximize()
            } else {
                window.maximize()
            };
            trace(&format!(
                "maxclick 调用 {} 返回={:?}",
                if zoomed { "unmaximize" } else { "maximize" },
                outcome.as_ref().err()
            ));
            // 必须回读：`maximize()` 是投递式的（tao 内部走 `execute_in_thread`），调用返回时
            // 窗口还没动。不回读的话，「调用了」和「生效了」在日志里长得一模一样。
            std::thread::sleep(std::time::Duration::from_millis(250));
            trace(&format!(
                "maxclick 之后 IsZoomed={} tauri_is_maximized={:?}",
                unsafe { IsZoomed(hwnd_value as *mut c_void) } != 0,
                window.is_maximized().map_err(|error| error.to_string())
            ));
        });
    }

    // ── 诊断（仅 K2A_WINDOW_TRACE=1 时写文件）──────────────────────────
    //
    // 贴靠浮出是个「系统行为」，出问题时光看代码判断不了是没命中、还是命中了但系统没浮出。
    // 这两行日志用最少的侵入把「窗口过程到底被问了没有、问的坐标是多少」记下来。

    fn tracing() -> bool {
        std::env::var_os("K2A_WINDOW_TRACE").is_some()
    }

    fn trace(line: &str) {
        if !tracing() {
            return;
        }
        append_trace(line);
    }

    /// 前端还没上报按钮矩形时也要留一行 —— 否则「钩子根本没被问到」和「被问到了但没矩形」
    /// 在日志里长得一模一样（都是什么都没有）。
    fn trace_no_rect() {
        if !tracing() {
            return;
        }
        if TRACE_BUDGET.fetch_sub(1, Ordering::Relaxed) == 0 {
            return;
        }
        append_trace("nchittest 无矩形（前端尚未上报最大化按钮位置）");
    }

    /// 每次 `WM_NCHITTEST` 记一行：客户区坐标、命中区、本次实际返回的命中码。
    ///
    /// `over=true` 的那几行说明「指针压在最大化按钮上，窗口过程确实被问到了」（贴靠浮出的前提）；
    /// `over=false` 的行则说明原过程给出的命中码 —— 上边缘那里 `result=12(HTTOP)` 才正常。
    /// `zoomed=` 与 `border=` 是为了排除 tao 自己的两个前提条件（它只在未最大化时才做拖边缩放判定）。
    fn trace_nchittest(hwnd: *mut c_void, hit: HitContext, result: isize) {
        if !tracing() {
            return;
        }
        if TRACE_BUDGET.fetch_sub(1, Ordering::Relaxed) == 0 {
            return;
        }
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        let border = unsafe { GetSystemMetricsForDpi(SM_CYFRAME, if dpi == 0 { 96 } else { dpi }) };
        let zoomed = unsafe { IsZoomed(hwnd) } != 0;
        append_trace(&format!(
            "hit client=({},{}) rect={:?} scale={} over={} result={} zoomed={zoomed} border={border}",
            hit.x, hit.y, hit.rect, hit.scale, hit.over, result
        ));
    }

    /// 前若干次命中判定的采样预算，避免把日志刷爆（只做诊断，正常路径不读这个值）。
    static TRACE_BUDGET: AtomicU32 = AtomicU32::new(120);

    fn append_trace(line: &str) {
        use std::io::Write;
        let path = crate::paths::get_data_path().join("window-trace.log");
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = writeln!(
                file,
                "{} {line}",
                chrono::Local::now().format("%H:%M:%S%.3f")
            );
        }
    }
}
