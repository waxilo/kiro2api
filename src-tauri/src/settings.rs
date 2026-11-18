//! 运行期默认值：代理替客户端补的那一档思考级别，以及 Codex 目录里的上下文大小。
//!
//! 这两个值都不是「本应用自己的偏好」，而是**我们对外声明的能力**：
//! 前者决定客户端没写 `effort` 时上游拿到什么档位，后者决定 Codex 认为还有多少上下文可用。
//! 所以它们必须可配，而且配完要能被两个消费方同时看到 —— `kiro::transform`（每个请求）
//! 与 `codex`（生成模型目录时）。
//!
//! ## 三层取值顺序
//!
//! 1. `config.json` 里用户选过的那一项（[`prefs::AppConfig`]）；
//! 2. 内置默认（[`DEFAULT_EFFORT`] / [`DEFAULT_CONTEXT`]）；
//! 3. 值本身还得过校验 —— 配置里写着已下线的档位时按「没配」处理，绝不原样发上游。
//!
//! ## 为什么要有进程内快照
//!
//! `transform` 每来一个请求就要问一次默认档，`codex` 生成目录也要问上下文大小。
//! 每次都读盘会把「读一个默认值」变成磁盘 IO 热路径，而且 `prefs::update_config` 带锁，
//! 代理线程与命令线程会互相等。所以进程内只留一份 [`RwLock`] 快照，写配置时同步更新。
//!
//! 快照**只**在 `lib.rs::setup` 里用磁盘配置初始化，不在读路径上懒加载：
//! 单元测试因此永远看到内置默认，不会因为开发机上 `config.json` 里选过什么而漂。

use std::sync::{OnceLock, RwLock};

use serde::Serialize;

use crate::kiro::models::DEFAULT_EFFORT;
use crate::prefs;

/// 思考级别的候选档位，写法与 Kiro 的枚举一致（顺序即从省到强）。
pub(crate) const EFFORT_OPTIONS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

/// 上下文大小的候选档位：每 100K 一档，200K 起、1M 止。
pub(crate) const CONTEXT_OPTIONS: &[u32] = &[
    200_000, 300_000, 400_000, 500_000, 600_000, 700_000, 800_000, 900_000, 1_000_000,
];

/// 上下文大小的内置默认：300K。
pub(crate) const DEFAULT_CONTEXT: u32 = 300_000;

/// 档位说明。文案与 Codex 目录里那三条保持一致，用户在两处看到的是同一套说法。
fn effort_label(effort: &str) -> &'static str {
    match effort {
        "low" => "更快",
        "medium" => "均衡",
        "high" => "更深入",
        "xhigh" => "极深入",
        "max" => "最强",
        _ => "",
    }
}

/// 上下文档位的展示文案：`300000` → `300K`，整百万 → `1M`。
fn context_label(tokens: u32) -> String {
    if tokens % 1_000_000 == 0 {
        format!("{}M", tokens / 1_000_000)
    } else {
        format!("{}K", tokens / 1_000)
    }
}

/// 落盘的档位归一化：只认候选表里的写法，其余返回 `None`（= 当作没配）。
fn normalize_effort(value: Option<&str>) -> Option<String> {
    let value = value?.trim().to_lowercase();
    EFFORT_OPTIONS
        .iter()
        .find(|option| **option == value)
        .map(|option| (*option).to_string())
}

#[derive(Debug, Clone)]
struct RuntimeSettings {
    default_effort: String,
    context_window: u32,
}

impl Default for RuntimeSettings {
    fn default() -> Self {
        Self {
            default_effort: DEFAULT_EFFORT.to_string(),
            context_window: DEFAULT_CONTEXT,
        }
    }
}

fn cell() -> &'static RwLock<RuntimeSettings> {
    static CELL: OnceLock<RwLock<RuntimeSettings>> = OnceLock::new();
    CELL.get_or_init(|| RwLock::new(RuntimeSettings::default()))
}

fn read() -> RuntimeSettings {
    // 锁中毒只可能来自「持锁线程 panic」，此时快照仍是最后写成功的值，
    // 用它继续服务比让整个代理不可用更合理。
    cell()
        .read()
        .map(|guard| guard.clone())
        .unwrap_or_default()
}

fn write(update: impl FnOnce(&mut RuntimeSettings)) {
    if let Ok(mut guard) = cell().write() {
        update(&mut guard);
    }
}

