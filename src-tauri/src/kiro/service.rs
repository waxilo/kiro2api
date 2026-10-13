//! 应用层集成：Tauri 命令、代理生命周期、模型列表、接入信息、凭据定时刷新。
//!
//! 这一层与协议内核（`server` / `transform` / `openai` / `auth`）的职责边界是：
//! 内核只关心「一次请求怎么翻译 + 怎么转发」，本层只关心「进程怎么启停、状态怎么呈现、
//! 配置怎么落盘」。因此这里不出现任何 HTTP 细节，也不出现任何界面概念。
//!
//! kiro2api 刻意**不**改写任何外部客户端的配置文件（不碰 `~/.claude/settings.json` 之类）：
//! 接入方式一律通过「一键复制配置片段」交给用户自行粘贴，避免应用私自改动用户环境。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::paths::kiro_sso_cache_dir;
use crate::prefs::{
    ensure_api_key, generate_api_key, load_config, mask_api_key, update_config, AppConfig,
};

pub(crate) const KIRO_RUNTIME_URL: &str = "https://runtime.us-east-1.kiro.dev/";
pub(crate) const KIRO_MANAGEMENT_URL: &str = "https://management.us-east-1.kiro.dev/";
/// 兜底默认模型：仅在无法查询账户可用模型时使用（部分套餐不含 Claude 模型）。
pub(crate) const KIRO_DEFAULT_MODEL: &str = "claude-opus-5";

/// OpenAI 兼容入口的路径前缀。与 `server.rs` 的路由共用同一常量，
/// 避免「界面显示的地址」与「服务实际监听的路径」漂移。
pub(crate) const KIRO_OPENAI_PATH_PREFIX: &str = "/openai/v1";

/// 定时刷新间隔（access token 通常约 1 小时）。
const REFRESH_DEFAULT_SECS: u64 = 25 * 60;
/// 最短刷新间隔，避免过于频繁打 OIDC。
const REFRESH_MIN_SECS: u64 = 5 * 60;
/// 过期前多久触发刷新。
const REFRESH_SKEW_SECS: i64 = 5 * 60;

// ── 运行态 ──────────────────────────────────────────────────────────────

pub struct KiroProxyState {
    pub(crate) inner: Arc<Mutex<Option<super::server::ProxyHandle>>>,
    pub(crate) key: Arc<Mutex<Option<String>>>,
    pub(crate) port: Arc<Mutex<Option<u16>>>,
    /// 递增后旧刷新循环会自行退出；代理启动时再开新循环。
    pub(crate) refresh_generation: Arc<AtomicU64>,
    /// 串行化 start / stop，避免连点并发启停导致孤儿代理或卡死。
    pub(crate) op_lock: Arc<Mutex<()>>,
}

impl Clone for KiroProxyState {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            key: Arc::clone(&self.key),
            port: Arc::clone(&self.port),
            refresh_generation: Arc::clone(&self.refresh_generation),
            op_lock: Arc::clone(&self.op_lock),
        }
    }
}

impl Default for KiroProxyState {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(None)),
            key: Arc::new(Mutex::new(None)),
            port: Arc::new(Mutex::new(None)),
            refresh_generation: Arc::new(AtomicU64::new(0)),
            op_lock: Arc::new(Mutex::new(())),
        }
    }
}

type ProxyStateRef = KiroProxyState;

fn is_proxy_running(state: &ProxyStateRef) -> bool {
    state
        .inner
        .lock()
        .map(|inner| {
            inner
                .as_ref()
                .and_then(|handle| handle.thread.as_ref())
                .map(|thread| !thread.is_finished())
                .unwrap_or(false)
        })
        .unwrap_or(false)
}

fn new_auth() -> super::auth::Auth {
    super::auth::Auth::new(
        kiro_sso_cache_dir(),
        None,
        KIRO_RUNTIME_URL.to_string(),
        KIRO_MANAGEMENT_URL.to_string(),
    )
}

/// 本机是否有可用 Kiro 凭据（凭据文件或环境变量），不发网络请求。
pub(crate) fn has_credential_file() -> bool {
    kiro_sso_cache_dir().join("kiro-auth-token.json").is_file()
}

pub(crate) fn is_locally_available() -> bool {
    if has_credential_file() {
        return true;
    }
    ["KIRO_AUTHORIZATION", "KIRO_ACCESS_TOKEN"]
        .iter()
        .any(|key| {
            std::env::var(key)
                .ok()
                .map(|v| !v.trim().is_empty())
                .unwrap_or(false)
        })
}

