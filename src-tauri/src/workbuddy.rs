//! WorkBuddy 接入管理：把本应用的接入配置写进 WorkBuddy 的自定义模型清单，
//! 并让用户在「三方接入」分区里管理这些条目。
//!
//! 应用原本的原则是**不**改任何外部客户端的配置（见 `kiro/service.rs` 的模块注释，
//! 那条原则仍然覆盖 Claude Code 等其它客户端）；WorkBuddy 是唯一的例外，
//! 成立的前提有三条：由用户在界面上显式发起；只写 WorkBuddy 的自定义模型清单
//! （`~/.workbuddy/models.json`）；除用户明确要求的移除外，对已有条目只增改、从不删除。
//!
//! 管理页只认「指向本应用」的条目：按条目 url 的 **host + 端口** 与代理当前端口比对
//! （用户手写的 `localhost` 与 `127.0.0.1` 指向同一个代理，都算匹配），其余条目一概
//! 不扫描、不展示、不删除 —— 那些是用户配给别的服务的，无权过问。
//!
//! 合并规则（界面文案与此保持一致）：
//! * 条目按 `(url, 模型 id)` 认定「同一个」；
//! * 已存在且 `apiKey` 一致 → 跳过；
//! * 已存在但 `apiKey` 不同 → 只更新 `apiKey` —— WorkBuddy 侧自己的开关字段
//!   （supportsImages 等）是用户在对方界面里调的，导入无权替他决定；
//! * 不存在 → 追加一条标准条目。

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::State;

use crate::kiro::{build_access, current_api_key, KiroProxyState};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbuddyImportSummary {
    pub added: usize,
    pub updated: usize,
    pub unchanged: usize,
}

/// 一条指向本应用的 WorkBuddy 自定义模型。`key_matches` 供界面提示
/// 「重置密钥后条目已过期」，修复动作（同步密钥）走的还是导入那条合并规则。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbuddyEntry {
    pub id: String,
    pub url: String,
    pub key_matches: bool,
}

/// 扫描结果。读文件失败不作为命令错误抛出 —— 界面要在分区里就地说明原因，
/// 而不是每回焦点回到窗口就弹一次 toast。
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WorkbuddyScan {
    pub installed: bool,
    pub entries: Vec<WorkbuddyEntry>,
    pub error: Option<String>,
}

/// WorkBuddy 的自定义模型清单路径。目录不存在视为「WorkBuddy 未安装」——
/// 替一个不存在的应用创建配置目录没有意义，用户装好后再导入即可。
fn models_path() -> Result<PathBuf, String> {
    let dir = dirs::home_dir()
        .ok_or_else(|| "无法定位用户主目录".to_string())?
        .join(".workbuddy");
    if !dir.is_dir() {
        return Err("未找到 WorkBuddy（~/.workbuddy 不存在）。请确认 WorkBuddy 已安装并启动过一次。".to_string());
    }
    Ok(dir.join("models.json"))
}

/// 新条目的固定形态：vendor 与三个能力开关取自现网条目的惯例值。
fn new_entry(model: &str, url: &str, api_key: &str) -> serde_json::Value {
    serde_json::json!({
        "id": model,
        "name": model,
        "vendor": "Custom",
        "url": url,
        "apiKey": api_key,
        "supportsToolCall": true,
        "supportsImages": false,
        "supportsReasoning": false,
        "useCustomProtocol": false,
    })
}

/// 合并的核心（纯函数，方便单元测试）：在 `entries` 上按规则增改，返回摘要。
fn merge_entries(
    entries: &mut Vec<serde_json::Value>,
    models: &[String],
    url: &str,
    api_key: &str,
) -> WorkbuddyImportSummary {
    let mut summary = WorkbuddyImportSummary {
        added: 0,
        updated: 0,
        unchanged: 0,
    };
    for model in models {
        let model = model.trim();
        if model.is_empty() {
            continue;
        }
        let existing = entries.iter_mut().find(|entry| {
            entry.get("url").and_then(|v| v.as_str()).map(str::trim) == Some(url)
                && entry.get("id").and_then(|v| v.as_str()).map(str::trim) == Some(model)
        });
        match existing {
            Some(entry) => {
                if entry.get("apiKey").and_then(|v| v.as_str()).map(str::trim) == Some(api_key) {
                    summary.unchanged += 1;
                } else if let Some(obj) = entry.as_object_mut() {
                    obj.insert(
                        "apiKey".to_string(),
                        serde_json::Value::String(api_key.to_string()),
                    );
                    summary.updated += 1;
                }
            }
            None => {
                entries.push(new_entry(model, url, api_key));
                summary.added += 1;
            }
        }
    }
    summary
}