/// 把磁盘配置折算成运行期快照：认得出的值照用，认不出的按「没配」处理。
///
/// 纯函数 —— 只做折算、不碰全局快照，这样单元测试可以直接断言折算结果，
/// 不必去改进程内的共享状态（并行跑的用例会互相踩）。
fn resolve_config(config: &prefs::AppConfig) -> RuntimeSettings {
    RuntimeSettings {
        default_effort: normalize_effort(config.default_effort.as_deref())
            .unwrap_or_else(|| DEFAULT_EFFORT.to_string()),
        context_window: config
            .context_window
            .filter(|tokens| CONTEXT_OPTIONS.contains(tokens))
            .unwrap_or(DEFAULT_CONTEXT),
    }
}

/// 用磁盘上的配置初始化快照。只应在应用启动时调用一次。
///
/// 认不出来的档位（旧版本写过、手工改坏）按「没配」处理，**不回写**磁盘 ——
/// 启动过程不该改动用户的配置文件，留着他自己能在界面上重新选。
pub(crate) fn init_from_config(config: &prefs::AppConfig) {
    let resolved = resolve_config(config);
    write(|settings| *settings = resolved);
}

/// 当前默认思考级别（客户端没指定时补上的那一档）。
pub(crate) fn default_effort() -> String {
    read().default_effort
}

/// 当前默认上下文大小（token）。
pub(crate) fn context_window() -> u32 {
    read().context_window
}

/// 改默认思考级别：校验 → 落盘 → 更新快照。任一步失败都不改动现有值。
pub(crate) fn set_default_effort(effort: &str) -> Result<(), String> {
    let normalized = normalize_effort(Some(effort)).ok_or_else(|| {
        format!(
            "不支持的思考级别「{effort}」，可选：{}",
            EFFORT_OPTIONS.join(" / ")
        )
    })?;
    prefs::update_config(|config| {
        config.default_effort = Some(normalized.clone());
        Ok(())
    })?;
    write(|settings| settings.default_effort = normalized);
    Ok(())
}

/// 改默认上下文大小：校验 → 落盘 → 更新快照。
pub(crate) fn set_context_window(tokens: u32) -> Result<(), String> {
    if !CONTEXT_OPTIONS.contains(&tokens) {
        return Err(format!(
            "不支持的上下文大小「{tokens}」，可选：{}",
            CONTEXT_OPTIONS
                .iter()
                .map(|option| context_label(*option))
                .collect::<Vec<_>>()
                .join(" / ")
        ));
    }
    prefs::update_config(|config| {
        config.context_window = Some(tokens);
        Ok(())
    })?;
    write(|settings| settings.context_window = tokens);
    Ok(())
}

