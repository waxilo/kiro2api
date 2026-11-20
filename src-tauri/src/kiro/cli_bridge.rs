//! kiro-cli 凭据读取：新版 kiro-cli（>= 2.31）不再写 Kiro IDE 的
//! `~/.aws/sso/cache/kiro-auth-token.json`，而是把凭据存进自己的密钥库
//! （`data.sqlite3` 的 `auth_kv` 表，位置按平台不同 —— macOS 在
//! `~/Library/Application Support/kiro-cli/`，Windows 在 `%LOCALAPPDATA%\Kiro-Cli\`，
//! 见 [`crate::paths::kiro_cli_db_paths`]；另镜像一份到钥匙串）。
//!
//! 本模块只做两件事：
//! 1. [`scan`]：以只读方式打开库，把令牌与 device registration 翻译成 IDE 的文件格式，
//!    供账号列表展示与判断可用性；
//! 2. [`import`]：把某一条记录写进一个**独立目录**（`数据目录/accounts/cli/<hash>/`），
//!    之后 [`super::auth::Auth`] 的读取与 IdC/social 刷新链路原样可用。
//!
//! 为什么是「导入副本」而不是「就地接管」：
//! - CLI 的库是它自己的资产，写回去要同时维护 sqlite 与钥匙串镜像，风险远大于收益；
//! - 以前的做法是把 CLI 凭据桥接进 `~/.aws/sso/cache/kiro-auth-token.json`，
//!   那等于覆盖 IDE 的登录 —— 两边各自刷新同一个 refreshToken 会互相顶掉。
//! 副本的代价是：本应用续期后，CLI 侧那份 refreshToken 会失效、需要重新登录；
//! 所以界面在 CLI 分组上如实写着这件事。CLI 一旦重新登录（库 mtime 变化），
//! 账号模块会重新导入一次，副本自动跟上。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use rusqlite::OpenFlags;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::paths::kiro_cli_db_paths;

/// 令牌记录键：IdC 与 social/外部 IdP 两种登录各存一行。
///
/// CLI 同时只保留一种登录方式（后登录的覆盖前一种），所以按这个顺序取**第一条**
/// 就是它当前在用的账号；两条都列出来只会把同一个账号显示两次。
const TOKEN_KEYS: [&str; 2] = ["kirocli:odic:token", "kirocli:external-idp:token"];
const REGISTRATION_KEY: &str = "kirocli:odic:device-registration";

/// 从 CLI 凭据库里读到的一条账号记录（已翻译成 IDE 的文件格式）。
#[derive(Debug, Clone)]
pub(crate) struct CliCredential {
    pub db: PathBuf,
    /// 来源键（`kirocli:odic:token` / `kirocli:external-idp:token`），用于诊断。
    pub key: String,
    /// 库文件的 mtime（Unix 毫秒）。副本据此判断「CLI 是否重新登录过」。
    pub db_mtime_ms: i64,
    /// IDE 格式令牌（含 `authMethod` / `clientIdHash`）。
    pub token: Value,
    /// 注册文件（文件名, 内容）；social 登录没有这一项。
    pub registration: Option<(String, Value)>,
}

/// 扫描本机所有候选凭据库，返回其中能读出的账号（通常 0 或 1 条）。
pub(crate) fn scan() -> Vec<CliCredential> {
    let mut found = Vec::new();
    for db in kiro_cli_db_paths() {
        match read_one(&db, None) {
            Ok(Some(cred)) => found.push(cred),
            Ok(None) => {}
            Err(error) => crate::diag::log(&format!(
                "[cli-bridge] 读取 {} 失败：{error}",
                db.display()
            )),
        }
    }
    found
}

/// 从指定库里读一条记录：`key` 给了就读那一行，否则按 [`TOKEN_KEYS`] 取当前登录方式。
///
/// 账号模块重建副本时用的是这个精确入口，而不是 `scan()` —— 用户可能有多个候选库，
/// 不能拿「本机第一个库」去覆盖另一个库导出的账号。
pub(crate) fn read_one(db: &Path, key: Option<&str>) -> Result<Option<CliCredential>, String> {
    if !db.is_file() {
        return Ok(None);
    }
    let con = rusqlite::Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("打开 {} 失败：{e}", db.display()))?;
    let (key, token) = match key {
        Some(key) => match read_kv_raw(&con, key)? {
            Some(value) => (key.to_string(), value),
            None => return Ok(None),
        },
        None => match read_kv_first(&con, &TOKEN_KEYS)? {
            Some((key, value)) => (key.to_string(), value),
            None => return Ok(None),
        },
    };
    let registration = read_kv_json(&con, &[REGISTRATION_KEY])?;
    let (registration_payload, token_payload) = map_credentials(&token, registration.as_ref())?;
    Ok(Some(CliCredential {
        db: db.to_path_buf(),
        key,
        db_mtime_ms: file_mtime_ms(db),
        token: token_payload,
        registration: registration_payload,
    }))
}