/// 从条目 url 里拆出 host 与端口。拿不到端口（无端口、非 `scheme://` 形态）视为不匹配。
pub(crate) fn url_origin(url: &str) -> Option<(String, u16)> {
    let rest = url.split("://").nth(1)?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let (host, port) = authority.rsplit_once(':')?;
    let port: u16 = port.trim().parse().ok()?;
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    Some((host.to_ascii_lowercase(), port))
}

/// 条目是否指向本应用：host 是本机回环且端口与代理一致。
/// `localhost` 与 `127.0.0.1` 指向同一个代理，都算；别的地址哪怕端口相同也不是本应用。
pub(crate) fn origin_matches(url: &str, port: u16) -> bool {
    match url_origin(url) {
        Some((host, entry_port)) => {
            entry_port == port && matches!(host.as_str(), "127.0.0.1" | "localhost" | "::1")
        }
        None => false,
    }
}

/// 读入 models.json 的条目数组。文件不存在（WorkBuddy 装了但从未配过自定义模型）
/// 返回空列表；格式坏了返回错误，由调用方决定怎么向用户交代。
fn read_entries(path: &Path) -> Result<Vec<serde_json::Value>, String> {
    match fs::read_to_string(path) {
        Ok(raw) => {
            let parsed: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
                format!("models.json 不是合法 JSON（{e}），请先在 WorkBuddy 里检查该文件")
            })?;
            parsed
                .as_array()
                .cloned()
                .ok_or_else(|| "models.json 顶层不是数组，无法安全操作".to_string())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(err) => Err(format!("读取 models.json 失败: {err}")),
    }
}

/// 没有增改就不写盘：避免无谓地碰用户的文件（也避免与 WorkBuddy 自身的写入竞争）。
fn import_to_file(
    models: &[String],
    url: &str,
    api_key: &str,
) -> Result<WorkbuddyImportSummary, String> {
    let path = models_path()?;
    let mut entries = read_entries(&path)?;
    let summary = merge_entries(&mut entries, models, url, api_key);
    if summary.added + summary.updated > 0 {
        write_atomic(&path, &entries)?;
    }
    Ok(summary)
}

/// 先写临时文件再改名：WorkBuddy 读到半个文件比读到旧值糟糕得多。
/// 改名失败（目标被 WorkBuddy 占用）时退回直接覆盖——内容写入是幂等的。
fn write_atomic(path: &Path, entries: &[serde_json::Value]) -> Result<(), String> {
    let body = serde_json::to_string(entries).map_err(|e| format!("序列化失败: {e}"))?;
    let tmp = path.with_extension("json.k2a-tmp");
    fs::write(&tmp, body.as_bytes()).map_err(|e| format!("写入临时文件失败: {e}"))?;
    if fs::rename(&tmp, path).is_ok() {
        return Ok(());
    }
    let result = fs::write(path, body.as_bytes()).map_err(|e| format!("写入 models.json 失败: {e}"));
    let _ = fs::remove_file(&tmp);
    result
}

/// 把选中的模型导入 WorkBuddy。地址与密钥都取自后端当前配置，
/// 前端只传模型 id 列表 —— 密钥不经过前端，与「复制」同一条纪律。
#[tauri::command]
pub async fn k2a_workbuddy_import(
    state: State<'_, KiroProxyState>,
    models: Vec<String>,
) -> Result<WorkbuddyImportSummary, String> {
    let selected: Vec<String> = models
        .into_iter()
        .map(|model| model.trim().to_string())
        .filter(|model| !model.is_empty())
        .collect();
    if selected.is_empty() {
        return Err("未选择任何模型".to_string());
    }
    // 地址由配置端口决定（见 `build_access`），停止状态同样有值 —— 导入与代理
    // 是否在跑无关，写的是 WorkBuddy 自己的模型清单。
    let access = build_access(&state);
    let api_key = current_api_key(&state)
        .ok_or_else(|| "还没有代理密钥，请先启动一次代理生成密钥".to_string())?;
    let url = access.openai_base_url;
    tauri::async_runtime::spawn_blocking(move || import_to_file(&selected, &url, &api_key))
        .await
        .map_err(|e| format!("导入任务失败: {e}"))?
}

