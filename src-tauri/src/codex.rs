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
//! * `requires_openai_auth = true` 时 Codex 用 `auth.json` 里 `OPENAI_API_KEY`
//!   作为 Bearer 凭据 —— 与 cc-switch 的第三方接入模板同一条路。
//!
//! 「接入」= 写 provider 表（base_url 指向本代理 /openai/v1）+ 把顶层
//! `model_provider` 指到它 + 把密钥写进 auth.json。Codex 一次只有一个活跃
//! provider，因此接入必然「占用」顶层选择 —— 移除时把指向我们的顶层键删掉，
//! Codex 回落到内置 openai provider。

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;
use toml_edit::{table, value, DocumentMut, Item, Table};
use tauri::State;

use crate::kiro::{build_access, current_api_key, KiroProxyState};
use crate::workbuddy::origin_matches;

/// 本应用在 Codex 里的 provider id。写配置与扫描识别都用它。
const PROVIDER_ID: &str = "kiro2api";

/// 一条指向本应用的 Codex provider。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexEntry {
    pub id: String,
    pub base_url: String,
    /// 顶层 `model_provider` 是否指向它（即 Codex 当前正在用它）。
    pub active: bool,
    /// auth.json 里的密钥与当前代理密钥是否一致；false 时界面提示重置密钥已让它过期。
    pub key_matches: bool,
}

