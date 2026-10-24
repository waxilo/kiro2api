//! kiro2api 自己发起的 Kiro 登录：AWS SSO OIDC 设备码流程（Builder ID / 企业 IdC）。
//!
//! 流程与 kiro-cli 相同，都是 AWS 公开协议：
//! 1. `POST /client/register` 注册一个 public client，拿 clientId / clientSecret（约 90 天有效）；
//! 2. `POST /device_authorization` 拿用户验证码与验证页地址，打开浏览器让用户确认；
//! 3. 后台按 `interval` 轮询 `POST /token`（device_code 授权），用户确认后拿到令牌。
//!
//! 结果写进 [`crate::paths::own_credential_dir`]，文件格式与 `~/.aws/sso/cache` 一致：
//! 令牌文件 `kiro-auth-token.json`（`authMethod = IdC` + `clientIdHash`）与注册文件
//! `<clientIdHash>.json`。之后的读取、到期刷新（`Auth::refresh_idc`）、额度与模型都不用改。
//! 不写共享缓存：Kiro IDE 和这里各自刷新同一个 refreshToken 会把对方顶掉。

use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use chrono::{SecondsFormat, TimeZone, Utc};
use reqwest::blocking::Client;
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_opener::OpenerExt;

use crate::paths::own_credential_dir;

/// Builder ID 的固定入口。
const BUILDER_ID_START_URL: &str = "https://view.awsapps.com/start";
const BUILDER_ID_REGION: &str = "us-east-1";
/// Builder ID 账户没有可发现的 profile，Kiro IDE 对它们统一用这个 ARN。
const BUILDER_ID_PROFILE_ARN: &str =
    "arn:aws:codewhisperer:us-east-1:699475941385:profile/EHGA3GRVQMUK";
/// 与 Kiro / kiro-cli 申请的权限一致。
const SCOPES: [&str; 5] = [
    "codewhisperer:completions",
    "codewhisperer:analysis",
    "codewhisperer:conversations",
    "codewhisperer:transformations",
    "codewhisperer:taskassist",
];
const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
const TOKEN_FILE: &str = "kiro-auth-token.json";

/// 登录代际：开始新登录或取消时加一，旧的轮询线程看到代际变了就退出。
static GENERATION: AtomicU64 = AtomicU64::new(0);
/// 当前等待中的验证页地址（「重新打开浏览器」用）。
static PENDING_URL: Mutex<Option<String>> = Mutex::new(None);

/// 返回给前端的设备码会话。
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct LoginSession {
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    /// 验证码失效时刻（UTC，RFC3339）。
    pub expires_at: String,
    /// 浏览器是否已经打开（失败时界面提示用户手动打开）。
    pub browser_opened: bool,
}

struct Registration {
    client_id: String,
    client_secret: String,
    expires_at: String,
}

struct DeviceGrant {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: String,
    expires_in: i64,
    interval: u64,
}

struct LoginTarget {
    start_url: String,
    region: String,
    /// `BuilderId` / `Enterprise`，写进令牌文件的 `provider`。
    provider: &'static str,
}

fn http() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap_or_else(|_| Client::new())
}

fn oidc_url(region: &str, path: &str) -> String {
    format!("https://oidc.{region}.amazonaws.com/{path}")
}

/// 发一次 OIDC 请求，返回 (HTTP 状态是否成功, JSON 体)。
fn post_json(client: &Client, url: &str, body: &Value) -> Result<(bool, Value), String> {
    let response = client
        .post(url)
        .header("accept", "application/json")
        .header("content-type", "application/json")
        .json(body)
        .send()
        .map_err(|e| format!("请求 {url} 失败：{e}"))?;
    let ok = response.status().is_success();
    let status = response.status();
    let text = response.text().unwrap_or_default();
    let data = if text.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(&text).unwrap_or_else(|_| json!({ "message": text }))
    };
    if !ok && data.get("error").is_none() && data.get("message").is_none() {
        return Ok((false, json!({ "message": format!("HTTP {status}") })));
    }
    Ok((ok, data))
}

fn error_text(data: &Value) -> String {
    let code = data.get("error").and_then(Value::as_str);
    let detail = data
        .get("error_description")
        .or_else(|| data.get("message"))
        .and_then(Value::as_str);
    match (code, detail) {
        (Some(c), Some(d)) => format!("{c}：{d}"),
        (Some(c), None) => c.to_string(),
        (None, Some(d)) => d.to_string(),
        (None, None) => "未知错误".to_string(),
    }
}

fn str_field(data: &Value, key: &str) -> Result<String, String> {
    data.get(key)
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("上游响应缺少 {key}"))
}

