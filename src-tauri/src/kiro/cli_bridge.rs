//! kiro-cli 凭据桥接：新版 kiro-cli（>= 2.31）不再写 Kiro IDE 的
//! `~/.aws/sso/cache/kiro-auth-token.json`，而是把凭据存进自己的密钥库
//! （macOS 实测：`~/Library/Application Support/kiro-cli/data.sqlite3` 的
//! `auth_kv` 表，另镜像一份到钥匙串）。本模块在规范凭据文件缺失时，
//! 从该库只读取出令牌与 device registration，按 IDE 的文件格式落盘，
//! 之后 [`super::auth::Auth`] 的读取与 IdC/social 刷新链路原样可用。
//!
//! 只在规范文件缺失时触发：本机可能同时存在 IDE 与 CLI 两套不同登录
//! （client_id 不同），桥接绝不覆盖已有的 IDE 凭据。

use std::fs;
use std::path::Path;
use std::sync::Mutex;
use std::time::SystemTime;

use rusqlite::OpenFlags;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::paths::{kiro_cli_db_path, kiro_sso_cache_dir};

/// 令牌记录键：IdC 与 social/外部 IdP 两种登录各存一行
const TOKEN_KEYS: [&str; 2] = ["kirocli:odic:token", "kirocli:external-idp:token"];
const REGISTRATION_KEY: &str = "kirocli:odic:device-registration";

/// 规范凭据文件是否可用；缺失时先尝试桥接一次。
/// 供 `has_credential_file` 等状态判定与启动路径调用，需容忍高频调用：
/// 规范文件存在时只有一次 stat 的开销。
pub(crate) fn ensure_bridged() -> bool {
    let canonical = kiro_sso_cache_dir().join("kiro-auth-token.json");
    if canonical.is_file() {
        return true;
    }
    try_bridge(&canonical);
    canonical.is_file()
}

fn try_bridge(canonical: &Path) {
    let db = kiro_cli_db_path();
    if !db.is_file() || !should_attempt(&db) {
        return;
    }
    if let Err(e) = bridge(canonical, &db) {
        crate::diag::log(&format!("[cli-bridge] kiro-cli 凭据桥接失败：{e}"));
    }
}

/// 桥接失败时按 db 的 mtime 节流：mtime 没变（CLI 没重新登录）就不反复重试，
/// 状态轮询不会每次都去开 sqlite；CLI 一旦重新登录，mtime 变化自动重试。
fn should_attempt(db: &Path) -> bool {
    static LAST: Mutex<Option<SystemTime>> = Mutex::new(None);
    let mtime = fs::metadata(db).and_then(|m| m.modified()).ok();
    let Ok(mut last) = LAST.lock() else {
        return false;
    };
    if last.is_some() && *last == mtime {
        return false;
    }
    *last = mtime;
    true
}

fn bridge(canonical: &Path, db: &Path) -> Result<(), String> {
    let con = rusqlite::Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(|e| format!("打开 {db:?} 失败：{e}"))?;
    let token = read_kv_json(&con, &TOKEN_KEYS)?
        .ok_or_else(|| format!("{db:?} 里没有令牌记录（是否已 kiro-cli login？）"))?;
    let registration = read_kv_json(&con, &[REGISTRATION_KEY])?;
    let (registration_payload, token_payload) = map_credentials(&token, registration.as_ref())?;

    let cache_dir = kiro_sso_cache_dir();
    fs::create_dir_all(&cache_dir).map_err(|e| format!("创建 SSO 缓存目录失败：{e}"))?;
    if let Some((file_name, payload)) = registration_payload {
        write_secret_file(&cache_dir.join(file_name), &payload)?;
    }
    write_secret_file(canonical, &token_payload)?;
    crate::diag::log(&format!("[cli-bridge] 已从 kiro-cli 凭据库桥接出 {}", canonical.display()));
    Ok(())
}

fn read_kv_json(con: &rusqlite::Connection, keys: &[&str]) -> Result<Option<Value>, String> {
    for key in keys {
        let result = con.query_row(
            "select value from auth_kv where key = ?1",
            [key],
            |row| row.get::<_, String>(0),
        );
        match result {
            Ok(raw) => {
                let value = serde_json::from_str(&raw)
                    .map_err(|e| format!("auth_kv[{key}] 不是合法 JSON：{e}"))?;
                return Ok(Some(value));
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => continue,
            Err(e) => return Err(format!("读取 auth_kv[{key}] 失败：{e}")),
        }
    }
    Ok(None)
}

/// auth_kv 记录 → (注册文件 (文件名, 内容), 令牌文件内容)。
/// 字段映射依据 kiro-cli 2.31.1 实测：令牌与注册均为 snake_case JSON，
/// IdC 记录带 start_url，social/外部 IdP 记录没有 device registration。
fn map_credentials(
    token: &Value,
    registration: Option<&Value>,
) -> Result<(Option<(String, Value)>, Value), String> {
    let field = |obj: &Value, key: &str| -> Result<String, String> {
        obj.get(key)
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .map(|v| v.to_string())
            .ok_or_else(|| format!("凭据记录缺少 {key}"))
    };
    let access_token = field(token, "access_token")?;
    let refresh_token = field(token, "refresh_token")?;
    let expires_at = field(token, "expires_at")?;
    let region = token
        .get("region")
        .and_then(Value::as_str)
        .unwrap_or("us-east-1");

    let is_idc = token
        .get("start_url")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty());

    let mut payload = Map::new();
    payload.insert("accessToken".into(), Value::String(access_token));
    payload.insert("refreshToken".into(), Value::String(refresh_token));
    payload.insert("expiresAt".into(), Value::String(expires_at));
    payload.insert("region".into(), Value::String(region.to_string()));

    let registration_payload = if is_idc {
        let reg = registration.ok_or("IdC 登录但缺少 device-registration 记录，无法支撑续期")?;
        let client_id = field(reg, "client_id")?;
        let client_secret = field(reg, "client_secret")?;
        let reg_expires = field(reg, "client_secret_expires_at")?;
        // 缓存文件名沿用 AWS CLI 惯例（client_id 的 sha256），稳定且不会与 IDE 的注册文件撞名
        let hash = format!("{:x}", Sha256::digest(client_id.as_bytes()));
        let file_name = format!("{hash}.json");
        payload.insert("authMethod".into(), Value::String("IdC".into()));
        payload.insert("provider".into(), Value::Null);
        payload.insert("clientIdHash".into(), Value::String(hash));
        (
            Some((
                file_name,
                json!({
                    "clientId": client_id,
                    "clientSecret": client_secret,
                    "expiresAt": reg_expires,
                }),
            )),
            Value::Object(payload),
        )
    } else {
        payload.insert("authMethod".into(), Value::String("social".into()));
        payload.insert("provider".into(), token.get("provider").cloned().unwrap_or(Value::Null));
        (None, Value::Object(payload))
    };
    Ok(registration_payload)
}

