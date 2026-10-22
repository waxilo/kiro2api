//! Codex CLI 接入管理：把本应用的接入配置写进 `~/.codex/config.toml` 与
//! `~/.codex/auth.json`，并让用户在「三方接入」分区里管理它。
//!
//! 外部客户端配置的修改纪律与 `workbuddy.rs` 一致（那里是第一条例外，这里是第二条）：
//! 由用户在界面上显式发起；只动指向本应用的条目；其余内容（用户注释、别的
//! provider、ChatGPT 登录凭据）一概原样保留。
//!
//! Codex 侧的事实（2026-02 起，openai/codex#7782）：
//! * 自定义 provider 写在 `[model_providers.<id>]` 表里，顶层 `model_provider`
//!   选择活跃 provider；
//! * `wire_api = "chat"` 已被移除，只认 `wire_api = "responses"` —— 本代理在
//!   `/openai/v1/responses` 提供该协议；
//! * 第三方密钥写在 provider 表的 `experimental_bearer_token` 里，Codex 直接拿它
//!   当 `Authorization: Bearer` 发出去（cc-switch 的第三方接入模板同一条路）。
//!
//! 「接入」= 写 provider 表（base_url 指向本代理 /openai/v1 + 密钥）+ 把顶层
//! `model_provider` 指到它。Codex 一次只有一个活跃 provider，因此接入必然
//! 「占用」顶层选择 —— 移除时把指向我们的顶层键删掉，Codex 回落到内置 openai。
//!
//! ## 不碰 auth.json
//!
//! `auth.json` 是**官方登录态**的地盘（`tokens` / `auth_mode`），Codex App 里
//! 显示的「账号」就来自它。早前版本把密钥写成 `auth.json` 的 `OPENAI_API_KEY`
//! 并配 `requires_openai_auth = true`：身份当场被换成 API key 模式，界面上账号
//! 变成我们的 provider 名；还原时只要密钥对不上（重置过密钥、换机器）就删不掉，
//! 账号一直卡在第三方。
//!
//! 现在密钥只进 `config.toml`：无论接入还是取消，Codex 的官方登录态都原样不动。
//! `auth.json` 只在**清理旧版残留**时被写（见 `clean_auth_residue`）—— 见不得
//! 我们自己写进去的那个 `k2a-…` 留在那儿继续冒充用户的登录身份。
//!
//! ## 取消接管 = 还原，不是删除
//!
//! 「把我们写进去的键删掉」和「还原成接管前的样子」不是一回事：用户原本可能
//! 指着自己的中转（`model_provider = "relay"` + 自己的 `model`），只删顶层键
//! 会让 Codex 回落到内置 openai，看起来就是「取消了但配置没还原」。
//!
//! 所以接管前把 `config.toml` **整份**快照到应用数据目录（`Kiro2API/codex-backup/`），
//! 取消接管时整份放回去。快照只在还没有快照时做一次 —— 重复点「应用」时配置里
//! 已经是我们写的内容，再快照一次就把还原点覆盖成「接管后」了。
//!
//! 缺快照（本次改动之前就接管过的）时退回旧行为：逐键删除，至少不留指向本应用的悬空配置。
//!
//! ## 模型目录
//!
//! Codex 支持顶层 `model_catalog_json` 指向一个模型目录文件（相对路径以
//! ~/.codex 为基准），内容是 `{ "models": [ModelInfo...] }` —— 进了目录的
//! 模型出现在 Codex 自己的 /model 选择器里，切换由用户在 Codex 侧完成
//! （选择会被 Codex 写回顶层 `model`）。我们把勾选的模型写成
//! kiro2api-model-catalog.json 并接管指针；slug 一律用 Kiro 公开模型 id，
//! 请求才能在代理侧正确解析（未知模型名会被代理归一化成默认模型）。
//! 条目字段按 openai/codex 新旧两个 schema 世代的超集生成：serde 不拒未知
//! 字段，多写不炸 —— 老版本忽略新键，新版本忽略旧键。
//!
//! 指针纪律：`model_catalog_json` 已指向别家的文件时一概不碰（接入只写
//! provider，模型交由用户自理）；指针指向我们时，取消全部勾选 = 撤指针 +
//! 撤顶层 model，目录文件随之删除。写盘顺序保证指针永不悬空 —— 建目录先写
//! 文件再写配置，撤目录先写配置再删文件。

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{json, Value};
use toml_edit::{table, value, DocumentMut, Item, Table};
use tauri::State;

use crate::codex_restart::codex_app_running;
use crate::kiro::models::ModelFamily;
use crate::kiro::{build_access, current_api_key, KiroProxyState};
use crate::workbuddy::origin_matches;

/// 本应用在 Codex 里的 provider id。写配置与扫描识别都用它。
const PROVIDER_ID: &str = "kiro2api";

/// provider 表里放密钥的键。Codex 会把它当 `Authorization: Bearer` 发出去，
/// 与 `requires_openai_auth`（读 auth.json 的 `OPENAI_API_KEY`）互斥 —— 我们只用
/// 这一个，auth.json 留给官方登录。
const TOKEN_FIELD: &str = "experimental_bearer_token";

/// 本应用密钥的形态（`prefs::generate_api_key`）。用来认出「旧版写进 auth.json
/// 的残留」—— 重置过密钥后，旧值已和当前密钥对不上，只能按形态认。
const KEY_PREFIX: &str = "k2a-";

/// 模型目录文件名（相对 ~/.codex）。指针识别按 basename 比对，用户就算把
/// 指针写成绝对路径也认得出是我们的。
const CATALOG_FILENAME: &str = "kiro2api-model-catalog.json";

/// 一条指向本应用的 Codex provider。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexEntry {
    pub id: String,
    pub base_url: String,
    /// 顶层 `model_provider` 是否指向它（即 Codex 当前正在用它）。
    pub active: bool,
    /// provider 配置里的密钥（`experimental_bearer_token`，旧版在 auth.json）与
    /// 当前代理密钥是否一致；false 时界面提示重置密钥已让它过期。
    pub key_matches: bool,
}

/// 模型目录指针的状态。Foreign = 指向别家的目录文件 —— 一概不碰。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogState {
    #[default]
    None,
    Ours,
    Foreign,
}

/// 扫描结果。读盘失败不作为命令错误抛出 —— 界面就地说明原因（与 WorkBuddy 同一取舍）。
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CodexScan {
    pub installed: bool,
    pub entries: Vec<CodexEntry>,
    pub error: Option<String>,
    /// 模型目录指针状态；models 仅在 Ours 时有值，active_model 仅在 Ours 时上报
    /// （那是 Codex /model 里用户当前的选择）。
    pub catalog_state: CatalogState,
    pub catalog_models: Vec<String>,
    pub active_model: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexImportSummary {
    pub config_written: bool,
    /// 密钥是否（重新）写进 provider 配置。密钥只进 `config.toml`，不再写 auth.json。
    pub key_written: bool,
    /// 模型目录文件被写入或删除
    pub catalog_written: bool,
    /// Codex 顶层默认模型被写入或撤除
    pub model_written: bool,
    /// 写入那一刻 Codex 桌面应用在不在跑：跑着才需要重启确认
    pub codex_running: bool,
}

/// `k2a_codex_remove` 的结果。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexRemoveOutcome {
    /// 是否确有改动（指向其它服务的配置一概不动）
    pub removed: bool,
    /// 是否走了「整份还原接管前配置」这条路（没有快照时是逐键删除，为 false）
    pub restored: bool,
    /// 移除那一刻 Codex 桌面应用在不在跑：跑着才需要重启确认
    pub codex_running: bool,
}

/// Codex 的配置目录。目录不存在视为「Codex 未安装」—— 替一个不存在的应用创建
/// 配置目录没有意义，用户装好后再接入即可。
fn codex_dir() -> Result<PathBuf, String> {
    let dir = dirs::home_dir()
        .ok_or_else(|| "无法定位用户主目录".to_string())?
        .join(".codex");
    if !dir.is_dir() {
        return Err("未找到 Codex（~/.codex 不存在）。请确认 Codex CLI 已安装并运行过一次。".to_string());
    }
    Ok(dir)
}

fn config_path(dir: &Path) -> PathBuf {
    dir.join("config.toml")
}

fn auth_path(dir: &Path) -> PathBuf {
    dir.join("auth.json")
}

fn catalog_path(dir: &Path) -> PathBuf {
    dir.join(CATALOG_FILENAME)
}

/// 接管前快照的存放目录：应用数据目录下的 `codex-backup/`。
///
/// 放应用数据目录而不是 `~/.codex` 里：那是 Codex 自己的地盘，我们不该往里
/// 留不属于它的文件；用户真要手工还原，路径也写在界面的错误提示里。
fn backup_root() -> PathBuf {
    crate::paths::get_data_path().join("codex-backup")
}

fn backup_config_path(root: &Path) -> PathBuf {
    root.join("config.toml")
}

fn backup_meta_path(root: &Path) -> PathBuf {
    root.join("meta.json")
}

/// 接管前的现场。
#[derive(Debug, Clone, serde::Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct TakeoverMeta {
    /// 接管那一刻 `config.toml` 是否存在。不存在说明这文件是我们从无到有写的，
    /// 取消接管时该把它删掉，而不是还原成一份空文件。
    had_config: bool,
    /// 接管那一刻 auth.json 里用户自己的 `OPENAI_API_KEY`（没有则为 None）。
    /// 我们不再往 auth.json 写密钥，这个字段只为还原旧版接管留下的现场。
    auth_key: Option<String>,
}