fn register_client(client: &Client, target: &LoginTarget) -> Result<Registration, String> {
    let (ok, data) = post_json(
        client,
        &oidc_url(&target.region, "client/register"),
        &json!({
            "clientName": "Kiro2API",
            "clientType": "public",
            "scopes": SCOPES,
        }),
    )?;
    if !ok {
        return Err(format!("注册登录客户端失败：{}", error_text(&data)));
    }
    let expires_at = data
        .get("clientSecretExpiresAt")
        .and_then(Value::as_i64)
        .and_then(|secs| Utc.timestamp_opt(secs, 0).single())
        .unwrap_or_else(|| Utc::now() + chrono::Duration::days(90))
        .to_rfc3339_opts(SecondsFormat::Secs, true);
    Ok(Registration {
        client_id: str_field(&data, "clientId")?,
        client_secret: str_field(&data, "clientSecret")?,
        expires_at,
    })
}

fn start_device_authorization(
    client: &Client,
    target: &LoginTarget,
    registration: &Registration,
) -> Result<DeviceGrant, String> {
    let (ok, data) = post_json(
        client,
        &oidc_url(&target.region, "device_authorization"),
        &json!({
            "clientId": registration.client_id,
            "clientSecret": registration.client_secret,
            "startUrl": target.start_url,
        }),
    )?;
    if !ok {
        return Err(format!("发起设备授权失败：{}", error_text(&data)));
    }
    let verification_uri = str_field(&data, "verificationUri")?;
    Ok(DeviceGrant {
        device_code: str_field(&data, "deviceCode")?,
        user_code: str_field(&data, "userCode")?,
        verification_uri_complete: data
            .get("verificationUriComplete")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| verification_uri.clone()),
        verification_uri,
        expires_in: data.get("expiresIn").and_then(Value::as_i64).unwrap_or(600),
        interval: data.get("interval").and_then(Value::as_u64).unwrap_or(5).max(1),
    })
}

/// 轮询结果：拿到令牌 / 失败 / 被新登录或取消替代。
enum PollOutcome {
    Token(Value),
    Failed(String),
    Superseded,
}

fn poll_token(
    client: &Client,
    target: &LoginTarget,
    registration: &Registration,
    grant: &DeviceGrant,
    generation: u64,
) -> PollOutcome {
    let deadline = Instant::now() + Duration::from_secs(grant.expires_in.max(60) as u64);
    let mut interval = grant.interval;
    let url = oidc_url(&target.region, "token");
    loop {
        // 按秒睡，取消能在 1 秒内生效
        for _ in 0..interval {
            if GENERATION.load(Ordering::SeqCst) != generation {
                return PollOutcome::Superseded;
            }
            thread::sleep(Duration::from_secs(1));
        }
        if GENERATION.load(Ordering::SeqCst) != generation {
            return PollOutcome::Superseded;
        }
        if Instant::now() >= deadline {
            return PollOutcome::Failed("验证码已过期，请重新发起登录".to_string());
        }
        let result = post_json(
            client,
            &url,
            &json!({
                "clientId": registration.client_id,
                "clientSecret": registration.client_secret,
                "grantType": DEVICE_GRANT,
                "deviceCode": grant.device_code,
            }),
        );
        // 网络抖动不算失败：下一轮接着问，直到验证码过期
        let Ok((ok, data)) = result else { continue };
        if ok {
            return PollOutcome::Token(data);
        }
        match data.get("error").and_then(Value::as_str) {
            Some("authorization_pending") => {}
            Some("slow_down") => interval += 5,
            Some("expired_token") => {
                return PollOutcome::Failed("验证码已过期，请重新发起登录".to_string())
            }
            Some("access_denied") => {
                return PollOutcome::Failed("浏览器里拒绝了授权".to_string())
            }
            _ => return PollOutcome::Failed(format!("登录失败：{}", error_text(&data))),
        }
    }
}