/// 当前生效的代理密钥：**内存副本优先，持久化值兜底**。
///
/// 不能以 `is_proxy_running` 作为分支条件 —— 线程一旦 spawn，`is_proxy_running` 立即为 true，
/// 而内存 key 是稍后几行才写入的。若在这段窗口内只看内存副本，就会读到空密钥，
/// 前端把复制按钮置灰，用户看到的现象是「刚打开就要重开一次才能复制」。
fn current_api_key(state: &ProxyStateRef) -> Option<String> {
    let in_memory = state
        .key
        .lock()
        .ok()
        .and_then(|key| key.clone())
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty());
    in_memory.or_else(|| {
        load_config()
            .proxy_api_key
            .map(|key| key.trim().to_string())
            .filter(|key| !key.is_empty())
    })
}

// ── 对外状态结构 ────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ProxyStatus {
    pub running: bool,
    /// 本机具备 Kiro 凭据（凭据文件 / 环境变量）或代理已在运行。
    pub available: bool,
    pub port: Option<u16>,
    pub has_api_key: bool,
    /// 凭据来源描述（SSO 共享缓存 / 环境变量 / 未找到）。
    pub auth_source: String,
    pub expires_at: Option<String>,
    pub profile_arn: Option<String>,
    pub model: String,
    pub auto_start: bool,
    pub config_dir: String,
    pub debug_log_path: String,
}

pub(crate) fn build_status(state: &ProxyStateRef) -> ProxyStatus {
    let running = is_proxy_running(state);
    let port = state.port.lock().ok().and_then(|p| *p);
    let key = current_api_key(state);
    let auth = new_auth();
    let config = load_config();
    ProxyStatus {
        running,
        available: is_locally_available() || running,
        port,
        has_api_key: key.is_some(),
        auth_source: auth.describe_auth_source(),
        expires_at: auth.get_sso_token_expiry(),
        profile_arn: auth.get_sso_profile_arn(),
        model: effective_default_model(&config),
        auto_start: config.auto_start,
        config_dir: crate::paths::get_data_path().display().to_string(),
        debug_log_path: crate::paths::get_debug_log_path().display().to_string(),
    }
}