/// 扫描 WorkBuddy 的自定义模型清单，只返回指向本应用（host+端口匹配）的条目。
#[tauri::command]
pub async fn k2a_workbuddy_list(state: State<'_, KiroProxyState>) -> Result<WorkbuddyScan, String> {
    let access = build_access(&state);
    let port = access.port;
    let api_key = current_api_key(&state);
    tauri::async_runtime::spawn_blocking(move || {
        Ok(match models_path() {
            Ok(path) => scan_entries_at(&path, port, api_key.as_deref()),
            // 目录不存在视为「WorkBuddy 未安装」，不算错误 —— 界面按未安装引导。
            Err(_) => WorkbuddyScan::default(),
        })
    })
    .await
    .map_err(|e| format!("扫描任务失败: {e}"))?
}

fn scan_entries_at(path: &Path, port: Option<u16>, api_key: Option<&str>) -> WorkbuddyScan {
    let Some(port) = port else {
        return WorkbuddyScan {
            error: Some("无法确定代理端口，无法判断哪些条目指向本应用".to_string()),
            ..WorkbuddyScan::default()
        };
    };
    // 文件不存在 = WorkBuddy 装了但还没写过清单，是正常的空结果；
    // 其余读盘失败作为 error 字段就地呈现，不抛命令错误（避免焦点刷新反复弹 toast）。
    match read_entries(path) {
        Ok(items) => {
            let entries = items
                .iter()
                .filter_map(|entry| {
                    let id = entry.get("id").and_then(|v| v.as_str())?.trim();
                    let url = entry.get("url").and_then(|v| v.as_str())?.trim();
                    if id.is_empty() || !origin_matches(url, port) {
                        return None;
                    }
                    let entry_key = entry.get("apiKey").and_then(|v| v.as_str()).map(str::trim);
                    let key_matches =
                        matches!((api_key, entry_key), (Some(a), Some(b)) if a.trim() == b);
                    Some(WorkbuddyEntry {
                        id: id.to_string(),
                        url: url.to_string(),
                        key_matches,
                    })
                })
                .collect();
            WorkbuddyScan {
                installed: true,
                entries,
                error: None,
            }
        }
        Err(error) => WorkbuddyScan {
            installed: true,
            error: Some(error),
            ..WorkbuddyScan::default()
        },
    }
}

/// 从 WorkBuddy 清单里移除一个指向本应用的模型（按模型 id + host+端口匹配）。
/// 返回移除条数 —— 指向其它服务、或 id 同名但地址不同的条目一概不动。
#[tauri::command]
pub async fn k2a_workbuddy_remove(
    state: State<'_, KiroProxyState>,
    model: String,
) -> Result<usize, String> {
    let model = model.trim().to_string();
    if model.is_empty() {
        return Err("未指定要移除的模型".to_string());
    }
    let port = build_access(&state)
        .port
        .ok_or_else(|| "无法确定代理端口，无法判断哪些条目指向本应用".to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        let path = models_path()?;
        remove_entries_at(&path, &model, port)
    })
    .await
    .map_err(|e| format!("移除任务失败: {e}"))?
}

fn remove_entries_at(path: &Path, model: &str, port: u16) -> Result<usize, String> {
    let mut entries = read_entries(path)?;
    let before = entries.len();
    entries.retain(|entry| {
        let id = entry.get("id").and_then(|v| v.as_str()).map(str::trim);
        let url = entry.get("url").and_then(|v| v.as_str());
        !(id == Some(model) && url.is_some_and(|u| origin_matches(u, port)))
    });
    let removed = before - entries.len();
    if removed > 0 {
        write_atomic(path, &entries)?;
    }
    Ok(removed)
}

