//! 模型名映射、effort 归一化与 token 估算。
//! 移植自 kiro2cli/src/models.js + utils.js 中的 estimateTokens（MIT）。

use serde_json::Value;

/// Kiro 内置模型名（public 名称）。
pub fn is_known_kiro_model(model: &str) -> bool {
    matches!(
        model,
        "auto"
            | "claude-sonnet-5"
            | "claude-opus-5"
            | "claude-opus-4.8"
            | "gpt-5.6-sol"
            | "gpt-5.6-terra"
            | "gpt-5.6-luna"
            | "claude-opus-4.7"
            | "claude-opus-4.6"
            | "claude-sonnet-4.6"
            | "claude-opus-4.5"
            | "claude-sonnet-4.5"
            | "claude-sonnet-4"
            | "claude-haiku-4.5"
            | "deepseek-3.2"
            | "minimax-m2.5"
            | "minimax-m2.1"
            | "glm-5"
            | "qwen3-coder-next"
    )
}

/// Claude 各代的 effort 枚举（上游 schema 拿不到时的回退值）。
const CLAUDE_EFFORT: &[&str] = &["low", "medium", "high", "xhigh", "max"];/// 4.6 两代没有 xhigh 这一档。
const CLAUDE_46_EFFORT: &[&str] = &["low", "medium", "high", "max"];
/// GPT 系列多一个 none。
const GPT_EFFORT: &[&str] = &["none", "low", "medium", "high", "xhigh", "max"];

/// 调用方没指定推理级别时由代理补上的档位，也是 Codex 模型目录里的默认档
/// （见 `codex::catalog_entry`）—— 两处必须同源，否则「Codex 默认 high、
/// 代理默认别的」会在客户端与代理之间来回打架。
///
/// `high` 能覆盖当前所有接受 effort 的型号：Claude 各代（`low`…`max`）、
/// GPT 系列（多一个 `none`）都含这一档。模型枚举里没有 `high` 时，补默认值的
/// 那条路径退到该模型的最高档（见 [`highest_effort`]）；完全不收 effort 的模型
/// 则整块省略，交给上游自己的默认。
pub const DEFAULT_EFFORT: &str = "high";

/// 模型支持的 effort 级别（none/low/medium/high/xhigh/max）。
///
/// Claude 走「按形状识别」而不是白名单：上游不定期上新（`claude-opus-5.5` 等），
/// 白名单漏掉一个型号就会让 [`effort_levels_for_model`] 变成空集 —— 后果不只是 effort 被丢掉，
/// 更糟的是**任何原样合入的 effort 都失去了校验依据**，脏值会直接打到上游报
/// `does not have a value in the enumeration [...]`。带日期后缀的公共 ID 有 6~8 位
/// 版本号，被 [`looks_like_kiro_claude_model`] 挡掉，仍回退为空集。
/// 上游 `supportedInputTypes` 缺失时（离线、旧缓存、未知型号）的输入模态兜底。
///
/// 按模型家族判而不是逐个型号：Claude 与 GPT 全系原生支持图片输入，Grok 当前也是
/// text+image；其余（deepseek / glm / minimax / qwen）上游声明为纯文本。
/// 上游给过真实值时一律以真实值为准，这里只服务于离线生成模型目录。
pub fn default_input_modalities(model: &str) -> Vec<String> {
    let supports_image = model.starts_with("claude-")
        || model.starts_with("gpt-")
        || model.starts_with("grok-");
    if supports_image {
        vec!["text".to_string(), "image".to_string()]
    } else {
        vec!["text".to_string()]
    }
}

pub fn effort_levels_for_model(kiro_model: &str) -> &'static [&'static str] {
    if matches!(kiro_model, "gpt-5.6-sol" | "gpt-5.6-terra" | "gpt-5.6-luna") {
        return GPT_EFFORT;
    }
    if !looks_like_kiro_claude_model(kiro_model) {
        return &[];
    }
    if matches!(kiro_model, "claude-opus-4.6" | "claude-sonnet-4.6") {
        CLAUDE_46_EFFORT
    } else {
        CLAUDE_EFFORT
    }
}

pub fn kiro_model_to_public_model_id(model_id: &str) -> String {
    if model_id == "claude-sonnet-4" {
        return "claude-sonnet-4-0".to_string();
    }
    if let Some((base, minor)) = model_id.split_once('.') {
        let parts: Vec<&str> = base.split('-').collect();
        if parts.len() == 3
            && parts[0] == "claude"
            && matches!(parts[1], "opus" | "sonnet" | "haiku")
            && parts[2] == "4"
        {
            return format!("{}-{}-{}-{}", parts[0], parts[1], parts[2], minor);
        }
    }
    model_id.to_string()
}