/// 接管前快照。已存在快照时返回 false 并原样保留 —— 见模块注释里的「只快照一次」。
///
/// 快照写不下去就直接失败：没有还原点的接管是个陷阱，用户点「清空」时会发现
/// 原来的配置回不来。
fn snapshot_before_takeover(root: &Path, dir: &Path) -> Result<bool, String> {
    if backup_meta_path(root).is_file() {
        return Ok(false);
    }
    let config_file = config_path(dir);
    let (had_config, text) = match fs::read_to_string(&config_file) {
        Ok(text) => (true, text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => (false, String::new()),
        Err(err) => return Err(format!("读取 config.toml 失败，无法备份接管前的配置: {err}")),
    };
    fs::create_dir_all(root).map_err(|e| format!("创建备份目录失败: {e}"))?;
    if had_config {
        write_atomic(&backup_config_path(root), &text)?;
    }
    let auth_file = auth_path(dir);
    let auth_key = read_auth(&auth_file)
        .ok()
        .and_then(|auth| {
            auth.get("OPENAI_API_KEY")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .filter(|key| !key.trim().is_empty())
        // 快照里这个键本身可能就是旧版我们自己写进去的（老用户点「应用」时
        // auth.json 里已全是我们的痕迹）：那是残留，不能当成「用户的原始密钥」
        // 记下来 —— 否则还原会把我们的密钥放回去，账号永远卡在第三方。
        .filter(|key| !key.trim().starts_with(KEY_PREFIX));
    let meta = TakeoverMeta {
        had_config,
        auth_key,
    };
    let body = serde_json::to_string_pretty(&meta).map_err(|e| format!("序列化备份摘要失败: {e}"))?;
    write_atomic(&backup_meta_path(root), &body)?;
    Ok(true)
}

/// 取消接管：把接管前的 `config.toml` 整份放回去，auth.json 只归位密钥那一个键。
///
/// 返回是否真的还原了（没有快照时为 false，由调用方退回逐键删除）。
fn restore_from_backup(root: &Path, dir: &Path, api_key: &str) -> Result<bool, String> {
    let meta_file = backup_meta_path(root);
    let Ok(raw) = fs::read_to_string(&meta_file) else {
        return Ok(false);
    };
    let meta: TakeoverMeta = serde_json::from_str(&raw)
        .map_err(|e| format!("接管前的备份已损坏（{e}），无法还原；请手动检查 {meta_file:?}"))?;

    // 快照残了（文件被删或读不动）：退回逐键删除，别把用户堵在「取消不了」上。
    // 顺带丢掉这份残档 —— 留着它会让下一次接管跳过快照，永远没有还原点。
    let backup_text = if meta.had_config {
        match fs::read_to_string(backup_config_path(root)) {
            Ok(text) => Some(text),
            Err(err) => {
                crate::diag::log(&format!(
                    "[codex] 接管前的 config.toml 快照不可读（{err}），退回逐键删除"
                ));
                let _ = fs::remove_dir_all(root);
                return Ok(false);
            }
        }
    } else {
        None
    };

    // 先读盘再动手：auth.json 坏了要在写 config.toml 之前就失败，
    // 免得「配置已还原但命令报错」这种半截状态
    let auth_file = auth_path(dir);
    let mut auth = read_auth(&auth_file)?;

    let config_file = config_path(dir);
    if let Some(text) = backup_text {
        write_atomic(&config_file, &text)?;
    } else if config_file.exists() {
        // 接管前根本没有这份文件：还回「不存在」，而不是留一份空文件
        fs::remove_file(&config_file)
            .map_err(|e| format!("删除接管时新建的 config.toml 失败: {e}"))?;
    }

    let auth_changed = match meta.auth_key.as_deref().map(str::trim) {
        // 接管前有自己的密钥：放回去。旧快照可能记着的是我们自己写的密钥
        // （见 `snapshot_before_takeover`），按形态认出来当残留处理。
        Some(original) if !is_our_key(original, api_key) => restore_user_key(&mut auth, original),
        // 接管前没有：把我们的痕迹清干净（新版不写 auth.json，旧版写过）。
        // 不按「等于当前密钥」判 —— 密钥重置过、换了机器就删不掉，正是旧版
        // 还原后 Codex 仍显示第三方账号的原因。
        _ => clean_auth_residue(&mut auth, api_key),
    };
    if auth_changed {
        // 清完只剩空对象等于「没有登录态」：连文件一起还回「不存在」，
        // 留一份 `{}` 只会让 Codex 与用户都以为这里还有点东西
        if auth.as_object().is_some_and(|obj| obj.is_empty()) {
            let _ = fs::remove_file(&auth_file);
        } else {
            write_auth(&auth_file, &auth)?;
        }
    }

    // 目录文件是本应用建的，指针已随快照还原 —— 留着就是孤儿文件
    let _ = fs::remove_file(catalog_path(dir));
    // 快照用完即弃。删不掉也不影响正确性：它此刻与磁盘上的配置一模一样，
    // 下次接管即便沿用它，还原出来的还是同一份 —— 而报错会把「已经还原好了」说成失败
    let _ = fs::remove_dir_all(root);
    Ok(true)
}

/// 先写临时文件再改名：Codex 读到半个文件比读到旧值糟糕得多。
/// 改名失败（目标被占用）时退回直接覆盖 —— 内容写入是幂等的。
fn write_atomic(path: &Path, body: &str) -> Result<(), String> {
    let tmp = path.with_extension("k2a-tmp");
    fs::write(&tmp, body.as_bytes()).map_err(|e| format!("写入临时文件失败: {e}"))?;
    if fs::rename(&tmp, path).is_ok() {
        return Ok(());
    }
    let result = fs::write(path, body.as_bytes())
        .map_err(|e| format!("写入 {} 失败: {}", path.file_name().and_then(|n| n.to_str()).unwrap_or("配置"), e));
    let _ = fs::remove_file(&tmp);
    result
}

/// 读 config.toml。文件不存在（装了但从未配置）返回空文档。
fn read_config(path: &Path) -> Result<DocumentMut, String> {
    match fs::read_to_string(path) {
        Ok(raw) => raw
            .parse::<DocumentMut>()
            .map_err(|e| format!("config.toml 不是合法 TOML（{e}），请先在编辑器里检查该文件")),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Ok("".parse::<DocumentMut>().expect("空串恒为合法 TOML"))
        }
        Err(err) => Err(format!("读取 config.toml 失败: {err}")),
    }
}

/// 读 auth.json。文件不存在返回空对象；格式坏了返回错误，由调用方决定怎么交代。
fn read_auth(path: &Path) -> Result<Value, String> {
    match fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str::<Value>(&raw)
            .map_err(|e| format!("auth.json 不是合法 JSON（{e}），请先检查该文件")),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Value::Object(Default::default())),
        Err(err) => Err(format!("读取 auth.json 失败: {err}")),
    }
}

/// 取路径末端的文件名（/ 与 \ 都认）—— 用户把指针写成绝对路径也认得出我们。
fn basename_of(path: &str) -> &str {
    let trimmed = path.trim();
    let start = trimmed.rfind(['/', '\\']).map(|i| i + 1).unwrap_or(0);
    &trimmed[start..]
}

/// 顶层 model_catalog_json 指针的状态：缺失 = None；basename 是我们的文件名 =
/// Ours；其余（包括指向别家或格式坏了的非字符串值）= Foreign —— 一概不碰。
fn pointer_state_from_doc(doc: &DocumentMut) -> CatalogState {
    match doc.get("model_catalog_json") {
        None => CatalogState::None,
        Some(item) => match item.as_str() {
            Some(v) if basename_of(v) == CATALOG_FILENAME => CatalogState::Ours,
            _ => CatalogState::Foreign,
        },
    }
}

/// 旧世代必需、新世代已移除的 `base_instructions`：必须是**字符串**（写 null
/// 会被旧版 Codex 拒载，cc-switch 同款教训），取 codex 官方模板的中性身份句。
/// 新版本不认识这个键，忽略不碍事。
const NEUTRAL_BASE_INSTRUCTIONS: &str =
    "You are Codex, a coding agent. You and the user share the same workspace and collaborate to achieve the user's goals.";

/// 一条目录条目。字段是 openai/codex 新旧两个 schema 世代的超集：两个世代
/// 各自必需的键都写上，新世代必需的 Option 槽位显式给 null —— serde 不拒未知
/// 字段，多写不炸。推理档位只挂 low/medium/high（默认 high）：全模型通吃，
/// Codex 侧用户选低档时代理侧的归一化也能接住。
fn catalog_entry(slug: &str, priority: i32) -> Value {
    json!({
        "slug": slug,
        "display_name": slug,
        "description": format!("kiro2api 本地代理 · {slug}"),
        "base_instructions": NEUTRAL_BASE_INSTRUCTIONS,
        "supports_reasoning_summaries": false,
        "supports_parallel_tool_calls": false,
        // 两个世代都必需的键
        "supported_reasoning_levels": [
            { "effort": "low", "description": "更快" },
            { "effort": "medium", "description": "均衡" },
            { "effort": "high", "description": "更深入" }
        ],
        "default_reasoning_level": "high",
        "shell_type": "shell_command",
        "visibility": "list",
        "supported_in_api": true,
        "priority": priority,
        "availability_nux": null,
        "upgrade": null,
        "support_verbosity": false,
        "default_verbosity": null,
        "apply_patch_tool_type": null,
        "truncation_policy": { "mode": "tokens", "limit": 200000 },
        "experimental_supported_tools": [],
        "context_window": 200000,
        "max_context_window": 200000,
        "effective_context_window_percent": 95,
        "supports_image_detail_original": false,
        "supports_search_tool": false,
        "input_modalities": ["text"],
        "additional_speed_tiers": [],
        "service_tiers": [],
    })
}