/// 清空 WorkBuddy 清单里**指向本应用**的全部条目（host+端口匹配），返回移除条数。
/// 条目本身捆绑了地址与密钥，条目清掉即端点与模型的痕迹一并消失；
/// 指向其它服务的条目一概不动 —— 清空只清本应用自己的痕迹。
#[tauri::command]
pub async fn k2a_workbuddy_clear(state: State<'_, KiroProxyState>) -> Result<usize, String> {
    let port = build_access(&state)
        .port
        .ok_or_else(|| "无法确定代理端口，无法判断哪些条目指向本应用".to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        let path = models_path()?;
        clear_entries_at(&path, port)
    })
    .await
    .map_err(|e| format!("清空任务失败: {e}"))?
}

fn clear_entries_at(path: &Path, port: u16) -> Result<usize, String> {
    let mut entries = read_entries(path)?;
    let before = entries.len();
    // 没有 url 的条目无从判定归属，保守起见保留
    entries.retain(|entry| {
        entry
            .get("url")
            .and_then(|v| v.as_str())
            .map(|url| !origin_matches(url, port))
            .unwrap_or(true)
    });
    let removed = before - entries.len();
    if removed > 0 {
        write_atomic(path, &entries)?;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, url: &str, api_key: &str) -> serde_json::Value {
        new_entry(id, url, api_key)
    }

    #[test]
    fn adds_missing_entries() {
        let mut entries = vec![entry("other", "http://x/v1", "k1")];
        let summary = merge_entries(&mut entries, &["m1".into()], "http://127.0.0.1:5680/openai/v1", "k2a-1");
        assert_eq!((summary.added, summary.updated, summary.unchanged), (1, 0, 0));
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1]["id"], "m1");
        assert_eq!(entries[1]["vendor"], "Custom");
    }

    #[test]
    fn skips_identical_entry() {
        let mut entries = vec![entry("m1", "http://127.0.0.1:5680/openai/v1", "k2a-1")];
        let summary = merge_entries(&mut entries, &["m1".into()], "http://127.0.0.1:5680/openai/v1", "k2a-1");
        assert_eq!((summary.added, summary.updated, summary.unchanged), (0, 0, 1));
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn updates_only_api_key_and_keeps_user_fields() {
        let mut stale = entry("m1", "http://127.0.0.1:5680/openai/v1", "old-key");
        // 用户在 WorkBuddy 里自己开过的开关，导入不得动它
        stale["supportsImages"] = serde_json::Value::Bool(true);
        let mut entries = vec![stale];
        let summary = merge_entries(&mut entries, &["m1".into()], "http://127.0.0.1:5680/openai/v1", "k2a-2");
        assert_eq!((summary.added, summary.updated, summary.unchanged), (0, 1, 0));
        assert_eq!(entries[0]["apiKey"], "k2a-2");
        assert_eq!(entries[0]["supportsImages"], true);
    }

    #[test]
    fn same_model_different_url_adds_separate_entry() {
        let mut entries = vec![entry("m1", "http://other-host/v1", "k1")];
        let summary = merge_entries(&mut entries, &["m1".into()], "http://127.0.0.1:5680/openai/v1", "k2a-1");
        assert_eq!((summary.added, summary.updated, summary.unchanged), (1, 0, 0));
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn blank_models_are_ignored() {
        let mut entries = Vec::new();
        let summary = merge_entries(&mut entries, &["  ".to_string(), String::new()], "http://x/v1", "k1");
        assert_eq!((summary.added, summary.updated, summary.unchanged), (0, 0, 0));
        assert!(entries.is_empty());
    }

    #[test]
    fn import_into_missing_file_starts_from_empty() {
        let dir = std::env::temp_dir().join(format!("k2a-wb-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("models.json");
        let _ = std::fs::remove_file(&path);
        let mut entries: Vec<serde_json::Value> = match fs::read_to_string(&path) {
            Ok(raw) => serde_json::from_str(&raw).unwrap(),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(err) => panic!("{err}"),
        };
        let summary = merge_entries(&mut entries, &["m1".into()], "http://127.0.0.1:5680/openai/v1", "k");
        assert_eq!(summary.added, 1);
        write_atomic(&path, &entries).unwrap();
        let round: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(round[0]["id"], "m1");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn origin_parses_host_and_port() {
        assert_eq!(
            url_origin("http://127.0.0.1:5680/openai/v1"),
            Some(("127.0.0.1".into(), 5680))
        );
        assert_eq!(url_origin("http://127.0.0.1/openai/v1"), None);
        assert_eq!(url_origin("127.0.0.1:5680"), None);
        assert_eq!(
            url_origin("http://[::1]:5680/v1"),
            Some(("::1".into(), 5680))
        );
    }

    #[test]
    fn origin_matches_only_loopback_with_same_port() {
        assert!(origin_matches("http://127.0.0.1:5680/openai/v1", 5680));
        assert!(origin_matches("http://localhost:5680", 5680));
        assert!(!origin_matches("http://127.0.0.1:5680/openai/v1", 5679));
        assert!(!origin_matches("http://10.0.0.2:5680/openai/v1", 5680));
        assert!(!origin_matches("http://127.0.0.1/openai/v1", 5680));
    }

    fn scan_fixture(path: &Path) {
        let body = serde_json::json!([
            { "id": "m1", "url": "http://127.0.0.1:5680/openai/v1", "apiKey": "k2a-1" },
            { "id": "stale", "url": "http://127.0.0.1:5680/openai/v1", "apiKey": "old" },
            { "id": "other-host", "url": "http://10.0.0.2:5680/v1", "apiKey": "k2a-1" },
            { "id": "other-port", "url": "http://127.0.0.1:9999/v1", "apiKey": "k2a-1" },
        ]);
        fs::write(path, body.to_string()).unwrap();
    }

    #[test]
    fn scan_returns_only_matching_entries() {
        let dir = std::env::temp_dir().join(format!("k2a-wb-scan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("models.json");
        scan_fixture(&path);

        let scan = scan_entries_at(&path, Some(5680), Some("k2a-1"));
        assert!(scan.installed);
        assert!(scan.error.is_none());
        let ids: Vec<&str> = scan.entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["m1", "stale"]);
        assert!(scan.entries[0].key_matches);
        assert!(!scan.entries[1].key_matches);

        let empty = scan_entries_at(&path, Some(5680), None);
        assert_eq!(empty.entries.len(), 2);
        assert!(empty.entries.iter().all(|e| !e.key_matches));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn scan_reports_broken_file_as_error_field() {
        let dir = std::env::temp_dir().join(format!("k2a-wb-broken-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("models.json");
        fs::write(&path, "not json").unwrap();
        let scan = scan_entries_at(&path, Some(5680), None);
        assert!(scan.installed);
        assert!(scan.entries.is_empty());
        assert!(scan.error.is_some());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn remove_only_touches_matching_entries() {
        let dir = std::env::temp_dir().join(format!("k2a-wb-rm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("models.json");
        scan_fixture(&path);

        let removed = remove_entries_at(&path, "m1", 5680).unwrap();
        assert_eq!(removed, 1);
        let round: Vec<serde_json::Value> = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let ids: Vec<&str> = round.iter().filter_map(|e| e["id"].as_str()).collect();
        assert_eq!(ids, vec!["stale", "other-host", "other-port"]);

        // 指向其它服务的同名模型不动
        assert_eq!(remove_entries_at(&path, "other-host", 5680).unwrap(), 0);
        // 没有匹配项时不写盘，也不报错
        assert_eq!(remove_entries_at(&path, "nope", 5680).unwrap(), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn clear_removes_only_matching_entries() {
        let dir = std::env::temp_dir().join(format!("k2a-wb-clear-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("models.json");
        scan_fixture(&path);

        // m1 与 stale 指向本应用，other-host / other-port 是别家的
        let removed = clear_entries_at(&path, 5680).unwrap();
        assert_eq!(removed, 2);
        let round: Vec<serde_json::Value> =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let ids: Vec<&str> = round.iter().filter_map(|e| e["id"].as_str()).collect();
        assert_eq!(ids, vec!["other-host", "other-port"]);

        // 再清一次无事发生：不写盘也不报错
        assert_eq!(clear_entries_at(&path, 5680).unwrap(), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