pub fn public_model_id_to_kiro_model(model_id: &str) -> String {
    if model_id == "claude-sonnet-4-0" {
        return "claude-sonnet-4".to_string();
    }
    let parts: Vec<&str> = model_id.split('-').collect();
    if parts.len() == 4
        && parts[0] == "claude"
        && matches!(parts[1], "opus" | "sonnet" | "haiku")
        && parts[2] == "4"
    {
        return format!("claude-{}-4.{}", parts[1], parts[3]);
    }
    model_id.to_string()
}

pub fn public_display_name(name: &str) -> String {
    name.strip_prefix("Claude ").unwrap_or(name).to_string()
}

fn get_effort_schema(schema: &Value) -> Option<&Value> {
    schema
        .get("properties")
        .and_then(|p| p.get("output_config"))
        .and_then(|oc| oc.get("properties"))
        .and_then(|p| p.get("effort"))
        .or_else(|| {
            schema
                .get("properties")
                .and_then(|p| p.get("reasoning"))
                .and_then(|r| r.get("properties"))
                .and_then(|p| p.get("effort"))
        })
}

pub fn get_supported_effort_levels(schema: &Value) -> Vec<String> {
    get_effort_schema(schema)
        .and_then(|effort| effort.get("enum"))
        .and_then(|values| values.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// 把请求里的 effort 收敛到「模型真正接受的枚举值」，是 effort 的唯一出口。
///
/// 返回 `None` 的语义是「不要发这个字段」，而不是「发个默认值」—— 上游对枚举校验
/// 是严格的，宁可省略也不能赌一个没被接受的字面量。`levels` 为空表示该模型不接受
/// effort，任何取值都应丢弃。
pub fn normalize_effort(effort: Option<&str>, levels: &[String]) -> Option<String> {
    let effort = effort?;
    if levels.is_empty() {
        return None;
    }
    if levels.iter().any(|level| level == effort) {
        return Some(effort.to_string());
    }
    // `xhigh` 是最高档的写法之一；只支持 max 的模型收敛过去，反之亦然。
    if effort == "xhigh" && levels.iter().any(|level| level == "max") {
        return Some("max".to_string());
    }
    if effort == "max" && !levels.iter().any(|level| level == "max") {
        if levels.iter().any(|level| level == "xhigh") {
            return Some("xhigh".to_string());
        }
        if levels.iter().any(|level| level == "high") {
            return Some("high".to_string());
        }
    }
    None
}

/// 档位的语义顺序（由弱到强）。
///
/// 不能拿 `levels` 的数组顺序当高低：那份 enum 直接来自上游 schema，写成什么顺序都有。
const EFFORT_RANK: &[&str] = &["none", "low", "medium", "high", "xhigh", "max"];

/// 模型实际支持的最高一档。
///
/// 用途只有一个：用户配的默认档不在这个模型的枚举里时，退到它能给的最强档
/// （见 `transform::resolve_effort`）。`levels` 为空说明模型压根不接受 effort，
/// 返回 `None` —— 此时发任何取值都只会换回 502，不能硬塞。
pub fn highest_effort(levels: &[String]) -> Option<String> {
    EFFORT_RANK
        .iter()
        .rev()
        .copied()
        .find(|rank| levels.iter().any(|level| level.as_str() == *rank))
        .map(str::to_string)
}

pub fn normalize_effort_level(value: Option<&str>) -> Option<String> {
    let value = value?;
    let normalized: String = value
        .to_lowercase()
        .chars()
        .filter(|ch| !matches!(ch, '_' | ' ' | '-' | '\t'))
        .collect();
    let mapped = match normalized.as_str() {
        "none" => "none",
        "low" => "low",
        "medium" | "med" => "medium",
        "high" => "high",
        "xhigh" | "extrahigh" => "xhigh",
        "max" | "maximum" => "max",
        _ => return None,
    };
    Some(mapped.to_string())
}

/// 从请求体中提取用户请求的 effort（兼容各种字段名）。
pub fn get_requested_effort(body: &Value) -> Option<String> {
    let candidates = [
        body.get("effort"),
        body.get("effort_level"),
        body.get("reasoning_effort"),
        body.get("reasoning").and_then(|r| r.get("effort")),
        body.get("output_config").and_then(|oc| oc.get("effort")),
        body.get("additionalModelRequestFields")
            .and_then(|f| f.get("output_config"))
            .and_then(|oc| oc.get("effort")),
        body.get("additional_model_request_fields")
            .and_then(|f| f.get("output_config"))
            .and_then(|oc| oc.get("effort")),
    ];
    for candidate in candidates.into_iter().flatten() {
        if let Some(value) = candidate.as_str() {
            if let Some(normalized) = normalize_effort_level(Some(value)) {
                return Some(normalized);
            }
        }
    }
    None
}

/// 请求里是否**出现过** effort 字段（不看取值认不认得）。
///
/// 用来区分「没提推理级别」和「提了但写法不认识」：前者由代理补
/// [`DEFAULT_EFFORT`]，后者保持既有语义（整块省略）。少了这道判断，
/// `additionalModelRequestFields.effort = "none"` 这类字面量会被当成「没提」，
/// 于是被悄悄升级成默认档 —— 恰恰是「显式关思考反被开到 high」那种事故。
pub fn effort_is_specified(body: &Value) -> bool {
    fn present(body: &Value, path: &[&str]) -> bool {
        let mut current = body;
        for key in path {
            match current.get(*key) {
                Some(next) => current = next,
                None => return false,
            }
        }
        !current.is_null()
    }

    let paths: [&[&str]; 11] = [
        &["effort"],
        &["effort_level"],
        &["reasoning_effort"],
        &["reasoning", "effort"],
        &["output_config", "effort"],
        &["additionalModelRequestFields", "effort"],
        &["additionalModelRequestFields", "output_config", "effort"],
        &["additionalModelRequestFields", "reasoning", "effort"],
        &["additional_model_request_fields", "effort"],
        &["additional_model_request_fields", "output_config", "effort"],
        &["additional_model_request_fields", "reasoning", "effort"],
    ];
    paths.iter().any(|path| present(body, path))
}

/// Kiro 的 Claude 模型命名形如 `claude-opus-5` / `claude-opus-4.8`。
///
/// 为什么需要这个「按形状识别」的补充判据：上游会不定期上新（实测上游已提供
/// `claude-opus-5.5` / `claude-sonnet-5.5`，而 [`is_known_kiro_model`] 的白名单
/// 只列到 5）。若只认白名单，用户在选择器里选中的新模型会被后面的兜底逻辑
/// **静默降级**成旧型号 —— 请求成功、答案看起来正常，但用的不是你选的模型，
/// 这类问题最难被发现。按形状识别让新版本直接透传。
///
/// 同时必须避免误伤带日期后缀的 Anthropic 公共 ID（如 `claude-3-5-sonnet-20241022`）：
/// 那些确实是 Kiro 不认识的模型，仍需走兜底映射到同族模型。
///
/// 注意这里还有一个容易踩的坑：带日期的公共 ID 会先被 [`public_model_id_to_kiro_model`]
/// 改写成 `claude-sonnet-4.20250514`，形状上与真实 Kiro 型号 `claude-sonnet-4.6` 撞车。
/// 因此版本段必须限制位数 —— Kiro 的版本号是个位数或两位（`5`、`4.8`），
/// 而日期后缀有 6~8 位。
fn looks_like_kiro_claude_model(model: &str) -> bool {
    /// Kiro 主/次版本号的位数上限；日期后缀（6~8 位）会被这一条挡掉。
    const MAX_VERSION_DIGITS: usize = 2;

    let Some(rest) = model.strip_prefix("claude-") else {
        return false;
    };
    let Some((family, version)) = rest.split_once('-') else {
        return false;
    };
    if !matches!(family, "opus" | "sonnet" | "haiku") {
        return false;
    }
    let is_short_number = |text: &str| {
        !text.is_empty()
            && text.len() <= MAX_VERSION_DIGITS
            && text.chars().all(|c| c.is_ascii_digit())
    };
    match version.split_once('.') {
        Some((major, minor)) => is_short_number(major) && is_short_number(minor),
        None => is_short_number(version),
    }
}

/// 归一化请求中的模型名为 Kiro 内部模型名。
///
/// **只做形态转换，不做任何猜测**：
/// - `KIRO_MODEL_OVERRIDE` 环境变量优先（显式覆盖）；
/// - `kiro:` 前缀剥离；
/// - Anthropic 公共 ID → Kiro 私有 ID（`claude-sonnet-4-5` → `claude-sonnet-4.5`）；
/// - **同代**的日期后缀 ID 去掉后缀（`claude-sonnet-4-5-20250929` → `claude-sonnet-4.5`）；
/// - 空值 → `default`。
///
/// 认不出的名字**原样返回**：「能不能用」由调用方拿账户可用清单判定，
/// 不可用就明确报 400（见 `server::resolve_kiro_model`）。
///
/// 这里曾经按关键字猜（含 `opus` → `claude-opus-5`、含 `haiku` → `claude-haiku-4.5`、
/// 含 `sonnet`/`claude-*` → `claude-sonnet-5`、都不认 → 默认模型），那是**静默降级**的
/// 主要来源：跨代甚至跨族换模型，请求成功、答案正常，只是花的不是你选的模型的额度。
/// 2026-11-20 按用户要求移除，只留下同代映射（Claude Code 不配模型时会发带日期的 ID）。
pub fn normalize_kiro_model(model: Option<&str>, default: &str) -> String {
    if let Ok(override_model) = std::env::var("KIRO_MODEL_OVERRIDE") {
        if !override_model.trim().is_empty() {
            return override_model;
        }
    }

    let model = match model {
        Some(model) if !model.trim().is_empty() => model,
        _ => return default.to_string(),
    };

    let unprefixed = model.strip_prefix("kiro:").unwrap_or(model);
    let kiro_model = public_model_id_to_kiro_model(unprefixed);

    if is_known_kiro_model(&kiro_model) || looks_like_kiro_claude_model(&kiro_model) {
        return kiro_model;
    }
    if let Some(same_generation) = same_generation_alias(unprefixed) {
        return same_generation;
    }
    unprefixed.to_string()
}

/// 把带日期后缀的 Anthropic 公共 ID 折到**同一代**的 Kiro 型号名上。
///
/// 存在的理由只有一条：Claude Code 没显式配模型时会发 `claude-sonnet-4-5-20250929`
/// 这类带日期的 ID，Kiro 不认识；折成 `claude-sonnet-4.5` 才能用上。
/// 跨代一律不折（`claude-3-5-sonnet-*` 折不出 Kiro 型号，原样返回、由调用方报 400）。
fn same_generation_alias(model: &str) -> Option<String> {
    let mut parts: Vec<&str> = model.split('-').collect();
    // 尾部是日期段（6~8 位纯数字）时先剥掉，剩下的才是「代数 + 版本」
    while parts.len() > 3 && is_date_segment(parts[parts.len() - 1]) {
        parts.pop();
    }
    if parts.len() < 3 || parts[0] != "claude" {
        return None;
    }
    if !matches!(parts[1], "opus" | "sonnet" | "haiku") {
        return None;
    }
    let is_short_number = |text: &str| {
        !text.is_empty()
            && text.len() <= 2
            && text.chars().all(|c| c.is_ascii_digit())
    };
    match parts.len() {
        3 if is_short_number(parts[2]) => Some(format!("claude-{}-{}", parts[1], parts[2])),
        4 if is_short_number(parts[2]) && is_short_number(parts[3]) => {
            Some(format!("claude-{}-{}.{}", parts[1], parts[2], parts[3]))
        }
        _ => None,
    }
}

fn is_date_segment(text: &str) -> bool {
    text.len() >= 6 && text.chars().all(|c| c.is_ascii_digit())
}

/// 粗略 token 估算：字符数 / 4（与 kiro2cli 一致）。
pub fn estimate_tokens(text: &str) -> i64 {
    let chars = text.chars().count() as i64;
    if chars == 0 {
        return 0;
    }
    ((chars + 3) / 4).max(1)
}

// ============ 模型能力抽象（schema 驱动） ============
//
// 目标：把「按模型名硬编码 is_claude / is_gpt 分支」替换成「按模型 schema /
// 能力配置决定哪些字段能发、怎么写」。当拿到 Kiro 的 additionalModelRequestFieldsSchema
// 时以其为唯一事实源；拿不到（或单元测试）时回退到按模型族的内置默认，保证既有行为不变。

/// 模型族。仅用于 schema 缺失时的回退默认与可读性；运行时以 schema 为准。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelFamily {
    Claude,
    Gpt,
    Deepseek,
    Other,
}

impl ModelFamily {
    pub fn classify(model: &str) -> ModelFamily {
        let model = model.trim();
        if model.starts_with("claude-") {
            ModelFamily::Claude
        } else if model.starts_with("gpt-") {
            ModelFamily::Gpt
        } else if model.starts_with("deepseek-") {
            ModelFamily::Deepseek
        } else {
            ModelFamily::Other
        }
    }
}

/// effort 该落到哪个字段：Claude 走 output_config.effort，GPT 走 reasoning.effort。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffortPlacement {
    OutputConfigEffort,
    ReasoningEffort,
    Unsupported,
}