/// 0600 + 原子替换，与 auth.rs 写令牌的口径一致
fn write_secret_file(path: &Path, payload: &Value) -> Result<(), String> {
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    {
        let mut file = fs::File::create(&tmp).map_err(|e| format!("创建 {:?} 失败：{e}", tmp))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = file
                .metadata()
                .map(|m| m.permissions())
                .map_err(|e| format!("stat {:?} 失败：{e}", tmp))?;
            perms.set_mode(0o600);
            let _ = file.set_permissions(perms);
        }
        serde_json::to_writer_pretty(&mut file, payload)
            .map_err(|e| format!("序列化凭据失败：{e}"))?;
        file.sync_all().map_err(|e| format!("sync {:?} 失败：{e}", tmp))?;
    }
    fs::rename(&tmp, path).map_err(|e| format!("替换 {:?} 失败：{e}", path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idc_records_map_to_ide_format_with_registration_file() {
        let token = json!({
            "access_token": "AT", "refresh_token": "RT",
            "expires_at": "2026-10-16T15:31:37.245Z",
            "region": "us-east-1",
            "start_url": "https://d-9066710b4d.awsapps.com/start",
            "oauth_flow": "Pkce"
        });
        let reg = json!({
            "client_id": "FAKECLIENTID123", "client_secret": "SECRET",
            "client_secret_expires_at": "2027-01-14T14:16:45Z",
            "region": "us-east-1"
        });
        let (registration, token_payload) = map_credentials(&token, Some(&reg)).unwrap();
        let (file_name, reg_payload) = registration.unwrap();
        assert_eq!(token_payload["authMethod"], "IdC");
        assert_eq!(token_payload["accessToken"], "AT");
        assert_eq!(token_payload["expiresAt"], "2026-10-16T15:31:37.245Z");
        // clientIdHash 与注册文件名严格对应（auth.rs 靠它找回注册文件）
        let expected_hash: String = format!("{:x}", Sha256::digest(b"FAKECLIENTID123"));
        assert_eq!(token_payload["clientIdHash"], expected_hash);
        assert_eq!(file_name, format!("{expected_hash}.json"));
        assert_eq!(reg_payload["clientId"], "FAKECLIENTID123");
        assert_eq!(reg_payload["expiresAt"], "2027-01-14T14:16:45Z");
    }

    #[test]
    fn social_records_skip_registration_file() {
        let token = json!({
            "access_token": "AT", "refresh_token": "RT",
            "expires_at": "2026-10-16T15:31:37.245Z",
            "region": "us-east-1", "provider": "BuilderId"
        });
        let (registration, token_payload) = map_credentials(&token, None).unwrap();
        assert!(registration.is_none());
        assert_eq!(token_payload["authMethod"], "social");
        assert_eq!(token_payload["provider"], "BuilderId");
        assert!(token_payload.get("clientIdHash").is_none());
    }

    #[test]
    fn idc_without_registration_is_rejected() {
        let token = json!({
            "access_token": "AT", "refresh_token": "RT",
            "expires_at": "2026-10-16T15:31:37.245Z",
            "start_url": "https://x.awsapps.com/start"
        });
        assert!(map_credentials(&token, None).is_err());
    }

    #[test]
    fn missing_core_fields_are_rejected() {
        let token = json!({ "access_token": "AT" });
        assert!(map_credentials(&token, None).is_err());
    }

    #[test]
    fn read_kv_json_skips_missing_keys_and_rejects_bad_json() {
        let con = rusqlite::Connection::open_in_memory().unwrap();
        con.execute("create table auth_kv (key text primary key, value text)", [])
            .unwrap();
        con.execute(
            "insert into auth_kv values ('kirocli:odic:token', '{\"access_token\":\"AT\"}')",
            [],
        )
        .unwrap();
        // 第一个键不存在时继续找下一个
        let value = read_kv_json(
            &con,
            &["kirocli:external-idp:token", "kirocli:odic:token"],
        )
        .unwrap()
        .unwrap();
        assert_eq!(value["access_token"], "AT");
        assert!(read_kv_json(&con, &["no-such-key"]).unwrap().is_none());
        con.execute("insert into auth_kv values ('bad', 'not-json')", [])
            .unwrap();
        assert!(read_kv_json(&con, &["bad"]).is_err());
    }
}