/// 令牌响应 → 自有凭据目录里的两份文件。
fn store_credentials(
    target: &LoginTarget,
    registration: &Registration,
    response: &Value,
) -> Result<(), String> {
    let access_token = str_field(response, "accessToken")?;
    let refresh_token = str_field(response, "refreshToken")?;
    let expires_in = response.get("expiresIn").and_then(Value::as_i64).unwrap_or(3600);
    let expires_at = (Utc::now() + chrono::Duration::seconds(expires_in))
        .to_rfc3339_opts(SecondsFormat::Millis, true);
    let hash = format!("{:x}", Sha256::digest(registration.client_id.as_bytes()));

    let mut token = json!({
        "accessToken": access_token,
        "refreshToken": refresh_token,
        "expiresAt": expires_at,
        "region": target.region,
        "startUrl": target.start_url,
        "authMethod": "IdC",
        "provider": target.provider,
        "clientIdHash": hash,
    });
    if target.provider == "BuilderId" {
        token["profileArn"] = Value::String(BUILDER_ID_PROFILE_ARN.to_string());
    }

    let dir = own_credential_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("创建凭据目录失败：{e}"))?;
    // 换账号登录时，上一份注册文件就没人引用了，顺手清掉
    remove_registration_of_current_token();
    super::cli_bridge::write_secret_file(
        &dir.join(format!("{hash}.json")),
        &json!({
            "clientId": registration.client_id,
            "clientSecret": registration.client_secret,
            "expiresAt": registration.expires_at,
        }),
    )?;
    super::cli_bridge::write_secret_file(&dir.join(TOKEN_FILE), &token)
}

/// 删掉当前自有令牌引用的注册文件（只动自有凭据目录）。
fn remove_registration_of_current_token() {
    let dir = own_credential_dir();
    let hash = fs::read_to_string(dir.join(TOKEN_FILE))
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|token| token.get("clientIdHash").and_then(Value::as_str).map(str::to_string));
    if let Some(hash) = hash.filter(|h| h.chars().all(|c| c.is_ascii_hexdigit())) {
        let _ = fs::remove_file(dir.join(format!("{hash}.json")));
    }
}

fn resolve_target(method: &str, start_url: Option<String>, region: Option<String>) -> Result<LoginTarget, String> {
    match method {
        "builder" => Ok(LoginTarget {
            start_url: BUILDER_ID_START_URL.to_string(),
            region: BUILDER_ID_REGION.to_string(),
            provider: "BuilderId",
        }),
        "idc" => {
            let start_url = start_url.unwrap_or_default().trim().trim_end_matches('#').to_string();
            if !start_url.starts_with("https://") || start_url.len() <= "https://".len() {
                return Err("请填写以 https:// 开头的 Start URL（如 https://d-xxxxxxxxxx.awsapps.com/start）".to_string());
            }
            let region = region
                .map(|r| r.trim().to_ascii_lowercase())
                .filter(|r| !r.is_empty())
                .unwrap_or_else(|| BUILDER_ID_REGION.to_string());
            // 区域会拼进主机名，只放行形如 us-east-1 的值
            let valid = regex::Regex::new(r"^[a-z]{2}(-[a-z]+)+-\d$")
                .map(|re| re.is_match(&region))
                .unwrap_or(false);
            if !valid {
                return Err(format!("区域格式不对：{region}（应形如 us-east-1）"));
            }
            Ok(LoginTarget { start_url, region, provider: "Enterprise" })
        }
        other => Err(format!("未知的登录方式：{other}")),
    }
}

/// 凭据目录变了（登录 / 退出），正在运行的代理要换成新的 Auth：按原端口重启一次。
fn restart_proxy_if_running(app: &AppHandle) {
    let state = app.state::<super::service::KiroProxyState>();
    let state = (*state).clone();
    let Ok(_op) = state.op_lock.lock() else { return };
    if !super::service::proxy_running(&state) {
        return;
    }
    let port = state.port.lock().ok().and_then(|p| *p);
    if let Err(e) = super::service::start_proxy(&state, port) {
        crate::diag::log(&format!("[login] 凭据切换后重启代理失败：{e}"));
    }
}

fn finish_success(app: &AppHandle) {
    restart_proxy_if_running(app);
    super::service::spawn_startup_refresh();
    let state = app.state::<super::service::KiroProxyState>();
    let status = super::service::build_status(&state);
    let _ = app.emit("login://done", &status);
}

// ── Tauri 命令 ──────────────────────────────────────────────────────────