/// 一个模型在 additionalModelRequestFields 方面的「已解析能力」。
/// schema 可用时由 schema 派生（唯一事实源）；否则回退到模型族默认。
#[derive(Debug, Clone)]
pub struct ModelProfile {
    pub family: ModelFamily,
    /// 是否接受顶层 max_tokens
    pub allow_max_tokens: bool,
    /// 是否接受顶层 thinking（adaptive/disabled）
    pub supports_thinking: bool,
    /// 是否接受顶层 output_config
    pub supports_output_config: bool,
    /// 是否接受顶层 reasoning
    pub supports_reasoning: bool,
    /// effort 的落位字段
    pub effort_placement: EffortPlacement,
    /// 接受 effort 的枚举值；空集表示该模型不接受 effort（任何取值都不许发）。
    /// 有 schema 时取 schema 的 `enum`，否则回退 [`effort_levels_for_model`]。
    pub effort_levels: Vec<String>,
    /// schema 顶层允许的字段白名单；None 表示 schema 未知，不做白名单过滤。
    pub allowed_fields: Option<Vec<String>>,
}

fn schema_has_property(schema: &Value, key: &str) -> bool {
    schema
        .get("properties")
        .and_then(|p| p.get(key))
        .is_some()
}

/// 从 schema 推断 effort 应放哪个字段（优先 output_config，其次 reasoning）。
fn schema_effort_placement(schema: &Value) -> EffortPlacement {
    let has_effort = |top: &str| {
        schema
            .get("properties")
            .and_then(|p| p.get(top))
            .and_then(|v| v.get("properties"))
            .and_then(|p| p.get("effort"))
            .is_some()
    };
    if has_effort("output_config") {
        EffortPlacement::OutputConfigEffort
    } else if has_effort("reasoning") {
        EffortPlacement::ReasoningEffort
    } else {
        EffortPlacement::Unsupported
    }
}