/// 目录文件内容：Codex 只认 `{ "models": [...] }`。priority 随位置递增，
/// 让选择器按勾选顺序排。
fn catalog_document(models: &[String]) -> Value {
    json!({
        "models": models
            .iter()
            .enumerate()
            .map(|(i, slug)| catalog_entry(slug, 1000 + i as i32))
            .collect::<Vec<_>>(),
    })
}

/// 解析目录文件文本，取出 slug 清单。缺 slug、不是 JSON 都按错误上抛；
/// models 为空数组在 Codex 侧等同坏文件（启动即报错），由调用方交代。
fn parse_catalog_text(text: &str) -> Result<Vec<String>, String> {
    let value: Value =
        serde_json::from_str(text).map_err(|e| format!("模型目录文件不是合法 JSON（{e}）"))?;
    let models = value
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| "模型目录文件缺少 models 数组".to_string())?;
    models
        .iter()
        .map(|m| {
            m.get("slug")
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| "模型目录条目缺少 slug".to_string())
        })
        .collect()
}

/// entries 的 doc 级核心：scan_files 与测试共用同一次解析。
///
/// 密钥优先取 provider 表里的 `experimental_bearer_token`；表里没有时用
/// `legacy_auth_key`（auth.json 的 `OPENAI_API_KEY`）兜底 —— 那是旧版写法，
/// 老用户点「应用」后自然迁到新位置。
fn entries_from_doc(
    doc: &DocumentMut,
    port: u16,
    legacy_auth_key: Option<&str>,
    current_key: Option<&str>,
) -> Vec<CodexEntry> {
    let active_provider = doc
        .get("model_provider")
        .and_then(|item| item.as_str())
        .unwrap_or("");
    let Some(providers) = doc.get("model_providers").and_then(|item| item.as_table_like()) else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    for (id, item) in providers.iter() {
        let Some(provider) = item.as_table_like() else {
            continue;
        };
        let Some(base_url) = provider.get("base_url").and_then(|v| v.as_str()) else {
            continue;
        };
        if !origin_matches(base_url, port) {
            continue;
        }
        let stored_key = provider
            .get(TOKEN_FIELD)
            .and_then(|v| v.as_str())
            .or(legacy_auth_key);
        let key_matches = matches!(
            (stored_key, current_key),
            (Some(stored), Some(current))
                if !current.trim().is_empty() && stored.trim() == current.trim()
        );
        entries.push(CodexEntry {
            id: id.to_string(),
            base_url: base_url.to_string(),
            active: id == active_provider,
            key_matches,
        });
    }
    entries
}

/// 目录指针状态与 Codex 顶层当前模型（/model 里用户的选择会写回这里）。
fn catalog_pointer_from_doc(doc: &DocumentMut) -> (CatalogState, Option<String>) {
    (
        pointer_state_from_doc(doc),
        doc.get("model").and_then(|item| item.as_str()).map(str::to_string),
    )
}

/// 原位覆盖顶层键：已有就沿用原来的 decor（键前的注释、行尾空白），没有才
/// 追加。`doc[key] = …` 的整对替换会把挂在键上的用户注释一起抹掉。
fn put_top_value(doc: &mut DocumentMut, key: &str, new: toml_edit::Value) {
    let table = doc.as_table_mut();
    match table.get_mut(key) {
        Some(Item::Value(slot)) => {
            let decor = slot.decor().clone();
            *slot = new;
            *slot.decor_mut() = decor;
        }
        Some(slot) => *slot = Item::Value(new),
        None => {
            table.insert(key, Item::Value(new));
        }
    }
}

/// 接入的核心（纯函数）：在文档上写/更新 provider 表、把顶层 model_provider
/// 指到它，并按 `models` 维护模型目录指针与顶层默认 model。只设置本应用的
/// 键，provider 表里用户自己加的其余键（http_headers 等）原样保留。
///
/// 密钥写进 provider 表的 `experimental_bearer_token`（auth.json 一概不碰）；
/// 旧版写过的 `requires_openai_auth` 顺手清掉 —— 它会让 Codex 转而读 auth.json，
/// 两个凭据来源同时在场只会打架。
///
/// 返回「是否接管了目录」—— Foreign 指针下一律 false（目录与 model 不碰）；
/// models 为空且指针是我们的 = 撤指针撤 model，也算接管。顶层 model 的取舍：
/// 用户已在 Codex /model 里选了我们清单中的模型时保留其选择，否则落到第一个。
fn import_into_document(
    doc: &mut DocumentMut,
    base_url: &str,
    api_key: &str,
    models: &[String],
) -> bool {
    if doc
        .get("model_providers")
        .and_then(Item::as_table_like)
        .is_none()
    {
        doc.insert("model_providers", table());
    }
    let providers = doc
        .get_mut("model_providers")
        .and_then(Item::as_table_like_mut)
        .expect("刚刚确保是表");
    if providers
        .get(PROVIDER_ID)
        .and_then(Item::as_table_like)
        .is_none()
    {
        providers.insert(PROVIDER_ID, Item::Table(Table::new()));
    }
    let provider = providers
        .get_mut(PROVIDER_ID)
        .and_then(Item::as_table_like_mut)
        .expect("刚刚确保是表");
    provider.insert("name", value(PROVIDER_ID));
    provider.insert("base_url", value(base_url));
    provider.insert("wire_api", value("responses"));
    provider.insert(TOKEN_FIELD, value(api_key));
    provider.remove("requires_openai_auth");
    put_top_value(doc, "model_provider", PROVIDER_ID.into());

    // 指针状态此刻还是旧值（下面才动 model_catalog_json）
    let state = pointer_state_from_doc(doc);
    if models.is_empty() {
        if state == CatalogState::Ours {
            doc.as_table_mut().remove("model_catalog_json");
            doc.as_table_mut().remove("model");
            return true;
        }
        return false;
    }
    if state == CatalogState::Foreign {
        return false;
    }
    put_top_value(doc, "model_catalog_json", CATALOG_FILENAME.into());
    let active = doc.get("model").and_then(|item| item.as_str()).unwrap_or("");
    if !models.iter().any(|m| m == active) {
        put_top_value(doc, "model", models[0].as_str().into());
    }
    true
}

/// provider 表里记录的密钥。旧版把密钥写在 auth.json，这里返回 None，由调用方
/// 用 auth.json 的值兜底（见 `scan_files`）。
fn provider_token(doc: &DocumentMut) -> Option<String> {
    doc.get("model_providers")
        .and_then(|item| item.as_table_like())
        .and_then(|providers| providers.get(PROVIDER_ID))
        .and_then(|item| item.as_table_like())
        .and_then(|provider| provider.get(TOKEN_FIELD))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// 这个 `OPENAI_API_KEY` 是不是我们写的：等于当前密钥，或者是本应用的密钥形态
/// （`k2a-` 前缀 —— 重置过密钥之后，旧值就只剩形态可认）。用户自己的 `sk-…` 不动。
fn is_our_key(value: &str, api_key: &str) -> bool {
    let value = value.trim();
    !value.is_empty() && (value == api_key.trim() || value.starts_with(KEY_PREFIX))
}

/// auth 对象里除了 `auth_mode` 之外还有没有真凭据（ChatGPT tokens、PAT、Bedrock…）。
/// 有就说明用户确实登录过官方，`auth_mode` 不能当残留删。
fn has_real_credentials(obj: &serde_json::Map<String, Value>) -> bool {
    [
        "tokens",
        "personal_access_token",
        "agent_identity",
        "bedrock_api_key",
        "bedrock_access_keys",
    ]
    .iter()
    .any(|key| {
        obj.get(*key).is_some_and(|value| match value {
            Value::Null => false,
            Value::String(text) => !text.trim().is_empty(),
            Value::Object(map) => !map.is_empty(),
            _ => true,
        })
    })
}

/// 清理 auth.json 的旧版残留（纯函数）。只做减法，官方登录凭据一个不碰。
///
/// * `OPENAI_API_KEY` 是我们的 → 删（旧版接管时写进去的）；
/// * 删完只剩 `auth_mode = "apikey"`，既没有密钥也没有任何真凭据 → 连 `auth_mode`
///   一起删。`resolved_mode` 是「显式 auth_mode 优先」，留着它 Codex 仍按 API key
///   身份运行（界面账号卡在第三方），这是旧版还原不干净的真正原因。
fn clean_auth_residue(auth: &mut Value, api_key: &str) -> bool {
    let Some(obj) = auth.as_object_mut() else {
        return false;
    };
    let mut changed = false;
    if obj
        .get("OPENAI_API_KEY")
        .and_then(Value::as_str)
        .is_some_and(|value| is_our_key(value, api_key))
    {
        obj.remove("OPENAI_API_KEY");
        changed = true;
    }
    let api_key_mode = obj
        .get("auth_mode")
        .and_then(Value::as_str)
        .is_some_and(|mode| mode.eq_ignore_ascii_case("apikey"));
    let key_left = obj
        .get("OPENAI_API_KEY")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty());
    if api_key_mode && !key_left && !has_real_credentials(obj) {
        obj.remove("auth_mode");
        changed = true;
    }
    changed
}

