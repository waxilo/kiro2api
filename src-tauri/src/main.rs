// Windows 发布版不弹控制台窗口（`windows_subsystem = "windows"`）；
// debug 构建保留控制台，便于看 eprintln 诊断输出。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    kiro2api_lib::run()
}