fn schema_allowed_fields(schema: &Value) -> Option<Vec<String>> {
    schema
        .get("properties")
        .and_then(|p| p.as_object())
        .map(|map| map.keys().cloned().collect())
}

fn default_profile(family: ModelFamily, kiro_model: &str) -> ModelProfile {
    let effort_levels: Vec<String> = effort_levels_for_model(kiro_model)
        .iter()
        .map(|level| level.to_string())
        .collect();
    match family {
        ModelFamily::Claude => ModelProfile {
            family,
            allow_max_tokens: true,
            supports_thinking: true,
            supports_output_config: true,
            supports_reasoning: false,
            effort_placement: EffortPlacement::OutputConfigEffort,
            effort_levels,
            allowed_fields: None,
        },
        ModelFamily::Gpt => ModelProfile {
            family,
            allow_max_tokens: false,
            supports_thinking: false,
            supports_output_config: false,
            supports_reasoning: true,
            effort_placement: EffortPlacement::ReasoningEffort,
            effort_levels,
            allowed_fields: None,
        },
        ModelFamily::Deepseek | ModelFamily::Other => ModelProfile {
            family,
            allow_max_tokens: false,
            supports_thinking: true,
            supports_output_config: true,
            supports_reasoning: false,
            effort_placement: EffortPlacement::OutputConfigEffort,
            effort_levels,
            allowed_fields: None,
        },
    }
}

