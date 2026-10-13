//! 平台专用配置的解析校验。
//!
//! 为什么需要这条测试：`tauri.macos.conf.json` 只在 macOS 目标下才会被读取，在 Windows 上
//! 无论是 `cargo check` 还是打包都完全看不到它 —— 一个拼错的键能一路通过本地全部验证，
//! 直到 macOS CI 才炸。这里用 Tauri 自己的解析器把三个目标各读一遍，把「读不到」变成「本机就红」。
//!
//! 顺带锁住第二条约束：平台配置与主配置按 RFC 7396（JSON Merge Patch）合并，**数组是整体替换
//! 而不是逐项合并**，所以平台配置里的 `app.windows` 必须整段重写。一旦漏写字段，该平台会静默
//! 丢掉那些设置（例如 width/height 回落到默认值，窗口尺寸在两台机器上就不一样了）。

use std::path::Path;

use tauri::utils::config::{parse::read_from, Config};
use tauri::utils::platform::Target;
use tauri::utils::TitleBarStyle;

const TAURI_DIR: &str = env!("CARGO_MANIFEST_DIR");

fn load(target: Target) -> Config {
    let (value, paths) = read_from(target, Path::new(TAURI_DIR))
        .unwrap_or_else(|error| panic!("读取 {target:?} 的配置失败: {error}"));
    serde_json::from_value(value).unwrap_or_else(|error| {
        let used = paths
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        panic!("{target:?} 的配置无法反序列化: {error}\n实际用到的文件: {used}")
    })
}

#[test]
fn every_target_config_parses() {
    for target in [Target::Windows, Target::MacOS, Target::Linux] {
        let config = load(target);
        assert_eq!(
            Some("kiro2api"),
            config.product_name.as_deref(),
            "{target:?} 的产品名被改掉了"
        );
        assert_eq!(1, config.app.windows.len(), "{target:?} 应当只有一个主窗口");
    }
}

#[test]
fn window_geometry_is_identical_on_every_target() {
    // 平台配置整段替换 app.windows：这里逐个字段对齐，防止某个平台悄悄回落到默认值。
    let reference = load(Target::Windows);
    let reference = &reference.app.windows[0];

    for target in [Target::MacOS, Target::Linux] {
        let config = load(target);
        let window = &config.app.windows[0];
        assert_eq!(reference.title, window.title, "{target:?} 的标题不一致");
        assert_eq!(reference.width, window.width, "{target:?} 的宽度不一致");
        assert_eq!(reference.height, window.height, "{target:?} 的高度不一致");
        assert_eq!(
            reference.min_width, window.min_width,
            "{target:?} 的最小宽度不一致"
        );
        assert_eq!(
            reference.min_height, window.min_height,
            "{target:?} 的最小高度不一致"
        );
        assert_eq!(
            reference.resizable, window.resizable,
            "{target:?} 的可缩放性不一致"
        );
    }
}

#[test]
fn decorations_follow_the_platform_convention() {
    // Windows 走全自绘：去掉系统装饰，阴影必须留着（无边框窗口靠它跟桌面分层）。
    let windows = load(Target::Windows);
    let window = &windows.app.windows[0];
    assert!(!window.decorations, "Windows 应当是无系统装饰的自绘窗口");
    assert!(window.shadow, "无边框窗口必须保留 DWM 阴影");

    // macOS 保留原生装饰 + 透明覆盖式标题栏：圆角、阴影、红绿灯都由系统提供，
    // 前端只画标题栏自身的像素。
    let macos = load(Target::MacOS);
    let window = &macos.app.windows[0];
    assert!(window.decorations, "macOS 需要保留原生窗口装饰");
    assert_eq!(
        TitleBarStyle::Overlay,
        window.title_bar_style,
        "macOS 应当使用覆盖式标题栏"
    );
    assert!(
        window.traffic_light_position.is_some(),
        "覆盖式标题栏下红绿灯位置必须显式给出，否则它不会落在自绘标题栏的垂直中线上"
    );
}