fn effective_default_model(config: &AppConfig) -> String {
    let configured = config.default_model.trim();
    if !configured.is_empty() {
        return configured.to_string();
    }
    config
        .display_models
        .first()
        .or_else(|| config.custom_models.first())
        .cloned()
        .unwrap_or_else(|| KIRO_DEFAULT_MODEL.to_string())
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ModelsState {
    pub running: bool,
    pub display_models: Vec<String>,
    pub custom_models: Vec<String>,
    /// 展示列表 + 自定义列表合并去重后的候选集。
    pub models: Vec<String>,
    pub default_model: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveModelsConfig {
    #[serde(default)]
    pub display_models: Vec<String>,
    #[serde(default)]
    pub custom_models: Vec<String>,
    #[serde(default)]
    pub default_model: Option<String>,
}

fn build_models_state(running: bool) -> ModelsState {
    let config = load_config();
    let mut merged = config.display_models.clone();
    for model in &config.custom_models {
        if !merged.iter().any(|existing| existing == model) {
            merged.push(model.clone());
        }
    }
    let default_model = effective_default_model(&config);
    ModelsState {
        running,
        display_models: config.display_models,
        custom_models: config.custom_models,
        models: merged,
        default_model,
    }
}

fn normalize_model_list(models: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut normalized = Vec::new();
    for model in models {
        let trimmed = model.trim();
        if trimmed.is_empty() || !seen.insert(trimmed.to_string()) {
            continue;
        }
        normalized.push(trimmed.to_string());
    }
    normalized
}

/// 双协议接入信息。明文密钥不经过前端：这里只回传脱敏串，
/// 复制动作由 [`k2a_copy_access`] 在后端直接写入系统剪贴板。
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ProxyAccess {
    pub running: bool,
    pub port: Option<u16>,
    /// Anthropic Messages 入口：`http://127.0.0.1:{port}`（Claude Code / Anthropic SDK）。
    pub anthropic_base_url: String,
    /// OpenAI Chat Completions 入口：`http://127.0.0.1:{port}/openai/v1`（OpenAI SDK / 兼容客户端）。
    pub openai_base_url: String,
    pub api_key_masked: String,
    pub has_api_key: bool,
    pub model: String,
}

pub(crate) fn build_access(state: &ProxyStateRef) -> ProxyAccess {
    let running = is_proxy_running(state);
    let port = if running {
        state.port.lock().ok().and_then(|p| *p)
    } else {
        None
    };
    let key = current_api_key(state);
    let origin = port.map(|port| format!("http://127.0.0.1:{port}"));
    ProxyAccess {
        running,
        port,
        anthropic_base_url: origin.clone().unwrap_or_default(),
        openai_base_url: origin
            .map(|origin| format!("{origin}{KIRO_OPENAI_PATH_PREFIX}"))
            .unwrap_or_default(),
        api_key_masked: key.as_deref().map(mask_api_key).unwrap_or_default(),
        has_api_key: key.is_some(),
        model: effective_default_model(&load_config()),
    }
}

// ── 启停 ────────────────────────────────────────────────────────────────

/// 启动代理核心逻辑（命令与自动启动共用）。调用方须已持有 `state.op_lock`。
pub(crate) fn start_proxy(state: &ProxyStateRef, port: Option<u16>) -> Result<ProxyStatus, String> {
    if is_proxy_running(state) {
        return Ok(build_status(state));
    }

    // 防御性清理：句柄若残留则先停掉，避免端口被自己占住
    if let Some(mut existing) = state
        .inner
        .lock()
        .map_err(|_| "代理句柄锁获取失败".to_string())?
        .take()
    {
        existing.stop();
    }

    let auth = new_auth();
    let auth_ok = matches!(auth.get_authorization(), Ok(Some(_)));
    if !auth_ok {
        return Err(format!(
            "未找到 Kiro 凭据：请先登录 Kiro IDE 或执行 `kiro-cli login`，\
             确认 {} 存在后再试。",
            kiro_sso_cache_dir().join("kiro-auth-token.json").display()
        ));
    }

    // 优先使用账户实际可用的模型（部分套餐不含 Claude 模型），失败时兜底内置默认
    let default_model = auth
        .pick_available_default_model()
        .unwrap_or_else(|| KIRO_DEFAULT_MODEL.to_string());

    let key = ensure_api_key()?;
    let configured_port = port.unwrap_or_else(|| load_config().port);
    let config = super::server::ProxyConfig {
        host: "127.0.0.1".to_string(),
        port: configured_port,
        proxy_api_key: Some(key.clone()),
        default_model: default_model.clone(),
        runtime_url: KIRO_RUNTIME_URL.to_string(),
        management_url: KIRO_MANAGEMENT_URL.to_string(),
        sso_cache_dir: kiro_sso_cache_dir(),
        profile_arn_override: None,
    };
    let handle = super::server::start_proxy(config)?;
    let actual_port = handle.port;

    *state.key.lock().map_err(|_| "lock failed".to_string())? = Some(key.clone());
    *state.port.lock().map_err(|_| "lock failed".to_string())? = Some(actual_port);
    *state.inner.lock().map_err(|_| "lock failed".to_string())? = Some(handle);

    // 记下端口与运行态，供下次启动恢复
    let _ = update_config(|config| {
        config.port = actual_port;
        config.last_running = true;
        if config.default_model.trim().is_empty() {
            config.default_model = default_model;
        }
        Ok(())
    });

    // 启动后立即从反代 /v1/models 同步一次模型列表
    if let Err(error) = sync_models(actual_port, &key) {
        eprintln!("[proxy] 启动后同步模型列表失败: {error}");
    }

    spawn_refresh_loop(state.clone());
    Ok(build_status(state))
}

/// 停止代理并清空运行态。
pub(crate) fn stop_proxy(state: &ProxyStateRef) -> Result<ProxyStatus, String> {
    state.refresh_generation.fetch_add(1, Ordering::SeqCst);
    if let Some(mut handle) = state
        .inner
        .lock()
        .map_err(|_| "代理句柄锁获取失败".to_string())?
        .take()
    {
        handle.stop();
    }
    *state.port.lock().map_err(|_| "lock failed".to_string())? = None;
    *state.key.lock().map_err(|_| "lock failed".to_string())? = None;
    let _ = update_config(|config| {
        config.last_running = false;
        Ok(())
    });
    Ok(build_status(state))
}

// ── 凭据刷新 ────────────────────────────────────────────────────────────

/// 强制刷新共享 SSO 凭据（IDE / CLI 同一文件），写回 `~/.aws/sso/cache`。
/// 返回 `(是否执行了刷新, 最新 expiresAt)`。
pub(crate) fn refresh_credentials() -> Result<(bool, Option<String>), String> {
    touch_kiro_cli();
    new_auth().force_refresh()
}

/// 每次「轻触」最多等多久。到点就终止子进程，绝不无限期等。
const KIRO_TOUCH_TIMEOUT: Duration = Duration::from_secs(8);

/// 若本机装了 kiro-cli，轻触一次以保持 CLI 侧会话活跃（不依赖其输出）。
///
/// 两个坑记在这里：
/// * **不能用 `Child::wait()`**（没有上限）。这个命令可能是整个 Electron 运行时的包装
///   （本机 `kiro` 就是 `Kiro\bin\kiro.cmd` → `Kiro.exe cli.js`），也可能在会话过期时
///   转去走登录流程而挂住。一旦它不退出，`refresh_credentials` 就永远不返回，前端的
///   `busy` 永久不解除 —— 整个界面锁死、只能重启。故一律走 `wait_with_timeout`。
/// * **Rust 的 `Command::new` 在 Windows 上不做 PATHEXT 解析**，所以这里传 `"kiro"`
///   匹配不到 `kiro.cmd`，会直接返回 NotFound（`kiro-cli` 同理）。也就是说在只有
///   `.cmd` 启动器的机器上，这个函数是空操作 —— 这是有意留着的：装上真正的
///   `kiro-cli.exe` 后它会自动生效，而空操作不会带来任何副作用。
fn touch_kiro_cli() {
    for bin in ["kiro-cli", "kiro"] {
        let mut cmd = std::process::Command::new(bin);
        crate::proc_guard::suppress_console_window(&mut cmd);
        let Ok(mut child) = cmd
            .arg("whoami")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        else {
            continue;
        };
        match crate::proc_guard::wait_with_timeout(&mut child, KIRO_TOUCH_TIMEOUT) {
            Some(status) if status.success() => {
                eprintln!("[proxy] 已在刷新前轻触 `{bin} whoami`");
                return;
            }
            Some(_) => {}
            None => eprintln!(
                "[proxy] 轻触 `{bin} whoami` 超过 {KIRO_TOUCH_TIMEOUT:?} 未退出，已终止（不影响本次刷新）"
            ),
        }
    }
}

fn next_refresh_delay(expires_at: Option<&str>) -> Duration {
    let default = Duration::from_secs(REFRESH_DEFAULT_SECS);
    let Some(raw) = expires_at else {
        return default;
    };
    let trimmed = raw.trim();
    if let Ok(exp) = chrono::DateTime::parse_from_rfc3339(trimmed) {
        return delay_until(exp.timestamp_millis());
    }
    // 兼容毫秒 ISO：2026-08-13T02:25:41.685Z
    let bare = trimmed.trim_end_matches('Z');
    for format in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S"] {
        if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(bare, format) {
            return delay_until(naive.and_utc().timestamp_millis());
        }
    }
    default
}

fn delay_until(expires_at_ms: i64) -> Duration {
    let default = Duration::from_secs(REFRESH_DEFAULT_SECS);
    let min = Duration::from_secs(REFRESH_MIN_SECS);
    let now = chrono::Utc::now().timestamp_millis();
    let target = expires_at_ms - REFRESH_SKEW_SECS * 1000;
    if target <= now {
        return min;
    }
    Duration::from_millis((target - now) as u64).clamp(min, default)
}

/// 可中断睡眠：代际变化或代理停止时立即返回 false。
fn sleep_interruptible(total: Duration, generation: u64, state: &ProxyStateRef) -> bool {
    let step = Duration::from_secs(2);
    let mut remaining = total;
    while remaining > Duration::ZERO {
        if state.refresh_generation.load(Ordering::SeqCst) != generation || !is_proxy_running(state)
        {
            return false;
        }
        let chunk = remaining.min(step);
        thread::sleep(chunk);
        remaining = remaining.saturating_sub(chunk);
    }
    state.refresh_generation.load(Ordering::SeqCst) == generation
}

/// 代理运行期间定时刷新共享 SSO 凭据，并向前端广播结果。
pub(crate) fn spawn_refresh_loop(state: ProxyStateRef) {
    let generation = state.refresh_generation.fetch_add(1, Ordering::SeqCst) + 1;
    let Some(app) = crate::APP_HANDLE.get().cloned() else {
        return;
    };
    thread::spawn(move || {
        eprintln!("[proxy] 凭据刷新循环启动 (gen={generation})");
        // 启动后先等一小段，避开代理刚接手请求的窗口
        if !sleep_interruptible(Duration::from_secs(30), generation, &state) {
            return;
        }
        loop {
            if state.refresh_generation.load(Ordering::SeqCst) != generation || !is_proxy_running(&state)
            {
                break;
            }

            match refresh_credentials() {
                Ok((true, expires_at)) => {
                    eprintln!("[proxy] 共享 SSO 凭据已刷新");
                    let mut status = build_status(&state);
                    if expires_at.is_some() {
                        status.expires_at = expires_at;
                    }
                    let _ = app.emit("proxy://token-refreshed", &status);
                }
                Ok((false, _)) => {
                    eprintln!("[proxy] 跳过刷新：无 refreshToken（环境变量鉴权）");
                }
                Err(error) => {
                    eprintln!("[proxy] 凭据刷新失败: {error}");
                    let _ = app.emit("proxy://token-refresh-failed", &error);
                }
            }

            let delay = next_refresh_delay(build_status(&state).expires_at.as_deref());
            if !sleep_interruptible(delay, generation, &state) {
                break;
            }
        }
        eprintln!("[proxy] 凭据刷新循环退出 (gen={generation})");
    });
}

// ── 模型列表 ────────────────────────────────────────────────────────────

/// 从本地反代 `/v1/models` 拉取模型 ID 列表。
pub(crate) fn fetch_model_ids(port: u16, api_key: &str) -> Result<Vec<String>, String> {
    let url = format!("http://127.0.0.1:{port}/v1/models");
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(45))
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;

    let mut last_error = String::from("unknown");
    // 代理线程刚起来或上游 ListAvailableModels 较慢时短暂重试
    for attempt in 1..=4u32 {
        match client
            .get(&url)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .send()
        {
            Ok(response) => {
                let status = response.status();
                if !status.is_success() {
                    let body = response.text().unwrap_or_default();
                    last_error = format!("HTTP {status}: {body}");
                } else {
                    let data: serde_json::Value = response
                        .json()
                        .map_err(|e| format!("解析模型列表失败: {e}"))?;
                    let mut ids: Vec<String> = data
                        .get("data")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|item| item.get("id").and_then(|v| v.as_str()))
                                .map(|s| s.trim().to_string())
                                .filter(|s| !s.is_empty())
                                .collect()
                        })
                        .unwrap_or_default();
                    ids.sort();
                    ids.dedup();
                    if ids.is_empty() {
                        last_error = "模型列表为空".to_string();
                    } else {
                        return Ok(ids);
                    }
                }
            }
            Err(error) => last_error = error.to_string(),
        }
        if attempt < 4 {
            thread::sleep(Duration::from_millis(400 * u64::from(attempt)));
        }
    }
    Err(format!("拉取模型列表失败: {last_error}"))
}

