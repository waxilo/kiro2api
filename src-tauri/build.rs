fn main() {
    // 图标换了要重新链接：tauri-build 默认不为 icons/ 声明 rerun-if-changed，
    // 只改图标不改代码时 exe 里仍是旧图标（任务栏/托盘看着像「换了个寂寞」）。
    println!("cargo:rerun-if-changed=icons");
    tauri_build::build()
}
