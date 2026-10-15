//! 诊断日志：追加写入 App 数据目录下的 `kiro-debug.log`（见 `paths::get_debug_log_path`）。
//!
//! 打包后的 GUI 进程没有控制台，`eprintln!` 的输出谁也看不见 —— 排查「刷新凭据超时」
//! 这类问题需要的时间线只有落到文件里才有用。写失败不影响调用方：日志永远只是日志，
//! 宁可缺一行，也不能让记日志这件事把业务打断。排查完整个文件可以直接删除。
//!
//! 时间戳带毫秒：这里记的就是「各阶段耗时」，秒级精度会让相邻两行对不上账。

use std::io::Write;
use std::time::Duration;

pub(crate) fn log(line: &str) {
    let path = crate::paths::get_debug_log_path();
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(
            f,
            "{} {line}",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f")
        );
    }
}

/// 时长 → `1.23s`。各阶段耗时的统一展示格式。
pub(crate) fn secs(duration: Duration) -> String {
    format!("{:.2}s", duration.as_secs_f64())
}