/// 同步上游模型列表到本地配置，并挑选一个合理的默认模型（保留原有选择优先）。
pub(crate) fn sync_models(port: u16, api_key: &str) -> Result<Vec<String>, String> {
    let ids = fetch_model_ids(port, api_key)?;
    update_config(|config| {
        let preferred = [config.default_model.trim()]
            .into_iter()
            .find(|model| !model.is_empty() && ids.iter().any(|id| id == model))
            .map(str::to_string)
            .or_else(|| ids.first().cloned())
            .unwrap_or_else(|| KIRO_DEFAULT_MODEL.to_string());
        config.display_models = ids.clone();
        config.default_model = preferred;
        Ok(())
    })?;
    Ok(ids)
}

// ── 接入配置文本 ────────────────────────────────────────────────────────

/// 该复制类型是否需要读取明文密钥。
///
/// 与前端「按钮是否可点」必须共用同一判据：地址 / 模型这类不依赖密钥的复制项，
/// 在密钥缺失时也必须可用；否则会出现「按钮可点、点了报错」。
fn kind_needs_key(kind: &str) -> bool {
    matches!(
        kind,
        "api_key" | "anthropic_env" | "anthropic_json" | "openai_env" | "openai_json"
    )
}

/// 组装写入剪贴板的接入文本。
/// kind: anthropic_base_url | openai_base_url | api_key | model
///     | anthropic_env | anthropic_json | openai_env | openai_json
pub(crate) fn format_access_text(
    kind: &str,
    anthropic_base_url: &str,
    openai_base_url: &str,
    key: &str,
    model: &str,
) -> Result<String, String> {
    Ok(match kind.trim() {
        "anthropic_base_url" => anthropic_base_url.to_string(),
        "openai_base_url" => openai_base_url.to_string(),
        "api_key" => key.to_string(),
        "model" => model.to_string(),
        // 通用 .env / shell 变量形式
        "anthropic_env" => format!(
            "ANTHROPIC_BASE_URL={anthropic_base_url}\nANTHROPIC_AUTH_TOKEN={key}\nANTHROPIC_MODEL={model}"
        ),
        // Claude Code settings.json 的 env 片段
        "anthropic_json" => serde_json::to_string_pretty(&serde_json::json!({
            "env": {
                "ANTHROPIC_BASE_URL": anthropic_base_url,
                "ANTHROPIC_AUTH_TOKEN": key,
                "ANTHROPIC_MODEL": model,
            }
        }))
        .map_err(|error| format!("生成配置片段失败: {error}"))?,
        // OpenAI SDK / 兼容客户端通用变量名（openai-python、LangChain、各类 ChatBox 等）
        "openai_env" => format!(
            "OPENAI_BASE_URL={openai_base_url}\nOPENAI_API_KEY={key}\nOPENAI_MODEL={model}"
        ),
        "openai_json" => serde_json::to_string_pretty(&serde_json::json!({
            "base_url": openai_base_url,
            "api_key": key,
            "model": model,
        }))
        .map_err(|error| format!("生成配置片段失败: {error}"))?,
        other => return Err(format!("不支持的复制类型: {other}")),
    })
}