/// 发起设备码登录：注册 client、拿验证码、打开浏览器，然后在后台轮询。
/// 结果通过事件 `login://done`（载荷 ProxyStatus）/ `login://failed`（错误文本）送达。
#[tauri::command]
pub async fn k2a_login_start(
    app: AppHandle,
    method: String,
    start_url: Option<String>,
    region: Option<String>,
) -> Result<LoginSession, String> {
    let target = resolve_target(&method, start_url, region)?;
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let (registration, grant) = tauri::async_runtime::spawn_blocking({
        let start_url = target.start_url.clone();
        let region = target.region.clone();
        let provider = target.provider;
        move || {
            let target = LoginTarget { start_url, region, provider };
            let client = http();
            let registration = register_client(&client, &target)?;
            let grant = start_device_authorization(&client, &target, &registration)?;
            Ok::<_, String>((registration, grant))
        }
    })
    .await
    .map_err(|e| format!("登录任务失败：{e}"))??;

    if GENERATION.load(Ordering::SeqCst) != generation {
        return Err("登录已取消".to_string());
    }
    if let Ok(mut url) = PENDING_URL.lock() {
        *url = Some(grant.verification_uri_complete.clone());
    }
    let browser_opened = app
        .opener()
        .open_url(grant.verification_uri_complete.clone(), None::<&str>)
        .is_ok();
    crate::diag::log(&format!(
        "[login] 设备码登录已发起（{}，{}），等待浏览器确认",
        target.provider, target.region
    ));

    let session = LoginSession {
        user_code: grant.user_code.clone(),
        verification_uri: grant.verification_uri.clone(),
        verification_uri_complete: grant.verification_uri_complete.clone(),
        expires_at: (Utc::now() + chrono::Duration::seconds(grant.expires_in))
            .to_rfc3339_opts(SecondsFormat::Secs, true),
        browser_opened,
    };

    thread::spawn(move || {
        let client = http();
        let outcome = poll_token(&client, &target, &registration, &grant, generation);
        if GENERATION.load(Ordering::SeqCst) == generation {
            if let Ok(mut url) = PENDING_URL.lock() {
                *url = None;
            }
        }
        match outcome {
            PollOutcome::Superseded => {}
            PollOutcome::Failed(message) => {
                crate::diag::log(&format!("[login] 登录失败：{message}"));
                let _ = app.emit("login://failed", message);
            }
            PollOutcome::Token(response) => match store_credentials(&target, &registration, &response) {
                Ok(()) => {
                    crate::diag::log("[login] 登录成功，凭据已写入自有凭据目录");
                    finish_success(&app);
                }
                Err(e) => {
                    let _ = app.emit("login://failed", format!("保存凭据失败：{e}"));
                }
            },
        }
    });
    Ok(session)
}

/// 取消等待中的登录（轮询线程 1 秒内退出）。
#[tauri::command]
pub fn k2a_login_cancel() {
    GENERATION.fetch_add(1, Ordering::SeqCst);
    if let Ok(mut url) = PENDING_URL.lock() {
        *url = None;
    }
}

/// 重新打开验证页（浏览器被关掉、或第一次没能自动打开时）。
#[tauri::command]
pub fn k2a_login_reopen(app: AppHandle) -> Result<(), String> {
    let url = PENDING_URL
        .lock()
        .ok()
        .and_then(|u| u.clone())
        .ok_or_else(|| "没有等待中的登录".to_string())?;
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| format!("打开浏览器失败：{e}"))
}

/// 退出自有登录：只删自有凭据目录里的令牌与注册文件，不碰 Kiro IDE / CLI 的凭据。
#[tauri::command]
pub async fn k2a_logout(app: AppHandle) -> Result<super::service::ProxyStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        remove_registration_of_current_token();
        let token = own_credential_dir().join(TOKEN_FILE);
        if token.exists() {
            fs::remove_file(&token).map_err(|e| format!("删除凭据失败：{e}"))?;
        }
        crate::diag::log("[login] 已退出自有登录");
        restart_proxy_if_running(&app);
        let state = app.state::<super::service::KiroProxyState>();
        Ok(super::service::build_status(&state))
    })
    .await
    .map_err(|e| format!("退出登录任务失败：{e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_target_is_fixed() {
        let t = resolve_target("builder", Some("https://evil".into()), Some("eu-west-1".into())).unwrap();
        assert_eq!(t.start_url, BUILDER_ID_START_URL);
        assert_eq!(t.region, "us-east-1");
        assert_eq!(t.provider, "BuilderId");
    }

    #[test]
    fn idc_target_validates_url_and_region() {
        assert!(resolve_target("idc", Some("http://x".into()), None).is_err());
        assert!(resolve_target("idc", Some("https://".into()), None).is_err());
        assert!(resolve_target("idc", Some("https://d-1.awsapps.com/start".into()), Some("evil.com/x".into())).is_err());
        let t = resolve_target("idc", Some(" https://d-1.awsapps.com/start#".into()), Some("EU-West-1".into())).unwrap();
        assert_eq!(t.start_url, "https://d-1.awsapps.com/start");
        assert_eq!(t.region, "eu-west-1");
        let t = resolve_target("idc", Some("https://d-1.awsapps.com/start".into()), None).unwrap();
        assert_eq!(t.region, "us-east-1");
        assert!(resolve_target("google", None, None).is_err());
    }
}