/// 解析模型能力：优先以 Kiro 返回的 additionalModelRequestFieldsSchema 为唯一事实源，
/// 缺省时回退到按模型族的内置默认（保证既有行为 + 单元测试不变）。
pub fn resolve_model_profile(kiro_model: &str, schema: Option<&Value>) -> ModelProfile {
    let family = ModelFamily::classify(kiro_model);
    match schema {
        Some(schema) if schema.is_object() => {
            // effort 枚举以 schema 为准；schema 只声明了属性却没给 enum 时退回型号默认，
            // 避免「schema 没枚举 ⇒ 一律不许发」把正常请求也削成没有 effort。
            let schema_levels = get_supported_effort_levels(schema);
            let effort_levels = if schema_levels.is_empty() {
                effort_levels_for_model(kiro_model)
                    .iter()
                    .map(|level| level.to_string())
                    .collect()
            } else {
                schema_levels
            };
            ModelProfile {
                family,
                allow_max_tokens: schema_has_property(schema, "max_tokens"),
                supports_thinking: schema_has_property(schema, "thinking"),
                supports_output_config: schema_has_property(schema, "output_config"),
                supports_reasoning: schema_has_property(schema, "reasoning"),
                effort_placement: schema_effort_placement(schema),
                effort_levels,
                allowed_fields: schema_allowed_fields(schema),
            }
        }
        _ => default_profile(family, kiro_model),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 离线兜底：家族决定是否声明图片输入，未知型号保守按纯文本。
    #[test]
    fn default_input_modalities_follows_model_family() {
        let with_image = vec!["text".to_string(), "image".to_string()];
        let text_only = vec!["text".to_string()];
        for slug in ["gpt-5.6-terra", "gpt-5.6-luna", "claude-opus-4.8", "grok-4.7"] {
            assert_eq!(default_input_modalities(slug), with_image, "{slug}");
        }
        for slug in ["deepseek-3.2", "glm-5", "minimax-m2.5", "qwen3-coder-next", "unknown"] {
            assert_eq!(default_input_modalities(slug), text_only, "{slug}");
        }
    }

    #[test]
    fn defaults_preserve_legacy_family_behavior() {
        let claude = resolve_model_profile("claude-sonnet-5", None);
        assert!(claude.allow_max_tokens);
        assert!(claude.supports_thinking);
        assert!(claude.supports_output_config);
        assert_eq!(claude.effort_placement, EffortPlacement::OutputConfigEffort);
        assert!(claude.allowed_fields.is_none());

        let gpt = resolve_model_profile("gpt-5.6-sol", None);
        assert!(!gpt.allow_max_tokens);
        assert!(!gpt.supports_thinking);
        assert!(!gpt.supports_output_config);
        assert!(gpt.supports_reasoning);
        assert_eq!(gpt.effort_placement, EffortPlacement::ReasoningEffort);

        let deepseek = resolve_model_profile("deepseek-3.2", None);
        assert!(!deepseek.allow_max_tokens);
        assert!(deepseek.supports_thinking);
        assert!(deepseek.supports_output_config);
    }

    #[test]
    fn schema_is_source_of_truth_for_fields() {
        // 一个「只认识 reasoning + effort」的 schema（类似 GPT）
        let schema = json!({
            "properties": {
                "reasoning": { "properties": { "effort": { "enum": ["none","low","medium","high","xhigh","max"] } } }
            }
        });
        let profile = resolve_model_profile("gpt-5.6-terra", Some(&schema));
        assert!(!profile.allow_max_tokens);
        assert!(!profile.supports_thinking);
        assert!(!profile.supports_output_config);
        assert!(profile.supports_reasoning);
        assert_eq!(profile.effort_placement, EffortPlacement::ReasoningEffort);
        let allowed = profile.allowed_fields.unwrap();
        assert_eq!(allowed, vec!["reasoning".to_string()]);
    }

    #[test]
    fn schema_output_config_effort_placement() {
        let schema = json!({
            "properties": {
                "max_tokens": { "type": "integer" },
                "thinking": { "type": "object" },
                "output_config": { "properties": { "effort": { "enum": ["low","medium","high"] } } }
            }
        });
        let profile = resolve_model_profile("claude-opus-5", Some(&schema));
        assert!(profile.allow_max_tokens);
        assert!(profile.supports_thinking);
        assert!(profile.supports_output_config);
        assert_eq!(profile.effort_placement, EffortPlacement::OutputConfigEffort);
        assert_eq!(profile.allowed_fields.unwrap().len(), 3);
    }

    /// 上游新上的 Claude 型号必须原样透传，不能被兜底逻辑降级到旧型号。
    ///
    /// 实测上游已提供 `claude-opus-5.5` / `claude-sonnet-5.5`，而白名单只列到 5。
    /// 「静默换成另一个模型」是最难被发现的一类错误：请求成功、答案正常，
    /// 只是用的不是用户选的模型。
    #[test]
    fn newer_claude_versions_pass_through_instead_of_being_downgraded() {
        assert_eq!(normalize_kiro_model(Some("claude-opus-5.5"), "x"), "claude-opus-5.5");
        assert_eq!(normalize_kiro_model(Some("claude-sonnet-5.5"), "x"), "claude-sonnet-5.5");
        // 公共 ID 形态（`...-4-8`）先被转成 Kiro 形态再透传
        assert_eq!(normalize_kiro_model(Some("claude-opus-4-8"), "x"), "claude-opus-4.8");
        assert_eq!(normalize_kiro_model(Some("claude-opus-5"), "x"), "claude-opus-5");
        // 公共 ID 不带日期后缀的另一种写法
        assert_eq!(normalize_kiro_model(Some("claude-haiku-4.5"), "x"), "claude-haiku-4.5");
    }

    /// 带日期后缀的 Anthropic 公共 ID 只折到**同一代**，绝不跨代。
    ///
    /// Claude Code 不显式配模型时会发 `claude-sonnet-4-5-20250929` 这类 ID，
    /// 折成 `claude-sonnet-4.5` 才能用上；而 `claude-3-5-sonnet-*` 折不出 Kiro 型号，
    /// 必须原样返回让调用方明确报 400 —— 换成 `claude-sonnet-5` 属于静默降级
    /// （花的是另一个模型的额度），2026-11-20 已移除。
    #[test]
    fn dated_anthropic_ids_fold_to_the_same_generation_only() {
        assert_eq!(
            normalize_kiro_model(Some("claude-sonnet-4-5-20250929"), "x"),
            "claude-sonnet-4.5"
        );
        assert_eq!(
            normalize_kiro_model(Some("claude-sonnet-5-20260101"), "x"),
            "claude-sonnet-5"
        );
        assert_eq!(
            normalize_kiro_model(Some("claude-opus-4-1-20250805"), "x"),
            "claude-opus-4.1"
        );
        assert_eq!(
            normalize_kiro_model(Some("claude-sonnet-4-20250514"), "x"),
            "claude-sonnet-4"
        );
        // 更早的写法折不出 Kiro 型号：原样透传，由清单校验拦成 400
        assert_eq!(
            normalize_kiro_model(Some("claude-3-5-sonnet-20241022"), "x"),
            "claude-3-5-sonnet-20241022"
        );
    }

    /// effort 枚举要按形状跟着型号走：上游新上的 `5.5` 也必须有档位，否则
    /// 「模型支持哪些 effort」与「哪些 effort 算合法」同时失效。
    #[test]
    fn effort_levels_follow_model_shape_not_a_whitelist() {
        assert_eq!(effort_levels_for_model("claude-opus-5.5"), CLAUDE_EFFORT);
        assert_eq!(effort_levels_for_model("claude-sonnet-5.5"), CLAUDE_EFFORT);
        assert_eq!(effort_levels_for_model("claude-opus-5"), CLAUDE_EFFORT);
        // 4.6 两代少一档 xhigh
        assert_eq!(effort_levels_for_model("claude-opus-4.6"), CLAUDE_46_EFFORT);
        // GPT 家族多一个 none
        assert_eq!(effort_levels_for_model("gpt-5.6-sol"), GPT_EFFORT);
        // 不带 effort 能力的模型必须是空集：空集 = 任何取值都不许发
        assert!(effort_levels_for_model("deepseek-3.2").is_empty());
        // 带日期后缀的公共 ID 不是 Kiro 型号，不能被当成 Claude 档位依据
        assert!(effort_levels_for_model("claude-3-5-sonnet-20241022").is_empty());
    }

    #[test]
    fn normalize_effort_only_returns_values_inside_the_enum() {
        let claude: Vec<String> = CLAUDE_EFFORT.iter().map(|s| s.to_string()).collect();
        let claude46: Vec<String> = CLAUDE_46_EFFORT.iter().map(|s| s.to_string()).collect();
        let gpt: Vec<String> = GPT_EFFORT.iter().map(|s| s.to_string()).collect();

        assert_eq!(normalize_effort(Some("high"), &claude).as_deref(), Some("high"));
        // none 不在 Claude 的枚举里 → 不发
        assert_eq!(normalize_effort(Some("none"), &claude), None);
        // 不认识的写法一律不发，不猜
        assert_eq!(normalize_effort(Some("minimal"), &claude), None);
        // xhigh ↔ max 互转，按模型实际档位收敛
        assert_eq!(normalize_effort(Some("xhigh"), &claude46).as_deref(), Some("max"));
        assert_eq!(normalize_effort(Some("max"), &claude46).as_deref(), Some("max"));
        // 空枚举 = 模型不接受 effort
        assert_eq!(normalize_effort(Some("high"), &[]), None);
        // GPT 认 none
        assert_eq!(normalize_effort(Some("none"), &gpt).as_deref(), Some("none"));
    }

    /// 最高档按语义取，不信 enum 的书写顺序；空枚举没有「最高档」。
    #[test]
    fn highest_effort_uses_semantic_rank() {
        let levels = |items: &[&str]| -> Vec<String> { items.iter().map(|s| s.to_string()).collect() };

        // 乱序的 enum 也要挑出 max
        assert_eq!(highest_effort(&levels(&["max", "low", "medium"])).as_deref(), Some("max"));
        assert_eq!(highest_effort(&levels(&["low", "high", "medium"])).as_deref(), Some("high"));
        assert_eq!(highest_effort(&levels(&["medium"])).as_deref(), Some("medium"));
        // `none` 是最低档：有别的档时轮不到它
        assert_eq!(highest_effort(&levels(&["none", "low"])).as_deref(), Some("low"));
        assert_eq!(highest_effort(&levels(&["none"])).as_deref(), Some("none"));
        // 模型不收 effort：没有可退的档
        assert_eq!(highest_effort(&levels(&[])), None);
    }

    #[test]
    fn profile_effort_levels_come_from_schema_when_present() {
        let schema = json!({
            "properties": {
                "output_config": { "properties": { "effort": { "enum": ["low", "high"] } } }
            }
        });
        let profile = resolve_model_profile("claude-opus-5.5", Some(&schema));
        assert_eq!(profile.effort_levels, vec!["low".to_string(), "high".to_string()]);

        // schema 声明了属性却没有 enum → 退回型号默认，而不是「一律不许发」
        let schema_no_enum = json!({
            "properties": {
                "output_config": { "properties": { "effort": { "type": "string" } } }
            }
        });
        let profile = resolve_model_profile("claude-opus-5.5", Some(&schema_no_enum));
        assert_eq!(profile.effort_levels.len(), CLAUDE_EFFORT.len());
    }

    /// 「提了 effort」与「提得对不对」是两件事：前者决定要不要补默认档，
    /// 后者由 [`normalize_effort`] 裁决。
    #[test]
    fn effort_is_specified_sees_every_inbound_spelling() {
        let cases = [
            json!({ "effort": "none" }),
            json!({ "effort_level": "high" }),
            json!({ "reasoning_effort": "low" }),
            json!({ "reasoning": { "effort": "minimal" } }),
            json!({ "output_config": { "effort": "HIGH" } }),
            json!({ "additionalModelRequestFields": { "effort": "none" } }),
            json!({ "additionalModelRequestFields": { "output_config": { "effort": "minimal" } } }),
            json!({ "additionalModelRequestFields": { "reasoning": { "effort": "none" } } }),
            json!({ "additional_model_request_fields": { "effort": "none" } }),
            json!({ "additional_model_request_fields": { "output_config": { "effort": "none" } } }),
            json!({ "additional_model_request_fields": { "reasoning": { "effort": "none" } } }),
        ];
        for body in &cases {
            assert!(effort_is_specified(body), "该写法必须算「提过 effort」: {body}");
        }

        // 完全没提（或只有 null）才算没提 —— 这时才补默认档。
        assert!(!effort_is_specified(&json!({ "messages": [] })));
        assert!(!effort_is_specified(&json!({ "output_config": {} })));
        assert!(!effort_is_specified(&json!({ "output_config": { "effort": null } })));
    }

    /// 认不出的模型名原样透传，不再「猜一个近似的」或退回默认模型。
    ///
    /// 猜是静默降级的入口：客户端要 `gpt-4o`、拿回来的却是 Claude，额度与风格全不对。
    /// 现在这类名字会被 `resolve_kiro_model` 拿账户清单判定，不在清单里就明确报 400。
    #[test]
    fn unknown_models_pass_through_instead_of_guessing() {
        assert_eq!(
            normalize_kiro_model(Some("some-random-model"), "fallback"),
            "some-random-model"
        );
        assert_eq!(normalize_kiro_model(Some("gpt-4o"), "fallback"), "gpt-4o");
        assert_eq!(
            normalize_kiro_model(Some("claude-opus-3"), "fallback"),
            "claude-opus-3"
        );
        // 没给模型（或给了空串）才用默认：这不是替换，是客户端没提要求
        assert_eq!(normalize_kiro_model(None, "fallback"), "fallback");
        assert_eq!(normalize_kiro_model(Some("   "), "fallback"), "fallback");
    }

    #[test]
    fn kiro_prefix_is_stripped() {
        assert_eq!(normalize_kiro_model(Some("kiro:claude-opus-5"), "x"), "claude-opus-5");
    }
}