fn build_access_text(state: &ProxyStateRef, kind: &str) -> Result<String, String> {
    let access = build_access(state);
    if !access.running {
        return Err("请先启动代理，再复制接入信息".to_string());
    }
    let key = if kind_needs_key(kind) {
        current_api_key(state).ok_or_else(|| "代理密钥不可用，请重新启动代理".to_string())?
    } else {
        String::new()
    };
    format_access_text(
        kind,
        &access.anthropic_base_url,
        &access.openai_base_url,
        &key,
        &access.model,
    )
}

// ── Tauri 命令 ──────────────────────────────────────────────────────────

#[tauri::command]
pub fn k2a_status(state: State<'_, ProxyStateRef>) -> ProxyStatus {
    build_status(&state)
}

#[tauri::command]
pub fn k2a_models(state: State<'_, ProxyStateRef>) -> ModelsState {
    build_models_state(is_proxy_running(&state))
}

#[tauri::command]
pub fn k2a_access(state: State<'_, ProxyStateRef>) -> ProxyAccess {
    build_access(&state)
}

#[tauri::command]
pub async fn k2a_start(
    state: State<'_, ProxyStateRef>,
    port: Option<u16>,
) -> Result<ProxyStatus, String> {
    let state = (*state).clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _op = state
            .op_lock
            .lock()
            .map_err(|_| "操作锁获取失败".to_string())?;
        start_proxy(&state, port)
    })
    .await
    .map_err(|e| format!("启动任务失败: {e}"))?
}