/// 扫描结果。读盘失败不作为命令错误抛出 —— 界面就地说明原因（与 WorkBuddy 同一取舍）。
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CodexScan {
    pub installed: bool,
    pub entries: Vec<CodexEntry>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexImportSummary {
    pub config_written: bool,
    pub auth_written: bool,
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

/// 扫描的核心（纯函数）：从 config.toml 文本里找出指向本应用的 provider。
/// `stored_key` 是 auth.json 里的 OPENAI_API_KEY；密钥一致性只对「存了 key 且
/// 与当前代理密钥一致」的条目成立（与 WorkBuddy 的 keyMatches 同义）。
fn scan_config_text(
    config_text: Option<&str>,
    stored_key: Option<&str>,
    current_key: Option<&str>,
    port: u16,
) -> Result<Vec<CodexEntry>, String> {
    let Some(text) = config_text else {
        return Ok(Vec::new());
    };
    let doc = text
        .parse::<DocumentMut>()
        .map_err(|e| format!("config.toml 不是合法 TOML（{e}），请先在编辑器里检查该文件"))?;
    let active_provider = doc
        .get("model_provider")
        .and_then(|item| item.as_str())
        .unwrap_or("");
    let Some(providers) = doc.get("model_providers").and_then(|item| item.as_table_like()) else {
        return Ok(Vec::new());
    };
    let key_matches = matches!(
        (stored_key, current_key),
        (Some(stored), Some(current)) if stored.trim() == current.trim()
    );
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
        entries.push(CodexEntry {
            id: id.to_string(),
            base_url: base_url.to_string(),
            active: id == active_provider,
            key_matches,
        });
    }
    Ok(entries)
}

/// 接入的核心（纯函数）：在文档上写/更新 provider 表并把顶层 model_provider
/// 指向它。返回配置是否发生变化。只设置本应用的四个键，provider 表里用户自己
/// 加的其余键（http_headers 等）原样保留。
fn import_into_document(doc: &mut DocumentMut, base_url: &str) -> bool {
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
    provider.insert("requires_openai_auth", value(true));
    doc.insert("model_provider", value(PROVIDER_ID));
    true
}

/// 密钥合并的核心（纯函数）：把 OPENAI_API_KEY 写进 auth 对象，其余字段
/// （ChatGPT 登录的 tokens 等）原样保留。返回是否发生变化。
fn merge_auth_key(auth: &mut Value, api_key: &str) -> bool {
    let Some(obj) = auth.as_object_mut() else {
        return false;
    };
    let changed = obj
        .get("OPENAI_API_KEY")
        .and_then(Value::as_str)
        .map(str::trim)
        != Some(api_key.trim());
    if changed {
        obj.insert(
            "OPENAI_API_KEY".to_string(),
            Value::String(api_key.to_string()),
        );
    }
    changed
}

/// 移除的核心（纯函数）：删掉指向本应用的 provider 表与指向它的顶层选择，
/// 并从 auth 对象里删掉与我们当前密钥一致的 OPENAI_API_KEY（用户自己的密钥不动）。
/// 返回 (配置是否变化, 密钥是否变化)。
fn remove_from_parts(
    doc: &mut DocumentMut,
    auth: &mut Value,
    port: u16,
    api_key: &str,
) -> (bool, bool) {
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

    let mut auth_changed = false;
    if let Some(obj) = auth.as_object_mut() {
        let ours_key = obj
            .get("OPENAI_API_KEY")
            .and_then(Value::as_str)
            .map(str::trim)
            == Some(api_key.trim());
        if ours_key {
            obj.remove("OPENAI_API_KEY");
            auth_changed = true;
        }
    }
    (config_changed, auth_changed)
}

/// 接入 Codex：写 provider 表、激活它、把密钥写进 auth.json。
/// 地址与密钥都取自后端当前配置 —— 密钥不经过前端，与「复制」同一条纪律。
#[tauri::command]
pub async fn k2a_codex_import(state: State<'_, KiroProxyState>) -> Result<CodexImportSummary, String> {
    let access = build_access(&state);
    let api_key = current_api_key(&state)
        .ok_or_else(|| "还没有代理密钥，请先启动一次代理生成密钥".to_string())?;
    // 地址由配置端口决定（见 `build_access`），停止状态同样有值 —— 接入与代理
    // 是否在跑无关，写的是 Codex 自己的配置。
    if access.openai_base_url.is_empty() {
        return Err("无法确定代理端口，无法构造接入地址".to_string());
    }
    let base_url = access.openai_base_url;
    tauri::async_runtime::spawn_blocking(move || import_to_files(&base_url, &api_key))
        .await
        .map_err(|e| format!("接入任务失败: {e}"))?
}

fn import_to_files(base_url: &str, api_key: &str) -> Result<CodexImportSummary, String> {
    let dir = codex_dir()?;
    let config_file = config_path(&dir);
    let mut doc = read_config(&config_file)?;
    let before = doc.to_string();
    import_into_document(&mut doc, base_url);
    let config_written = doc.to_string() != before;
    if config_written {
        write_atomic(&config_file, &doc.to_string())?;
    }

    let auth_file = auth_path(&dir);
    let mut auth = read_auth(&auth_file)?;
    let auth_written = merge_auth_key(&mut auth, api_key);
    if auth_written {
        write_atomic(&auth_file, &serde_json::to_string(&auth).map_err(|e| format!("序列化失败: {e}"))?)?;
    }
    Ok(CodexImportSummary {
        config_written,
        auth_written,
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
    let auth_key = read_auth(&auth_path(&dir))
        .ok()
        .and_then(|auth| auth.get("OPENAI_API_KEY").and_then(Value::as_str).map(str::to_string));
    match scan_config_text(config_text.as_deref(), auth_key.as_deref(), api_key, port) {
        Ok(entries) => CodexScan {
            installed: true,
            entries,
            error: None,
        },
        Err(error) => CodexScan {
            installed: true,
            error: Some(error),
            ..CodexScan::default()
        },
    }
}

/// 从 Codex 配置里移除指向本应用的 provider（含顶层选择与 auth.json 里的密钥）。
/// 返回是否确有移除 —— 指向其它服务的配置一概不动。
#[tauri::command]
pub async fn k2a_codex_remove(state: State<'_, KiroProxyState>) -> Result<bool, String> {
    let port = build_access(&state)
        .port
        .ok_or_else(|| "无法确定代理端口，无法判断哪些条目指向本应用".to_string())?;
    let api_key = current_api_key(&state)
        .ok_or_else(|| "还没有代理密钥，无法识别要移除的密钥".to_string())?;
    tauri::async_runtime::spawn_blocking(move || remove_from_files(port, &api_key))
        .await
        .map_err(|e| format!("移除任务失败: {e}"))?
}

fn remove_from_files(port: u16, api_key: &str) -> Result<bool, String> {
    let dir = codex_dir()?;
    let config_file = config_path(&dir);
    let mut doc = read_config(&config_file)?;
    let auth_file = auth_path(&dir);
    let mut auth = read_auth(&auth_file)?;
    let (config_changed, auth_changed) = remove_from_parts(&mut doc, &mut auth, port, api_key);
    if config_changed {
        write_atomic(&config_file, &doc.to_string())?;
    }
    if auth_changed {
        write_atomic(
            &auth_file,
            &serde_json::to_string(&auth).map_err(|e| format!("序列化失败: {e}"))?,
        )?;
    }
    Ok(config_changed || auth_changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const PORT: u16 = 5680;
    const BASE_URL: &str = "http://127.0.0.1:5680/openai/v1";

    #[test]
    fn import_creates_provider_and_activates_on_fresh_config() {
        let mut doc = "".parse::<DocumentMut>().unwrap();
        assert!(import_into_document(&mut doc, BASE_URL));
        let text = doc.to_string();
        let parsed: DocumentMut = text.parse().unwrap();
        let provider = &parsed["model_providers"][PROVIDER_ID];
        assert_eq!(provider["base_url"].as_str(), Some(BASE_URL));
        assert_eq!(provider["wire_api"].as_str(), Some("responses"));
        assert_eq!(provider["requires_openai_auth"].as_bool(), Some(true));
        assert_eq!(parsed["model_provider"].as_str(), Some(PROVIDER_ID));
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
        assert!(import_into_document(&mut doc, BASE_URL));
        let text = doc.to_string();
        assert!(text.contains("# 我的 Codex 配置"));
        assert!(text.contains("# 上面是别家中转，别动"));
        assert!(text.contains("[model_providers.relay]"));
        assert!(text.contains("https://relay.example/v1"));
        let parsed: DocumentMut = text.parse().unwrap();
        assert_eq!(parsed["model_provider"].as_str(), Some(PROVIDER_ID));
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
        import_into_document(&mut doc, BASE_URL);
        let text = doc.to_string();
        assert!(text.contains("127.0.0.1:5680"));
        assert!(!text.contains("127.0.0.1:1111"));
        assert!(text.contains("keep-me"), "用户自己的键必须保留");
        let parsed: DocumentMut = text.parse().unwrap();
        assert_eq!(parsed["model_provider"].as_str(), Some(PROVIDER_ID));
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
        let entries = scan_config_text(Some(&config), Some("k2a-1"), Some("k2a-1"), PORT).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, PROVIDER_ID);
        assert!(entries[0].active);
        assert!(entries[0].key_matches);

        let inactive = scan_config_text(Some(&config), Some("k2a-2"), Some("k2a-1"), PORT).unwrap();
        assert!(!inactive[0].key_matches);

        // 没配过：文件不存在
        assert!(scan_config_text(None, None, None, PORT).unwrap().is_empty());
        // 坏 TOML 上抛为错误（界面就地呈现）
        assert!(scan_config_text(Some("not toml [[["), None, None, PORT).is_err());
    }

    #[test]
    fn merge_auth_key_preserves_other_fields() {
        let mut auth = json!({
            "tokens": { "access_token": "chatgpt" },
            "last_refresh": "2026-01-01",
        });
        assert!(merge_auth_key(&mut auth, "k2a-1"));
        assert_eq!(auth["OPENAI_API_KEY"], "k2a-1");
        assert_eq!(auth["tokens"]["access_token"], "chatgpt");
        // 幂等
        assert!(!merge_auth_key(&mut auth, "k2a-1"));
    }

    #[test]
    fn remove_touches_only_ours() {
        let config = format!(
            r#"model_provider = "{PROVIDER_ID}"

[model_providers.{PROVIDER_ID}]
base_url = "{BASE_URL}"
wire_api = "responses"

[model_providers.other]
base_url = "https://relay.example/v1"
"#
        );
        let mut doc = config.parse::<DocumentMut>().unwrap();
        let mut auth = json!({ "OPENAI_API_KEY": "k2a-1", "tokens": { "a": 1 } });
        let (config_changed, auth_changed) =
            remove_from_parts(&mut doc, &mut auth, PORT, "k2a-1");
        assert!(config_changed && auth_changed);
        let text = doc.to_string();
        assert!(!text.contains("[model_providers.kiro2api]"));
        assert!(text.contains("[model_providers.other]"));
        // 顶层选择一并归还（model_providers 表头仍会含这个词，按解析结果断言）
        let parsed: DocumentMut = text.parse().unwrap();
        assert!(parsed.get("model_provider").is_none());
        assert!(auth.get("OPENAI_API_KEY").is_none());
        assert_eq!(auth["tokens"]["a"], 1);
    }

    #[test]
    fn remove_ignores_foreign_base_url_and_foreign_key() {
        // 表存在但 base_url 已被用户改成别的服务：不删表，也不动密钥
        let config = format!(
            r#"[model_providers.{PROVIDER_ID}]
base_url = "https://elsewhere.example/v1"
"#
        );
        let mut doc = config.parse::<DocumentMut>().unwrap();
        let mut auth = json!({ "OPENAI_API_KEY": "user-own-key" });
        let (config_changed, auth_changed) =
            remove_from_parts(&mut doc, &mut auth, PORT, "k2a-1");
        assert!(!config_changed && !auth_changed);
        assert!(doc.to_string().contains("elsewhere.example"));
        assert_eq!(auth["OPENAI_API_KEY"], "user-own-key");
    }

    #[test]
    fn remove_resets_top_level_even_without_table() {
        // 顶层还指着我们但表已没了（用户手删）：把悬空的顶层键清掉
        let mut doc = format!("model_provider = \"{PROVIDER_ID}\"\n")
            .parse::<DocumentMut>()
            .unwrap();
        let mut auth = Value::Object(Default::default());
        let (config_changed, _) = remove_from_parts(&mut doc, &mut auth, PORT, "k2a-1");
        assert!(config_changed);
        assert!(!doc.to_string().contains("model_provider"));
    }

    #[test]
    fn atomic_write_round_trip() {
        let dir = std::env::temp_dir().join(format!("k2a-codex-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        write_atomic(&path, "a = 1\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "a = 1\n");
        write_atomic(&path, "a = 2\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "a = 2\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
