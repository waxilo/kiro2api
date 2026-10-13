//! Windows 子进程控制台窗口抑制。
//!
//! Tauri GUI 进程本身无控制台；在此进程里 spawn 子进程（git / kiro-cli / claude 探测等）
//! 若不指定 `CREATE_NO_WINDOW`，Windows 会为子进程新建一个控制台窗口——表现为
//! 使用中偶尔弹出黑色终端框一闪而过。所有后台子进程都应经 `suppress_console_window`
//! 应用该标志（用户主动「在外部终端打开」的路径除外，那需要可见窗口）。

use std::process::{Child, Command, ExitStatus};
use std::time::{Duration, Instant};

/// 为 Command 应用「不创建控制台窗口」标志（`CREATE_NO_WINDOW`，0x08000000）。
/// 非 Windows 平台为空操作。
pub(crate) fn suppress_console_window(cmd: &mut Command) {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    let _ = cmd;
}

/// 等子进程退出，到点就杀掉。返回 `None` 表示超时（已 kill 并回收）。
///
/// 为什么必须有：`Child::wait()` 没有上限。给一个会走网络、可能弹登录、或者干脆是
/// 一整个 Electron 运行时（本机的 `kiro` 就是）的命令用 `wait()`，只要它不退出，调用方
/// 就永远不返回 —— 凭据刷新跑在 `spawn_blocking` 里，前端的 `busy` 于是永久不解除，
/// 所有 busy 门控的按钮一起灰死，只能重启应用。
///
/// 宁可放弃这次「轻触」，也不能没有上限：拿不到结果比拿到「超时」更糟。
pub(crate) fn wait_with_timeout(child: &mut Child, timeout: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    // kill 之后必须 wait 一次，否则留下僵尸进程
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return None,
        }
    }
}