#[tauri::command]
pub async fn k2a_stop(state: State<'_, ProxyStateRef>) -> Result<ProxyStatus, String> {
    let state = (*state).clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _op = state
            .op_lock
            .lock()
            .map_err(|_| "操作锁获取失败".to_string())?;
        stop_proxy(&state)
    })
    .await
    .map_err(|e| format!("停止任务失败: {e}"))?
}

/// 从运行中的代理重新同步模型列表。
#[tauri::command]
pub async fn k2a_sync_models(state: State<'_, ProxyStateRef>) -> Result<ModelsState, String> {
    let state = (*state).clone();
    tauri::async_runtime::spawn_blocking(move || {
        if !is_proxy_running(&state) {
            return Err("请先启动代理".to_string());
        }
        let port = state
            .port
            .lock()
            .map_err(|_| "lock failed".to_string())?
            .ok_or_else(|| "代理端口未知".to_string())?;
        let key = current_api_key(&state).ok_or_else(|| "代理密钥未知".to_string())?;
        sync_models(port, &key)?;
        Ok(build_models_state(true))
    })
    .await
    .map_err(|e| format!("同步任务失败: {e}"))?
}

/// 保存展示 / 自定义模型列表与默认模型。
#[tauri::command]
pub fn k2a_save_models(config: SaveModelsConfig) -> Result<ModelsState, String> {
    update_config(|current| {
        current.display_models = normalize_model_list(&config.display_models);
        current.custom_models = normalize_model_list(&config.custom_models);
        let all: Vec<String> = current
            .display_models
            .iter()
            .chain(current.custom_models.iter())
            .cloned()
            .collect();
        let preferred = config
            .default_model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .map(str::to_string)
            .or_else(|| {
                let existing = current.default_model.trim();
                all.iter().any(|id| id == existing).then(|| existing.to_string())
            })
            .or_else(|| all.first().cloned())
            .unwrap_or_default();
        current.default_model = preferred;
        Ok(())
    })?;
    Ok(build_models_state(false))
}