// ── 前端契约 ────────────────────────────────────────────────────────────
//
// 单选组的形状由后端给：选项表只在这里存在一份，前端不再抄一遍档位与文案。

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PrefOption<T> {
    pub(crate) value: T,
    pub(crate) label: String,
    /// 跟在中文说明后面的英文标识（思考级别的档位名 `high` 这类）。
    ///
    /// 上游与客户端都在用英文档位名，用户对着客户端配置时得能一一对上号，
    /// 所以中文说明旁边总要留一个。纯数字的上下文档位没有这一截，字段直接不输出。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) hint: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PrefChoice<T> {
    /// 当前选中的值。
    pub(crate) value: T,
    pub(crate) options: Vec<PrefOption<T>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PreferencesView {
    pub(crate) default_effort: PrefChoice<String>,
    pub(crate) context_window: PrefChoice<u32>,
}

fn view() -> PreferencesView {
    let current = read();
    PreferencesView {
        default_effort: PrefChoice {
            value: current.default_effort.clone(),
            options: EFFORT_OPTIONS
                .iter()
                .map(|effort| PrefOption {
                    value: (*effort).to_string(),
                    label: effort_label(effort).to_string(),
                    hint: Some((*effort).to_string()),
                })
                .collect(),
        },
        context_window: PrefChoice {
            value: current.context_window,
            options: CONTEXT_OPTIONS
                .iter()
                .map(|tokens| PrefOption {
                    value: *tokens,
                    label: context_label(*tokens),
                    hint: None,
                })
                .collect(),
        },
    }
}

#[tauri::command]
pub(crate) fn k2a_preferences_get() -> PreferencesView {
    view()
}

#[tauri::command]
pub(crate) fn k2a_set_default_effort(effort: String) -> Result<PreferencesView, String> {
    set_default_effort(&effort)?;
    // 已接管的 Codex 目录里也写着这一档：让它就位，用户不必再点一次「应用」。
    if let Err(error) = crate::codex::refresh_catalog_if_ours() {
        eprintln!("[settings] 刷新 Codex 模型目录失败: {error}");
    }
    Ok(view())
}

#[tauri::command]
pub(crate) fn k2a_set_context_window(tokens: u32) -> Result<PreferencesView, String> {
    set_context_window(tokens)?;
    if let Err(error) = crate::codex::refresh_catalog_if_ours() {
        eprintln!("[settings] 刷新 Codex 模型目录失败: {error}");
    }
    Ok(view())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 没配过时用内置默认：high + 300K。
    #[test]
    fn defaults_are_high_and_300k() {
        let resolved = resolve_config(&prefs::AppConfig::default());
        assert_eq!(resolved.default_effort, "high");
        assert_eq!(resolved.context_window, 300_000);
        // 进程内快照的初值也必须与之一致（单测不初始化，读到的就是它）
        assert_eq!(default_effort(), "high");
        assert_eq!(context_window(), 300_000);
    }

    /// 配置里的合法值要生效；首尾空白与大小写不该让值失效。
    #[test]
    fn resolve_accepts_valid_values() {
        let config = prefs::AppConfig {
            default_effort: Some("  XHIGH ".to_string()),
            context_window: Some(1_000_000),
            ..prefs::AppConfig::default()
        };
        let resolved = resolve_config(&config);
        assert_eq!(resolved.default_effort, "xhigh");
        assert_eq!(resolved.context_window, 1_000_000);
    }

    /// 配置里写着不认识的档位 / 不在候选表里的上下文大小 → 回落内置默认，
    /// 而不是把脏值原样发给上游。
    #[test]
    fn resolve_falls_back_on_unknown_values() {
        let config = prefs::AppConfig {
            default_effort: Some("ultra".to_string()),
            context_window: Some(123_456),
            ..prefs::AppConfig::default()
        };
        let resolved = resolve_config(&config);
        assert_eq!(resolved.default_effort, "high");
        assert_eq!(resolved.context_window, 300_000);
    }

    /// 非法值直接被拒，且拒绝发生在写盘与改快照之前。
    ///
    /// 只测拒绝路径：成功的 set 会真的写 `config.json`，测试不该动用户的配置文件。
    #[test]
    fn invalid_values_are_rejected_before_any_write() {
        assert!(set_context_window(250_000).is_err());
        assert!(set_default_effort("none").is_err());
        assert!(set_default_effort("").is_err());
        // 快照仍是内置默认，没被上面的失败调用改坏
        assert_eq!(default_effort(), "high");
        assert_eq!(context_window(), 300_000);
    }

    /// 候选表：思考级别五档，上下文每 100K 一档、200K 起步、1M 封顶。
    #[test]
    fn option_tables_cover_the_agreed_ranges() {
        assert_eq!(EFFORT_OPTIONS, &["low", "medium", "high", "xhigh", "max"]);
        assert_eq!(CONTEXT_OPTIONS.first(), Some(&200_000));
        assert_eq!(CONTEXT_OPTIONS.last(), Some(&1_000_000));
        assert!(CONTEXT_OPTIONS.windows(2).all(|pair| pair[1] - pair[0] == 100_000));
    }

    /// 英文注释：思考级别每档都带自己的档位名（与上游、客户端写法一致），
    /// 上下文档位是纯数字，不带。
    #[test]
    fn effort_options_carry_their_english_name() {
        let view = view();
        for option in &view.default_effort.options {
            assert_eq!(option.hint.as_deref(), Some(option.value.as_str()));
        }
        assert!(view
            .context_window
            .options
            .iter()
            .all(|option| option.hint.is_none()));
    }

    /// 展示文案：整百万用 M，其余用 K。
    #[test]
    fn context_labels_stay_short() {
        assert_eq!(context_label(200_000), "200K");
        assert_eq!(context_label(900_000), "900K");
        assert_eq!(context_label(1_000_000), "1M");
    }

    /// 前端的选项表就是这两份常量：值 + 文案，缺一不可。
    #[test]
    fn view_exposes_every_option_with_a_label() {
        let view = view();
        assert_eq!(view.default_effort.options.len(), EFFORT_OPTIONS.len());
        assert_eq!(view.context_window.options.len(), CONTEXT_OPTIONS.len());
        assert!(view.default_effort.options.iter().all(|option| !option.label.is_empty()));
        assert_eq!(view.context_window.options.last().map(|option| option.label.as_str()), Some("1M"));
        // 当前值必须落在候选表里，否则界面会出现「一个都没选中」
        assert!(view.default_effort.options.iter().any(|option| option.value == view.default_effort.value));
        assert!(view.context_window.options.iter().any(|option| option.value == view.context_window.value));
    }
}