/// 把一条记录写进 `dir`（创建目录、覆盖旧副本），供 `Auth` 直接读取。
pub(crate) fn import(cred: &CliCredential, dir: &Path) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| format!("创建账号目录失败：{e}"))?;
    if let Some((file_name, payload)) = &cred.registration {
        write_secret_file(&dir.join(file_name), payload)?;
    }
    write_secret_file(&dir.join("kiro-auth-token.json"), &cred.token)
}

fn file_mtime_ms(path: &Path) -> i64 {
    fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

fn read_kv_first(
    con: &rusqlite::Connection,
    keys: &'static [&'static str],
) -> Result<Option<(&'static str, Value)>, String> {
    for key in keys {
        if let Some(value) = read_kv_raw(con, key)? {
            return Ok(Some((key, value)));
        }
    }
    Ok(None)
}

fn read_kv_raw(con: &rusqlite::Connection, key: &str) -> Result<Option<Value>, String> {
    let result = con.query_row(
        "select value from auth_kv where key = ?1",
        [key],
        |row| row.get::<_, String>(0),
    );
    match result {
        Ok(raw) => serde_json::from_str(&raw)
            .map(Some)
            .map_err(|e| format!("auth_kv[{key}] 不是合法 JSON：{e}")),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(format!("读取 auth_kv[{key}] 失败：{e}")),
    }
}

fn read_kv_json(con: &rusqlite::Connection, keys: &[&str]) -> Result<Option<Value>, String> {
    for key in keys {
        if let Some(value) = read_kv_raw(con, key)? {
            return Ok(Some(value));
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

    let (registration_payload, token_payload) = if is_idc {
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
        payload.insert(
            "startUrl".into(),
            token.get("start_url").cloned().unwrap_or(Value::Null),
        );
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
    Ok((registration_payload, token_payload))
}

/// 0600 + 原子替换，与 auth.rs 写令牌的口径一致
pub(crate) fn write_secret_file(path: &Path, payload: &Value) -> Result<(), String> {
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    {
        let mut file = fs::File::create(&tmp).map_err(|e| format!("创建 {:?} 失败：{e}", tmp))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = file
                .metadata()
                .map_err(|m| format!("stat {:?} 失败：{m}", tmp))?
                .permissions();
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
        assert_eq!(token_payload["startUrl"], "https://d-9066710b4d.awsapps.com/start");
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
    fn read_kv_skips_missing_keys_and_rejects_bad_json() {
        let con = rusqlite::Connection::open_in_memory().unwrap();
        con.execute("create table auth_kv (key text primary key, value text)", [])
            .unwrap();
        con.execute(
            "insert into auth_kv values ('kirocli:odic:token', '{\"access_token\":\"AT\"}')",
            [],
        )
        .unwrap();
        // 第一个键不存在时继续找下一个
        let (key, value) = read_kv_first(&con, &["kirocli:external-idp:token", "kirocli:odic:token"])
            .unwrap()
            .unwrap();
        assert_eq!(key, "kirocli:odic:token");
        assert_eq!(value["access_token"], "AT");
        assert!(read_kv_first(&con, &["no-such-key"]).unwrap().is_none());
        assert!(read_kv_json(&con, &["no-such-key"]).unwrap().is_none());
        con.execute("insert into auth_kv values ('bad', 'not-json')", [])
            .unwrap();
        assert!(read_kv_json(&con, &["bad"]).is_err());
    }

    /// 对不存在的库返回空，而不是报错 —— 未装 kiro-cli 是常态。
    #[test]
    fn scan_ignores_missing_database() {
        // 直接用不存在的路径走 read_one：不能触碰真实 %LOCALAPPDATA%
        let missing = std::env::temp_dir().join("k2a-no-such-cli-db.sqlite3");
        assert!(read_one(&missing, None).unwrap().is_none());
    }
}