/// 接管前的用户密钥归位（纯函数）。返回是否变化。
fn restore_user_key(auth: &mut Value, original: &str) -> bool {
    let Some(obj) = auth.as_object_mut() else {
        return false;
    };
    let current = obj
        .get("OPENAI_API_KEY")
        .and_then(Value::as_str)
        .map(str::trim);
    if current == Some(original.trim()) {
        return false;
    }
    obj.insert(
        "OPENAI_API_KEY".to_string(),
        Value::String(original.to_string()),
    );
    true
}

/// 把上面的纯函数落到盘上。`drop_if_empty` 为 true 时，清完只剩空对象的
/// auth.json 直接删掉 —— 空文件与「没有」等价，留着只是噪音。
fn clean_auth_file(auth_file: &Path, api_key: &str, drop_if_empty: bool) -> Result<bool, String> {
    let mut auth = read_auth(auth_file)?;
    if !clean_auth_residue(&mut auth, api_key) {
        return Ok(false);
    }
    if drop_if_empty && auth.as_object().is_some_and(|obj| obj.is_empty()) {
        let _ = fs::remove_file(auth_file);
        return Ok(true);
    }
    write_auth(auth_file, &auth)?;
    Ok(true)
}

/// 写 auth.json。内容来自读盘 + 删键，序列化失败只可能是内部问题。
fn write_auth(path: &Path, auth: &Value) -> Result<(), String> {
    let body = serde_json::to_string(auth).map_err(|e| format!("序列化失败: {e}"))?;
    write_atomic(path, &body)
}

/// 移除的核心（纯函数）：删掉指向本应用的 provider 表与指向它的顶层选择，
/// 并从 auth 对象里删掉与我们当前密钥一致的 OPENAI_API_KEY（用户自己的密钥不动）。
/// 模型目录指针是我们的时，连同指针与顶层 model 一起撤（目录文件由调用方在
/// 配置落盘后删除）；指向别家的一概不动。返回 (配置是否变化, 密钥是否变化,
/// 目录指针是否被撤)。
fn remove_from_parts(
    doc: &mut DocumentMut,
    auth: &mut Value,
    port: u16,
    api_key: &str,
) -> (bool, bool, bool) {
    let mut config_changed = false;

    let ours = doc
        .get("model_providers")
        .and_then(|item| item.as_table_like())
        .and_then(|providers| providers.get(PROVIDER_ID))
        .and_then(|item| item.as_table_like())
        .and_then(|provider| provider.get("base_url"))
        .and_then(|v| v.as_str())
        .is_some_and(|base_url| origin_matches(base_url, port));
    if ours {
        if let Some(providers) = doc["model_providers"].as_table_like_mut() {
            providers.remove(PROVIDER_ID);
            config_changed = true;
        }
    }
    // 顶层指向被我们占用过才归还：指向别的 provider 时不碰
    if doc.get("model_provider").and_then(|item| item.as_str()) == Some(PROVIDER_ID) {
        doc.as_table_mut().remove("model_provider");
        config_changed = true;
    }

    // 目录指针是我们的才撤：顶层 model 是我们写入或用户在我们目录里选的，
    // 一并归还；指向别家的目录与 model 原样保留
    let mut catalog_removed = false;
    if pointer_state_from_doc(doc) == CatalogState::Ours {
        doc.as_table_mut().remove("model_catalog_json");
        doc.as_table_mut().remove("model");
        config_changed = true;
        catalog_removed = true;
    }

    // 旧版把密钥写在 auth.json：按形态认（重置过密钥也认得出），新版没有这个键
    let auth_changed = clean_auth_residue(auth, api_key);
    (config_changed, auth_changed, catalog_removed)
}

/// 前端来的模型清单：去首尾空白、丢空项、去重（保序），只留 GPT 系 ——
/// Codex 桌面端只认 gpt- 开头的模型，目录里混进 claude/deepseek 会让用户在
/// /model 里选到后请求失败。旧目录里的非 GPT 条目借「应用」自然清出。
fn clean_models(models: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    models
        .into_iter()
        .map(|m| m.trim().to_string())
        .filter(|m| {
            !m.is_empty()
                && ModelFamily::classify(m) == ModelFamily::Gpt
                && seen.insert(m.clone())
        })
        .collect()
}

/// 接入 Codex：写 provider 表、激活它、把密钥写进 auth.json，并按勾选的
/// 模型维护目录文件与顶层默认 model。地址与密钥都取自后端当前配置 ——
/// 密钥不经过前端，与「复制」同一条纪律。
#[tauri::command]
pub async fn k2a_codex_import(
    state: State<'_, KiroProxyState>,
    models: Vec<String>,
) -> Result<CodexImportSummary, String> {
    let access = build_access(&state);
    let api_key = current_api_key(&state)
        .ok_or_else(|| "还没有代理密钥，请先启动一次代理生成密钥".to_string())?;
    // 地址由配置端口决定（见 `build_access`），停止状态同样有值 —— 接入与代理
    // 是否在跑无关，写的是 Codex 自己的配置。
    if access.openai_base_url.is_empty() {
        return Err("无法确定代理端口，无法构造接入地址".to_string());
    }
    let base_url = access.openai_base_url;
    let models = clean_models(models);
    tauri::async_runtime::spawn_blocking(move || import_to_files(&base_url, &api_key, &models))
        .await
        .map_err(|e| format!("接入任务失败: {e}"))?
}

fn import_to_files(
    base_url: &str,
    api_key: &str,
    models: &[String],
) -> Result<CodexImportSummary, String> {
    let dir = codex_dir()?;
    import_to_dir(&dir, &backup_root(), base_url, api_key, models)
}

fn import_to_dir(
    dir: &Path,
    backup: &Path,
    base_url: &str,
    api_key: &str,
    models: &[String],
) -> Result<CodexImportSummary, String> {
    let config_file = config_path(dir);
    let mut doc = read_config(&config_file)?;
    // 先拍接管前的快照再动文件：这一步失败就中止接管 —— 没有还原点的接管
    // 会让「清空」变不回原样，宁可现在就报错
    snapshot_before_takeover(backup, dir)?;
    let model_before = doc
        .get("model")
        .and_then(|item| item.as_str())
        .map(str::to_string);
    let token_before = provider_token(&doc);
    let before = doc.to_string();
    let catalog_managed = import_into_document(&mut doc, base_url, api_key, models);
    let config_written = doc.to_string() != before;
    let key_written = provider_token(&doc) != token_before;
    let model_written = doc
        .get("model")
        .and_then(|item| item.as_str())
        .map(str::to_string)
        != model_before;

    let catalog_file = catalog_path(dir);
    let catalog_written = if catalog_managed && !models.is_empty() {
        // 先写目录文件再写配置：指针落地时文件必已就位，中断也只会多出一个
        // 没被引用的文件，不会让 Codex 拿到悬空指针
        let desired = serde_json::to_string_pretty(&catalog_document(models))
            .map_err(|e| format!("序列化模型目录失败: {e}"))?;
        let differs =
            fs::read_to_string(&catalog_file).ok().as_deref() != Some(desired.as_str());
        if differs {
            write_atomic(&catalog_file, &desired)?;
        }
        differs
    } else {
        false
    };
    if config_written {
        write_atomic(&config_file, &doc.to_string())?;
    }
    if catalog_managed && models.is_empty() {
        // 撤目录：配置里的指针已先消失，此时删文件不会造成悬空引用；
        // 删不掉只是留个孤儿文件，不值得报错
        let _ = fs::remove_file(&catalog_file);
    }

    // auth.json 只清理旧版残留（早前版本把密钥写进了 OPENAI_API_KEY，可能还留下
    // auth_mode = "apikey"）：它承载的是官方登录身份，我们不往里写自己的东西。
    clean_auth_file(&auth_path(dir), api_key, true)?;
    Ok(CodexImportSummary {
        config_written,
        key_written,
        catalog_written,
        model_written,
        codex_running: codex_app_running(),
    })
}

/// 扫描 Codex 配置里指向本应用的 provider。
#[tauri::command]
pub async fn k2a_codex_list(state: State<'_, KiroProxyState>) -> Result<CodexScan, String> {
    let access = build_access(&state);
    let port = access.port;
    let api_key = current_api_key(&state);
    tauri::async_runtime::spawn_blocking(move || Ok(scan_files(port, api_key.as_deref())))
        .await
        .map_err(|e| format!("扫描任务失败: {e}"))?
}