/// 设置默认模型（会顺带把它加进自定义列表，避免选择器里看不到）。
#[tauri::command]
pub fn k2a_set_default_model(model: String) -> Result<ModelsState, String> {
    let trimmed = model.trim().to_string();
    if trimmed.is_empty() {
        return Err("模型名称不能为空".to_string());
    }
    update_config(|current| {
        let known = current
            .display_models
            .iter()
            .chain(current.custom_models.iter())
            .any(|id| id == &trimmed);
        if !known {
            current.custom_models.push(trimmed.clone());
        }
        current.default_model = trimmed.clone();
        Ok(())
    })?;
    Ok(build_models_state(false))
}

#[tauri::command]
pub fn k2a_set_auto_start(enabled: bool) -> Result<bool, String> {
    update_config(|config| {
        config.auto_start = enabled;
        Ok(enabled)
    })
}

#[tauri::command]
pub fn k2a_set_port(port: u16) -> Result<u16, String> {
    if port < 1024 {
        return Err("端口需大于 1023，避免占用系统保留端口".to_string());
    }
    update_config(|config| {
        config.port = port;
        Ok(port)
    })
}

/// 复制接入信息到系统剪贴板（明文密钥只在后端流转）。
#[tauri::command]
pub fn k2a_copy_access(state: State<'_, ProxyStateRef>, kind: String) -> Result<bool, String> {
    let text = build_access_text(&state, kind.trim())?;
    if text.trim().is_empty() {
        return Ok(false);
    }
    let mut clipboard =
        arboard::Clipboard::new().map_err(|e| format!("无法访问系统剪贴板: {e}"))?;
    clipboard
        .set_text(text)
        .map_err(|e| format!("写入剪贴板失败: {e}"))?;
    Ok(true)
}

/// 重置代理密钥（旧密钥立即失效）。代理运行中不允许重置，避免正在用的客户端突然失配。
#[tauri::command]
pub fn k2a_reset_key(state: State<'_, ProxyStateRef>) -> Result<ProxyAccess, String> {
    if is_proxy_running(&state) {
        return Err("请先停止代理，再重置密钥".to_string());
    }
    let key = generate_api_key();
    update_config(|config| {
        config.proxy_api_key = Some(key);
        Ok(())
    })?;
    Ok(build_access(&state))
}

/// 查询 Kiro 账户额度（阻塞 HTTP 放后台线程，不卡界面）。
#[tauri::command]
pub async fn k2a_usage() -> Result<super::auth::KiroUsageInfo, String> {
    tauri::async_runtime::spawn_blocking(|| new_auth().get_usage_limits())
        .await
        .map_err(|e| format!("额度查询任务失败: {e}"))?
}

/// 手动刷新共享 SSO 凭据。
#[tauri::command]
pub async fn k2a_refresh_token(app: AppHandle) -> Result<ProxyStatus, String> {
    let (refreshed, expires_at) = tauri::async_runtime::spawn_blocking(refresh_credentials)
        .await
        .map_err(|e| format!("凭据刷新任务失败: {e}"))??;
    if !refreshed {
        return Err(
            "未找到可刷新的 Kiro refreshToken，请先登录 Kiro IDE 或执行 `kiro-cli login`"
                .to_string(),
        );
    }
    let state = app.state::<ProxyStateRef>();
    let mut status = build_status(&state);
    if let Some(exp) = expires_at {
        status.expires_at = Some(exp);
    }
    let _ = app.emit("proxy://token-refreshed", &status);
    Ok(status)
}