fn scan_files(port: Option<u16>, api_key: Option<&str>) -> CodexScan {
    let Some(port) = port else {
        return CodexScan {
            error: Some("无法确定代理端口，无法判断哪些条目指向本应用".to_string()),
            ..CodexScan::default()
        };
    };
    let dir = match codex_dir() {
        Ok(dir) => dir,
        // 目录不存在视为「Codex 未安装」，不算错误 —— 界面按未安装引导。
        Err(_) => return CodexScan::default(),
    };
    let config_file = config_path(&dir);
    let config_text = match fs::read_to_string(&config_file) {
        Ok(raw) => Some(raw),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            return CodexScan {
                installed: true,
                error: Some(format!("读取 config.toml 失败: {err}")),
                ..CodexScan::default()
            };
        }
    };
    // 旧版把密钥写在 auth.json，读出来只作 provider 表缺 token 时的兜底
    let auth_key = read_auth(&auth_path(&dir))
        .ok()
        .and_then(|auth| auth.get("OPENAI_API_KEY").and_then(Value::as_str).map(str::to_string));

    let parsed = match config_text
        .as_deref()
        .unwrap_or("")
        .parse::<DocumentMut>()
    {
        Ok(doc) => doc,
        Err(e) => {
            return CodexScan {
                installed: true,
                error: Some(format!(
                    "config.toml 不是合法 TOML（{e}），请先在编辑器里检查该文件"
                )),
                ..CodexScan::default()
            };
        }
    };
    let entries = entries_from_doc(&parsed, port, auth_key.as_deref(), api_key);
    let (catalog_state, active_model) = catalog_pointer_from_doc(&parsed);
    let mut scan = CodexScan {
        installed: true,
        entries,
        error: None,
        catalog_state,
        catalog_models: Vec::new(),
        active_model: if catalog_state == CatalogState::Ours {
            active_model
        } else {
            None
        },
    };

    // 指针指向我们但文件缺失/坏掉会卡死 Codex 启动 —— 必须就地交代，光报
    // 「没模型」用户会以为只是没接入
    if catalog_state == CatalogState::Ours {
        match fs::read_to_string(catalog_path(&dir)) {
            Ok(text) => match parse_catalog_text(&text) {
                Ok(models) if !models.is_empty() => scan.catalog_models = models,
                Ok(_) => {
                    scan.error = Some(
                        "模型目录文件里没有模型，Codex 将无法启动；请重新接入或移除后重试"
                            .to_string(),
                    )
                }
                Err(e) => {
                    scan.error =
                        Some(format!("{e}，Codex 将无法启动；请重新接入或移除后重试"))
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                scan.error = Some(
                    "配置指向模型目录文件（kiro2api-model-catalog.json）但文件已不存在，Codex 将无法启动；请重新接入或移除后重试"
                        .to_string(),
                )
            }
            Err(err) => scan.error = Some(format!("读取模型目录文件失败: {err}")),
        }
    }
    scan
}

/// 从 Codex 配置里移除指向本应用的 provider（含顶层选择与 auth.json 里的密钥）。
/// 有接管前的快照时**整份还原**（见模块注释），没有则逐键删除。
/// 返回是否确有改动、是否走了「还原」，以及那一刻 Codex 桌面应用在不在跑 ——
/// 跑着才需要重启确认。指向其它服务的配置一概不动。
#[tauri::command]
pub async fn k2a_codex_remove(state: State<'_, KiroProxyState>) -> Result<CodexRemoveOutcome, String> {
    let port = build_access(&state)
        .port
        .ok_or_else(|| "无法确定代理端口，无法判断哪些条目指向本应用".to_string())?;
    let api_key = current_api_key(&state)
        .ok_or_else(|| "还没有代理密钥，无法识别要移除的密钥".to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        let (removed, restored) = remove_from_files(port, &api_key)?;
        let codex_running = codex_app_running();
        Ok(CodexRemoveOutcome {
            removed,
            restored,
            codex_running,
        })
    })
    .await
    .map_err(|e| format!("移除任务失败: {e}"))?
}

fn remove_from_files(port: u16, api_key: &str) -> Result<(bool, bool), String> {
    let dir = codex_dir()?;
    remove_from_dir(&dir, &backup_root(), port, api_key)
}

fn remove_from_dir(
    dir: &Path,
    backup: &Path,
    port: u16,
    api_key: &str,
) -> Result<(bool, bool), String> {
    // 接管前的快照还在：整份放回去。「删掉我们的键」只够让 Codex 回落到内置
    // openai，够不上用户点「取消接管」时想要的那个「还原」
    if restore_from_backup(backup, dir, api_key)? {
        return Ok((true, true));
    }

    // 没有快照（本次改动之前就接管过的）：退回逐键删除，至少不留悬空配置
    let config_file = config_path(dir);
    let mut doc = read_config(&config_file)?;
    let auth_file = auth_path(dir);
    let mut auth = read_auth(&auth_file)?;
    let (config_changed, auth_changed, catalog_removed) =
        remove_from_parts(&mut doc, &mut auth, port, api_key);
    // 指针先随配置消失，再删目录文件 —— 中断也只会留下无害的孤儿文件
    if config_changed {
        write_atomic(&config_file, &doc.to_string())?;
    }
    if auth_changed {
        if auth.as_object().is_some_and(|obj| obj.is_empty()) {
            let _ = fs::remove_file(&auth_file);
        } else {
            write_auth(&auth_file, &auth)?;
        }
    }
    if catalog_removed {
        let _ = fs::remove_file(catalog_path(dir));
    }
    Ok((config_changed || auth_changed || catalog_removed, false))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const PORT: u16 = 5680;
    const BASE_URL: &str = "http://127.0.0.1:5680/openai/v1";
    const API_KEY: &str = "k2a-1";

    #[test]
    fn import_creates_provider_and_activates_on_fresh_config() {
        let mut doc = "".parse::<DocumentMut>().unwrap();
        assert!(import_into_document(
            &mut doc,
            BASE_URL,
            API_KEY,
            &["gpt-5.2-codex".to_string()],
        ));
        let text = doc.to_string();
        let parsed: DocumentMut = text.parse().unwrap();
        let provider = &parsed["model_providers"][PROVIDER_ID];
        assert_eq!(provider["base_url"].as_str(), Some(BASE_URL));
        assert_eq!(provider["wire_api"].as_str(), Some("responses"));
        // 密钥进 provider 表，auth.json 一概不碰
        assert_eq!(provider[TOKEN_FIELD].as_str(), Some(API_KEY));
        assert!(provider.get("requires_openai_auth").is_none());
        assert_eq!(parsed["model_provider"].as_str(), Some(PROVIDER_ID));
        // 勾选了模型：接管目录指针，默认模型落到第一个
        assert_eq!(parsed["model_catalog_json"].as_str(), Some(CATALOG_FILENAME));
        assert_eq!(parsed["model"].as_str(), Some("gpt-5.2-codex"));
        assert_eq!(pointer_state_from_doc(&parsed), CatalogState::Ours);
    }

    #[test]
    fn import_preserves_comments_and_other_providers() {
        let input = r#"# 我的 Codex 配置
model = "gpt-5.3"
model_provider = "relay"

[model_providers.relay]
name = "Relay"
base_url = "https://relay.example/v1"
wire_api = "responses"

# 上面是别家中转，别动
"#;
        let mut doc = input.parse::<DocumentMut>().unwrap();
        assert!(import_into_document(
            &mut doc,
            BASE_URL,
            API_KEY,
            &["gpt-5.2".to_string(), "gpt-5.2-codex".to_string()],
        ));
        let text = doc.to_string();
        assert!(text.contains("# 我的 Codex 配置"));
        assert!(text.contains("# 上面是别家中转，别动"));
        assert!(text.contains("[model_providers.relay]"));
        assert!(text.contains("https://relay.example/v1"));
        let parsed: DocumentMut = text.parse().unwrap();
        assert_eq!(parsed["model_provider"].as_str(), Some(PROVIDER_ID));
        assert_eq!(parsed["model_catalog_json"].as_str(), Some(CATALOG_FILENAME));
        assert_eq!(parsed["model"].as_str(), Some("gpt-5.2"));
    }

    #[test]
    fn import_updates_ours_and_keeps_user_extras() {
        let input = r#"[model_providers.kiro2api]
name = "kiro2api"
base_url = "http://127.0.0.1:1111/openai/v1"
wire_api = "responses"
requires_openai_auth = true

[model_providers.kiro2api.env_http_headers]
X-Custom = "keep-me"
"#;
        let mut doc = input.parse::<DocumentMut>().unwrap();
        import_into_document(&mut doc, BASE_URL, API_KEY, &[]);
        let text = doc.to_string();
        assert!(text.contains("127.0.0.1:5680"));
        assert!(!text.contains("127.0.0.1:1111"));
        assert!(text.contains("keep-me"), "用户自己的键必须保留");
        assert!(
            !text.contains("requires_openai_auth"),
            "旧版写的 requires_openai_auth 必须清掉，否则 Codex 仍去读 auth.json"
        );
        let parsed: DocumentMut = text.parse().unwrap();
        assert_eq!(parsed["model_provider"].as_str(), Some(PROVIDER_ID));
        // 从没写过目录 + 空清单：不该冒出指针
        assert!(parsed.get("model_catalog_json").is_none());
        assert!(parsed.get("model").is_none());
    }

    #[test]
    fn import_respects_foreign_catalog_pointer() {
        let input = r#"model_catalog_json = "cc-switch-model-catalog.json"
model = "their-pick"
"#;
        let mut doc = input.parse::<DocumentMut>().unwrap();
        let models = vec!["gpt-5.2".to_string()];
        // 目录归别家管：目录与 model 不碰，provider 照常接管
        assert!(!import_into_document(&mut doc, BASE_URL, API_KEY, &models));
        let parsed: DocumentMut = doc.to_string().parse().unwrap();
        assert_eq!(
            parsed["model_catalog_json"].as_str(),
            Some("cc-switch-model-catalog.json")
        );
        assert_eq!(parsed["model"].as_str(), Some("their-pick"));
        assert_eq!(parsed["model_provider"].as_str(), Some(PROVIDER_ID));
    }

    #[test]
    fn import_keeps_codex_model_choice_when_it_is_ours() {
        let input = format!(
            "model_catalog_json = \"{CATALOG_FILENAME}\"\nmodel = \"gpt-5.2-codex\"\n"
        );
        let mut doc = input.parse::<DocumentMut>().unwrap();
        let models = vec!["gpt-5.2".to_string(), "gpt-5.2-codex".to_string()];
        assert!(import_into_document(&mut doc, BASE_URL, API_KEY, &models));
        let parsed: DocumentMut = doc.to_string().parse().unwrap();
        // 用户在 Codex /model 里选的还在清单里：保留，不重置成第一个
        assert_eq!(parsed["model"].as_str(), Some("gpt-5.2-codex"));
    }

    #[test]
    fn import_overwrite_keeps_comments_on_replaced_keys() {
        let input = "# 选型说明\nmodel = \"gpt-5.3\"\n\n# 用这个中转\nmodel_provider = \"relay\"\n";
        let mut doc = input.parse::<DocumentMut>().unwrap();
        import_into_document(&mut doc, BASE_URL, API_KEY, &["gpt-5.2".to_string()]);
        let text = doc.to_string();
        assert!(text.contains("# 选型说明"), "被覆盖键上的注释必须保留");
        assert!(text.contains("# 用这个中转"), "被覆盖键上的注释必须保留");
        let parsed: DocumentMut = text.parse().unwrap();
        assert_eq!(parsed["model"].as_str(), Some("gpt-5.2"));
        assert_eq!(parsed["model_provider"].as_str(), Some(PROVIDER_ID));
    }

    #[test]
    fn import_with_no_models_clears_ours_and_leaves_foreign() {
        let ours = format!("model_catalog_json = \"{CATALOG_FILENAME}\"\nmodel = \"gpt-5.2\"\n");
        let mut doc = ours.parse::<DocumentMut>().unwrap();
        assert!(import_into_document(&mut doc, BASE_URL, API_KEY, &[]));
        let parsed: DocumentMut = doc.to_string().parse().unwrap();
        assert!(parsed.get("model_catalog_json").is_none());
        assert!(parsed.get("model").is_none());
        assert_eq!(parsed["model_provider"].as_str(), Some(PROVIDER_ID));

        let foreign = "model_catalog_json = \"other.json\"\nmodel = \"their-model\"\n";
        let mut doc = foreign.parse::<DocumentMut>().unwrap();
        assert!(!import_into_document(&mut doc, BASE_URL, API_KEY, &[]));
        let parsed: DocumentMut = doc.to_string().parse().unwrap();
        assert_eq!(parsed["model_catalog_json"].as_str(), Some("other.json"));
        assert_eq!(parsed["model"].as_str(), Some("their-model"));
    }

    #[test]
    fn pointer_state_recognizes_ours_by_basename() {
        let none = "".parse::<DocumentMut>().unwrap();
        assert_eq!(pointer_state_from_doc(&none), CatalogState::None);
        let ours = format!("model_catalog_json = \"{CATALOG_FILENAME}\"")
            .parse::<DocumentMut>()
            .unwrap();
        assert_eq!(pointer_state_from_doc(&ours), CatalogState::Ours);
        // 绝对路径按 basename 认（TOML 单引号字面串，反斜杠不经转义）
        let absolute = r"model_catalog_json = 'C:\Users\me\.codex\kiro2api-model-catalog.json'"
            .parse::<DocumentMut>()
            .unwrap();
        assert_eq!(pointer_state_from_doc(&absolute), CatalogState::Ours);
        let foreign =
            r#"model_catalog_json = "/home/me/.codex/cc-switch-model-catalog.json""#
                .parse::<DocumentMut>()
                .unwrap();
        assert_eq!(pointer_state_from_doc(&foreign), CatalogState::Foreign);
        assert_eq!(basename_of("C:\\a\\b\\k.json"), "k.json");
        assert_eq!(basename_of("/a/b/k.json"), "k.json");
        assert_eq!(basename_of("k.json"), "k.json");
    }

    #[test]
    fn catalog_entry_covers_both_schema_generations() {
        let entry = catalog_entry("m1", 1000);
        assert_eq!(entry["slug"], "m1");
        assert_eq!(entry["display_name"], "m1");
        assert_eq!(entry["visibility"], "list");
        assert_eq!(entry["shell_type"], "shell_command");
        assert_eq!(entry["supported_in_api"], true);
        assert_eq!(entry["priority"], 1000);
        assert_eq!(entry["default_reasoning_level"], "high");
        assert_eq!(entry["truncation_policy"]["mode"], "tokens");
        let levels = entry["supported_reasoning_levels"].as_array().unwrap();
        assert_eq!(levels.len(), 3);
        // 旧世代必需的键在位（新版本会忽略）；base_instructions 必须是字符串
        assert_eq!(entry["base_instructions"].as_str(), Some(NEUTRAL_BASE_INSTRUCTIONS));
        assert_eq!(entry["supports_reasoning_summaries"], false);
        assert_eq!(entry["supports_parallel_tool_calls"], false);
        // 新世代必需的键在位（老版本会忽略）
        assert!(entry.get("availability_nux").is_some());
        assert!(entry.get("upgrade").is_some());
        assert!(entry.get("experimental_supported_tools").is_some());
    }

    /// Codex `ModelInfo` 的「必需键」镜像：只抄对应世代里没有 serde default 的
    /// 字段（Option 字段无 default 时同样必填），不写 deny_unknown_fields ——
    /// 与 Codex 的解析语义等价。目录文件反序列化失败 = 我们漏了必需键。
    mod schema_mirror {
        use serde::Deserialize;

        #[derive(Deserialize)]
        pub struct Preset {
            #[allow(dead_code)]
            pub effort: String,
            #[allow(dead_code)]
            pub description: String,
        }

        #[derive(Deserialize)]
        pub struct Truncation {
            #[allow(dead_code)]
            pub mode: String,
            #[allow(dead_code)]
            pub limit: i64,
        }

        #[derive(Deserialize)]
        pub struct Models<T> {
            #[allow(dead_code)]
            pub models: Vec<T>,
        }

        /// openai/codex 当前 main 的必需键（2026-02 逐字段核对过源码）
        #[derive(Deserialize)]
        pub struct Current {
            #[allow(dead_code)]
            pub slug: String,
            #[allow(dead_code)]
            pub display_name: String,
            #[allow(dead_code)]
            pub description: Option<String>,
            #[allow(dead_code)]
            pub supported_reasoning_levels: Vec<Preset>,
            #[allow(dead_code)]
            pub shell_type: String,
            #[allow(dead_code)]
            pub visibility: String,
            #[allow(dead_code)]
            pub supported_in_api: bool,
            #[allow(dead_code)]
            pub priority: i32,
            #[allow(dead_code)]
            pub availability_nux: Option<serde_json::Value>,
            #[allow(dead_code)]
            pub upgrade: Option<serde_json::Value>,
            #[allow(dead_code)]
            pub support_verbosity: bool,
            #[allow(dead_code)]
            pub default_verbosity: Option<String>,
            #[allow(dead_code)]
            pub apply_patch_tool_type: Option<String>,
            #[allow(dead_code)]
            pub truncation_policy: Truncation,
            #[allow(dead_code)]
            pub experimental_supported_tools: Vec<String>,
        }

        /// cc-switch 0.148 世代的必需键（模板形状 + #6661）
        #[derive(Deserialize)]
        pub struct Legacy {
            #[allow(dead_code)]
            pub slug: String,
            #[allow(dead_code)]
            pub display_name: String,
            #[allow(dead_code)]
            pub description: Option<String>,
            #[allow(dead_code)]
            pub base_instructions: String,
            #[allow(dead_code)]
            pub supports_reasoning_summaries: bool,
            #[allow(dead_code)]
            pub supports_parallel_tool_calls: bool,
            #[allow(dead_code)]
            pub supported_reasoning_levels: Vec<Preset>,
            #[allow(dead_code)]
            pub shell_type: String,
            #[allow(dead_code)]
            pub visibility: String,
            #[allow(dead_code)]
            pub supported_in_api: bool,
            #[allow(dead_code)]
            pub priority: i32,
            #[allow(dead_code)]
            pub support_verbosity: bool,
            #[allow(dead_code)]
            pub truncation_policy: Truncation,
        }

        pub fn parse<T: for<'de> Deserialize<'de>>(text: &str) -> Result<usize, String> {
            let parsed: Models<T> = serde_json::from_str(text)
                .map_err(|e| format!("目录文件按 Codex 的 schema 解析失败: {e}"))?;
            Ok(parsed.models.len())
        }
    }

    #[test]
    fn catalog_document_parses_as_both_codex_generations() {
        let text = serde_json::to_string(&catalog_document(&[
            "gpt-5.2".to_string(),
            "gpt-5.2-codex".to_string(),
        ]))
        .unwrap();
        let current = schema_mirror::parse::<schema_mirror::Current>(&text).unwrap();
        assert_eq!(current, 2);
        let legacy = schema_mirror::parse::<schema_mirror::Legacy>(&text).unwrap();
        assert_eq!(legacy, 2);
    }

    #[test]
    fn parse_catalog_text_reads_slugs_and_rejects_junk() {
        let text = serde_json::to_string(&catalog_document(&["m1".to_string(), "m2".to_string()]))
            .unwrap();
        assert_eq!(parse_catalog_text(&text).unwrap(), vec!["m1", "m2"]);
        assert!(parse_catalog_text("{}").is_err());
        assert!(parse_catalog_text(r#"{"models": []}"#).is_ok_and(|m| m.is_empty()));
        assert!(parse_catalog_text(r#"{"models": [{"display_name": "x"}]}"#).is_err());
        assert!(parse_catalog_text("not json").is_err());
    }

    #[test]
    fn clean_models_trims_dedups_and_keeps_gpt_only() {
        assert_eq!(
            clean_models(vec![
                " gpt-5.2 ".to_string(),
                String::new(),
                "gpt-5.2".to_string(),
                "claude-sonnet-5".to_string(),
                "deepseek-v3".to_string(),
                "gpt-5.6-terra".to_string(),
            ]),
            vec!["gpt-5.2".to_string(), "gpt-5.6-terra".to_string()]
        );
    }

    #[test]
    fn scan_finds_only_matching_entries() {
        let config = format!(
            r#"model_provider = "{PROVIDER_ID}"

[model_providers.{PROVIDER_ID}]
name = "{PROVIDER_ID}"
base_url = "{BASE_URL}"
wire_api = "responses"

[model_providers.other]
name = "other"
base_url = "http://10.0.0.2:5680/v1"
wire_api = "responses"

[model_providers.stale]
name = "stale"
base_url = "http://127.0.0.1:9999/openai/v1"
wire_api = "responses"
"#
        );
        let entries_from = |stored: Option<&str>, current: Option<&str>| {
            let doc = config.parse::<DocumentMut>().unwrap();
            entries_from_doc(&doc, PORT, stored, current)
        };
        let entries = entries_from(Some("k2a-1"), Some("k2a-1"));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, PROVIDER_ID);
        assert!(entries[0].active);
        assert!(entries[0].key_matches);

        let inactive = entries_from(Some("k2a-2"), Some("k2a-1"));
        assert!(!inactive[0].key_matches);

        // 新版把密钥写在 provider 表：表里的值优先于 auth.json 兜底
        let with_token = format!(
            "model_provider = \"{PROVIDER_ID}\"\n\n[model_providers.{PROVIDER_ID}]\nbase_url = \"{BASE_URL}\"\n{TOKEN_FIELD} = \"k2a-2\"\n"
        );
        let doc = with_token.parse::<DocumentMut>().unwrap();
        let entries = entries_from_doc(&doc, PORT, Some("k2a-1"), Some("k2a-1"));
        assert!(!entries[0].key_matches, "表里的密钥与当前不一致要报出来");

        // 没配过（config.toml 不存在按空文档算）：空清单
        let empty = "".parse::<DocumentMut>().unwrap();
        assert!(entries_from_doc(&empty, PORT, None, None).is_empty());
        // 坏 TOML 解析不成文档 —— scan_files 会把它就地放进 error 而不是抛命令错误
        assert!("not toml [[[".parse::<DocumentMut>().is_err());
    }

    #[test]
    fn clean_residue_removes_our_key_and_keeps_official_login() {
        let mut auth = json!({
            "OPENAI_API_KEY": "k2a-1",
            "tokens": { "access_token": "chatgpt" },
            "last_refresh": "2026-01-01",
        });
        assert!(clean_auth_residue(&mut auth, "k2a-1"));
        assert!(auth.get("OPENAI_API_KEY").is_none());
        assert_eq!(auth["tokens"]["access_token"], "chatgpt");
        // 幂等
        assert!(!clean_auth_residue(&mut auth, "k2a-1"));
    }

    #[test]
    fn clean_residue_recognizes_rotated_key_by_shape() {
        // 重置过密钥：旧残留与当前密钥对不上，按 `k2a-` 形态仍要认得出
        let mut auth = json!({ "OPENAI_API_KEY": "k2a-old" });
        assert!(clean_auth_residue(&mut auth, "k2a-new"));
        assert!(auth.as_object().unwrap().is_empty());
        // 用户自己的密钥一概不动
        let mut own = json!({ "OPENAI_API_KEY": "sk-user-own" });
        assert!(!clean_auth_residue(&mut own, "k2a-new"));
        assert_eq!(own["OPENAI_API_KEY"], "sk-user-own");
    }

    #[test]
    fn clean_residue_drops_dead_api_key_mode() {
        // 只删密钥会留下 auth_mode = "apikey"：显式模式优先于字段存在性，
        // Codex 仍按 API key 身份运行（账号卡在第三方），必须一起撤
        let mut dead = json!({ "auth_mode": "apikey", "OPENAI_API_KEY": "k2a-1" });
        assert!(clean_auth_residue(&mut dead, "k2a-1"));
        assert!(dead.as_object().unwrap().is_empty());

        // 真凭据在场时 auth_mode 不能当残留删
        let mut logged_in = json!({ "auth_mode": "apikey", "tokens": { "access_token": "x" } });
        assert!(!clean_auth_residue(&mut logged_in, "k2a-1"));
        assert_eq!(logged_in["auth_mode"], "apikey");
    }

    #[test]
    fn remove_touches_only_ours() {
        let config = format!(
            r#"model_provider = "{PROVIDER_ID}"
model_catalog_json = "{CATALOG_FILENAME}"
model = "gpt-5.2"

[model_providers.{PROVIDER_ID}]
base_url = "{BASE_URL}"
wire_api = "responses"

[model_providers.other]
base_url = "https://relay.example/v1"
"#
        );
        let mut doc = config.parse::<DocumentMut>().unwrap();
        let mut auth = json!({ "OPENAI_API_KEY": "k2a-1", "tokens": { "a": 1 } });
        let (config_changed, auth_changed, catalog_removed) =
            remove_from_parts(&mut doc, &mut auth, PORT, "k2a-1");
        assert!(config_changed && auth_changed && catalog_removed);
        let text = doc.to_string();
        assert!(!text.contains("[model_providers.kiro2api]"));
        assert!(text.contains("[model_providers.other]"));
        // 顶层选择一并归还（model_providers 表头仍会含这个词，按解析结果断言）
        let parsed: DocumentMut = text.parse().unwrap();
        assert!(parsed.get("model_provider").is_none());
        assert!(parsed.get("model_catalog_json").is_none());
        assert!(parsed.get("model").is_none());
        assert!(auth.get("OPENAI_API_KEY").is_none());
        assert_eq!(auth["tokens"]["a"], 1);
    }

    #[test]
    fn remove_ignores_foreign_base_url_and_foreign_key() {
        // 表存在但 base_url 已被用户改成别的服务：不删表，也不动密钥；
        // 目录指针指向别家时同样原样保留（model 也别家在用）
        let config = format!(
            r#"model_catalog_json = "cc-switch-model-catalog.json"
model = "their-pick"

[model_providers.{PROVIDER_ID}]
base_url = "https://elsewhere.example/v1"
"#
        );
        let mut doc = config.parse::<DocumentMut>().unwrap();
        let mut auth = json!({ "OPENAI_API_KEY": "user-own-key" });
        let (config_changed, auth_changed, catalog_removed) =
            remove_from_parts(&mut doc, &mut auth, PORT, "k2a-1");
        assert!(!config_changed && !auth_changed && !catalog_removed);
        assert!(doc.to_string().contains("elsewhere.example"));
        assert!(doc.to_string().contains("cc-switch-model-catalog.json"));
        assert_eq!(auth["OPENAI_API_KEY"], "user-own-key");
    }

    #[test]
    fn remove_resets_top_level_even_without_table() {
        // 顶层还指着我们但表已没了（用户手删）：把悬空的顶层键清掉
        let mut doc = format!("model_provider = \"{PROVIDER_ID}\"\n")
            .parse::<DocumentMut>()
            .unwrap();
        let mut auth = Value::Object(Default::default());
        let (config_changed, _, catalog_removed) =
            remove_from_parts(&mut doc, &mut auth, PORT, "k2a-1");
        assert!(config_changed);
        assert!(!catalog_removed);
        assert!(!doc.to_string().contains("model_provider"));
    }

    #[test]
    fn remove_clears_catalog_pointer_even_without_provider_table() {
        // 指针与顶层 model 还在但 provider 表已没了（用户手删）：一并清掉，
        // 否则 Codex 起不来
        let config = format!("model_catalog_json = \"{CATALOG_FILENAME}\"\nmodel = \"gpt-5.2\"\n");
        let mut doc = config.parse::<DocumentMut>().unwrap();
        let mut auth = Value::Object(Default::default());
        let (config_changed, _, catalog_removed) =
            remove_from_parts(&mut doc, &mut auth, PORT, "k2a-1");
        assert!(config_changed && catalog_removed);
        let parsed: DocumentMut = doc.to_string().parse().unwrap();
        assert!(parsed.get("model_catalog_json").is_none());
        assert!(parsed.get("model").is_none());
    }

    /// 一次接管的临时现场：(Codex 配置目录, 快照目录)。
    fn takeover_dirs(tag: &str) -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("k2a-codex-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let codex = base.join("codex");
        fs::create_dir_all(&codex).unwrap();
        (codex, base.join("backup"))
    }

    /// 用户接管前就存在的一份配置：别的中转 + 注释 + 自己的选型。
    const ORIGINAL_CONFIG: &str = r#"# 我的 Codex 配置
model = "gpt-5.3"
model_provider = "relay"

[model_providers.relay]
name = "Relay"
base_url = "https://relay.example/v1"
wire_api = "responses"
"#;

    #[test]
    fn clear_restores_the_whole_config_from_before_takeover() {
        let (dir, backup) = takeover_dirs("restore");
        fs::write(config_path(&dir), ORIGINAL_CONFIG).unwrap();
        fs::write(
            auth_path(&dir),
            json!({ "tokens": { "access_token": "chatgpt" } }).to_string(),
        )
        .unwrap();

        import_to_dir(&dir, &backup, BASE_URL, "k2a-1", &["gpt-5.2".to_string()]).unwrap();
        let taken = fs::read_to_string(config_path(&dir)).unwrap();
        assert!(taken.contains(&format!("[model_providers.{PROVIDER_ID}]")));
        assert!(taken.contains("k2a-1") || taken.contains(CATALOG_FILENAME));

        let (removed, restored) = remove_from_dir(&dir, &backup, PORT, "k2a-1").unwrap();
        assert!(removed && restored);
        // 整份一致：顶层选型、别家中转、注释都在
        assert_eq!(fs::read_to_string(config_path(&dir)).unwrap(), ORIGINAL_CONFIG);
        // 密钥：接管前没有 → 把我们写进去的删掉，ChatGPT token 原样留着
        let auth: Value =
            serde_json::from_str(&fs::read_to_string(auth_path(&dir)).unwrap()).unwrap();
        assert!(auth.get("OPENAI_API_KEY").is_none());
        assert_eq!(auth["tokens"]["access_token"], "chatgpt");
        // 我们写的目录文件不该留下孤儿
        assert!(!catalog_path(&dir).exists());
        // 快照用完即弃
        assert!(!backup_meta_path(&backup).exists());
        fs::remove_dir_all(dir.parent().unwrap()).ok();
    }

    #[test]
    fn takeover_never_touches_user_auth_json() {
        let (dir, backup) = takeover_dirs("authkey");
        fs::write(config_path(&dir), ORIGINAL_CONFIG).unwrap();
        fs::write(
            auth_path(&dir),
            json!({ "OPENAI_API_KEY": "sk-user-own", "tokens": { "a": 1 } }).to_string(),
        )
        .unwrap();

        import_to_dir(&dir, &backup, BASE_URL, "k2a-1", &["gpt-5.2".to_string()]).unwrap();
        // 密钥进 config.toml，auth.json 一字不动 —— 官方登录态与账号显示不受影响
        let taken: Value =
            serde_json::from_str(&fs::read_to_string(auth_path(&dir)).unwrap()).unwrap();
        assert_eq!(taken["OPENAI_API_KEY"], "sk-user-own");
        let config = fs::read_to_string(config_path(&dir)).unwrap();
        assert!(config.contains("k2a-1"));

        remove_from_dir(&dir, &backup, PORT, "k2a-1").unwrap();
        let after: Value =
            serde_json::from_str(&fs::read_to_string(auth_path(&dir)).unwrap()).unwrap();
        // 用户自己的密钥必须还在（不能因为我们清空就把它删了）
        assert_eq!(after["OPENAI_API_KEY"], "sk-user-own");
        assert_eq!(after["tokens"]["a"], 1);
        fs::remove_dir_all(dir.parent().unwrap()).ok();
    }

    #[test]
    fn legacy_auth_residue_is_cleaned_on_both_sides() {
        // 旧版接管留下的现场：auth.json 里有我们的密钥 + auth_mode = "apikey"
        let (dir, backup) = takeover_dirs("legacy");
        fs::write(config_path(&dir), ORIGINAL_CONFIG).unwrap();
        fs::write(
            auth_path(&dir),
            json!({ "auth_mode": "apikey", "OPENAI_API_KEY": "k2a-old" }).to_string(),
        )
        .unwrap();

        // 点「应用」即迁移：密钥挪进 provider 表，auth.json 的残留清空并删文件
        import_to_dir(&dir, &backup, BASE_URL, "k2a-1", &["gpt-5.2".to_string()]).unwrap();
        assert!(!auth_path(&dir).exists(), "旧残留清空后 auth.json 不该留着");

        // 再放一份残留，走「取消接入」：同样要清干净（密钥已重置也对得上）
        fs::write(
            auth_path(&dir),
            json!({ "auth_mode": "apikey", "OPENAI_API_KEY": "k2a-older" }).to_string(),
        )
        .unwrap();
        remove_from_dir(&dir, &backup, PORT, "k2a-1").unwrap();
        assert!(!auth_path(&dir).exists());
        fs::remove_dir_all(dir.parent().unwrap()).ok();
    }

    #[test]
    fn reapplying_takeover_keeps_the_original_snapshot() {
        let (dir, backup) = takeover_dirs("reapply");
        fs::write(config_path(&dir), ORIGINAL_CONFIG).unwrap();
        import_to_dir(&dir, &backup, BASE_URL, "k2a-1", &["gpt-5.2".to_string()]).unwrap();
        // 第二次点「应用」：配置里已经是我们的内容，快照不能被它覆盖
        import_to_dir(
            &dir,
            &backup,
            BASE_URL,
            "k2a-1",
            &["gpt-5.2".to_string(), "gpt-5.3-codex".to_string()],
        )
        .unwrap();
        remove_from_dir(&dir, &backup, PORT, "k2a-1").unwrap();
        assert_eq!(fs::read_to_string(config_path(&dir)).unwrap(), ORIGINAL_CONFIG);
        fs::remove_dir_all(dir.parent().unwrap()).ok();
    }

    #[test]
    fn clear_removes_config_we_created_from_nothing() {
        let (dir, backup) = takeover_dirs("fresh");
        // 接管前：没有 config.toml，也没有 auth.json
        import_to_dir(&dir, &backup, BASE_URL, "k2a-1", &["gpt-5.2".to_string()]).unwrap();
        assert!(config_path(&dir).is_file());
        // 密钥写在 provider 表里，auth.json 连文件都不该冒出来
        assert!(!auth_path(&dir).exists());

        let (removed, restored) = remove_from_dir(&dir, &backup, PORT, "k2a-1").unwrap();
        assert!(removed && restored);
        // 还回「不存在」，而不是留一份空文件
        assert!(!config_path(&dir).exists());
        assert!(!auth_path(&dir).exists());
        fs::remove_dir_all(dir.parent().unwrap()).ok();
    }

    #[test]
    fn clear_without_snapshot_falls_back_to_key_removal() {
        // 本次改动之前就接管过的用户：没有快照，退回逐键删除（至少不留悬空配置）
        let (dir, backup) = takeover_dirs("nosnapshot");
        let mut doc: DocumentMut = ORIGINAL_CONFIG.parse().unwrap();
        import_into_document(&mut doc, BASE_URL, API_KEY, &["gpt-5.2".to_string()]);
        fs::write(config_path(&dir), doc.to_string()).unwrap();
        fs::write(auth_path(&dir), json!({ "OPENAI_API_KEY": "k2a-1" }).to_string()).unwrap();

        let (removed, restored) = remove_from_dir(&dir, &backup, PORT, "k2a-1").unwrap();
        assert!(removed);
        assert!(!restored, "没有快照时不能说成「已还原」");
        let parsed: DocumentMut =
            fs::read_to_string(config_path(&dir)).unwrap().parse().unwrap();
        assert!(parsed.get("model_provider").is_none());
        assert!(parsed["model_providers"].get(PROVIDER_ID).is_none());
        // 但没快照就没有旧值可放回：顶层 model 是删掉而不是还原成 gpt-5.3
        assert!(parsed.get("model").is_none());
        fs::remove_dir_all(dir.parent().unwrap()).ok();
    }

    #[test]
    fn broken_snapshot_falls_back_to_key_removal() {
        let (dir, backup) = takeover_dirs("brokensnap");
        fs::write(config_path(&dir), ORIGINAL_CONFIG).unwrap();
        import_to_dir(&dir, &backup, BASE_URL, "k2a-1", &["gpt-5.2".to_string()]).unwrap();
        // 快照里的 config.toml 丢了（磁盘问题、被清理工具删掉…）
        fs::remove_file(backup_config_path(&backup)).unwrap();

        let (removed, restored) = remove_from_dir(&dir, &backup, PORT, "k2a-1").unwrap();
        assert!(removed);
        assert!(!restored);
        let parsed: DocumentMut =
            fs::read_to_string(config_path(&dir)).unwrap().parse().unwrap();
        assert!(parsed.get("model_provider").is_none());
        // 残档必须丢掉：留着它会让下一次接管跳过快照，永远没有还原点
        assert!(!backup_meta_path(&backup).exists());
        fs::remove_dir_all(dir.parent().unwrap()).ok();
    }

    #[test]
    fn snapshot_skips_when_backup_dir_already_has_meta() {
        let (dir, backup) = takeover_dirs("snaponce");
        fs::write(config_path(&dir), ORIGINAL_CONFIG).unwrap();
        assert!(snapshot_before_takeover(&backup, &dir).unwrap());
        // 中间用户改了配置，再拍一次不该覆盖第一份
        fs::write(config_path(&dir), "model = \"changed\"\n").unwrap();
        assert!(!snapshot_before_takeover(&backup, &dir).unwrap());
        assert_eq!(
            fs::read_to_string(backup_config_path(&backup)).unwrap(),
            ORIGINAL_CONFIG
        );
        fs::remove_dir_all(dir.parent().unwrap()).ok();
    }

    #[test]
    fn atomic_write_round_trip() {        let dir = std::env::temp_dir().join(format!("k2a-codex-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        write_atomic(&path, "a = 1\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "a = 1\n");
        write_atomic(&path, "a = 2\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "a = 2\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