/// 在系统文件管理器里打开配置目录或诊断日志（便于排查问题）。
#[tauri::command]
pub fn k2a_reveal(path: String) -> Result<(), String> {
    let target = match path.as_str() {
        "config" => crate::paths::get_data_path(),
        "log" => crate::paths::get_debug_log_path(),
        "sso" => kiro_sso_cache_dir(),
        other => return Err(format!("未知的路径类型: {other}")),
    };
    crate::reveal_in_file_manager(&target)
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "http://127.0.0.1:5680";
    const O: &str = "http://127.0.0.1:5680/openai/v1";
    const K: &str = "k2a-test1234";
    const M: &str = "claude-opus-5";

    #[test]
    fn single_field_kinds() {
        assert_eq!(format_access_text("anthropic_base_url", A, O, K, M).unwrap(), A);
        assert_eq!(format_access_text("openai_base_url", A, O, K, M).unwrap(), O);
        assert_eq!(format_access_text("api_key", A, O, K, M).unwrap(), K);
        assert_eq!(format_access_text("model", A, O, K, M).unwrap(), M);
    }

    /// 两套协议的入口地址必须不同：OpenAI 入口要带 `/openai/v1` 前缀。
    /// 若两者相同，OpenAI SDK 自带 `/v1` 会拼成 `/v1/chat/completions` 落到 Anthropic 路由上。
    #[test]
    fn two_protocol_urls_are_distinct() {
        let anthropic = format_access_text("anthropic_base_url", A, O, K, M).unwrap();
        let openai = format_access_text("openai_base_url", A, O, K, M).unwrap();
        assert_ne!(anthropic, openai);
        assert!(!anthropic.ends_with(KIRO_OPENAI_PATH_PREFIX));
        assert!(openai.ends_with(KIRO_OPENAI_PATH_PREFIX));
    }

    #[test]
    fn anthropic_env_uses_anthropic_variable_names() {
        let text = format_access_text("anthropic_env", A, O, K, M).unwrap();
        assert_eq!(
            text,
            "ANTHROPIC_BASE_URL=http://127.0.0.1:5680\n\
             ANTHROPIC_AUTH_TOKEN=k2a-test1234\n\
             ANTHROPIC_MODEL=claude-opus-5"
        );
        assert!(!text.contains("OPENAI_"));
    }

    #[test]
    fn openai_env_uses_openai_variable_names() {
        let text = format_access_text("openai_env", A, O, K, M).unwrap();
        assert_eq!(
            text,
            "OPENAI_BASE_URL=http://127.0.0.1:5680/openai/v1\n\
             OPENAI_API_KEY=k2a-test1234\n\
             OPENAI_MODEL=claude-opus-5"
        );
        // 不能混入 Anthropic 变量名，否则复制到 OpenAI 客户端后不生效
        assert!(!text.contains("ANTHROPIC"));
    }

    #[test]
    fn json_snippets_are_valid_json() {
        let anthropic: serde_json::Value =
            serde_json::from_str(&format_access_text("anthropic_json", A, O, K, M).unwrap()).unwrap();
        assert_eq!(anthropic["env"]["ANTHROPIC_BASE_URL"], A);
        assert_eq!(anthropic["env"]["ANTHROPIC_MODEL"], M);

        let openai: serde_json::Value =
            serde_json::from_str(&format_access_text("openai_json", A, O, K, M).unwrap()).unwrap();
        assert_eq!(openai["base_url"], O);
        assert_eq!(openai["api_key"], K);
    }

    #[test]
    fn unknown_kind_is_rejected() {
        assert!(format_access_text("oauth", A, O, K, M).is_err());
    }

    /// 判据必须与前端一致：只有真正依赖密钥的 kind 才需要读密钥，
    /// 否则无密钥时「接口地址 / 模型」按钮可点却报错。
    #[test]
    fn only_key_dependent_kinds_require_the_key() {
        for kind in ["anthropic_base_url", "openai_base_url", "model"] {
            assert!(!kind_needs_key(kind), "{kind} 不应依赖密钥");
        }
        for kind in [
            "api_key",
            "anthropic_env",
            "anthropic_json",
            "openai_env",
            "openai_json",
        ] {
            assert!(kind_needs_key(kind), "{kind} 应当依赖密钥");
        }
    }

    #[test]
    fn urls_and_model_copy_work_without_a_key() {
        assert_eq!(format_access_text("anthropic_base_url", A, O, "", M).unwrap(), A);
        assert_eq!(format_access_text("openai_base_url", A, O, "", M).unwrap(), O);
        assert_eq!(format_access_text("model", A, O, "", M).unwrap(), M);
    }

    #[test]
    fn model_list_normalization_dedupes_and_trims() {
        let input = vec![
            " a ".to_string(),
            "a".to_string(),
            "".to_string(),
            "b".to_string(),
        ];
        assert_eq!(normalize_model_list(&input), vec!["a", "b"]);
    }

    #[test]
    fn refresh_delay_falls_back_when_expiry_unparseable() {
        let delay = next_refresh_delay(Some("not-a-date"));
        assert_eq!(delay.as_secs(), REFRESH_DEFAULT_SECS);
        assert_eq!(next_refresh_delay(None).as_secs(), REFRESH_DEFAULT_SECS);
    }

    #[test]
    fn refresh_delay_is_clamped_for_near_expiry() {
        // 已过期 ⇒ 取最短间隔
        let past = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
        assert_eq!(
            next_refresh_delay(Some(&past)).as_secs(),
            REFRESH_MIN_SECS
        );
    }
}
