//! 应用层集成：Tauri 命令、代理生命周期、模型列表、接入信息、凭据定时刷新。
//!
//! 这一层与协议内核（`server` / `transform` / `openai` / `auth`）的职责边界是：
//! 内核只关心「一次请求怎么翻译 + 怎么转发」，本层只关心「进程怎么启停、状态怎么呈现、
//! 配置怎么落盘」。因此这里不出现任何 HTTP 细节，也不出现任何界面概念。
//!
//! kiro2api 刻意**不**改写任何外部客户端的配置文件（不碰 `~/.claude/settings.json` 之类）：
//! 接入方式一律通过「一键复制配置片段」交给用户自行粘贴，避免应用私自改动用户环境。
//! 唯一例外是 WorkBuddy 快捷导入（`crate::workbuddy`）：用户显式发起、只写它自己的
//! 模型清单且从不删除条目，该原则对其余客户端仍然成立。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::kiro::models::kiro_model_to_public_model_id;
use crate::paths::{active_credential_dir, kiro_sso_cache_dir};
use crate::prefs::{
    cached_model_rates, cached_models, cached_usage, ensure_api_key, generate_api_key, load_config,
    mask_api_key, ms_to_iso, store_models, store_usage, update_config,
};

pub(crate) const KIRO_RUNTIME_URL: &str = "https://runtime.us-east-1.kiro.dev/";
pub(crate) const KIRO_MANAGEMENT_URL: &str = "https://management.us-east-1.kiro.dev/";
/// 兜底默认模型：仅在无法查询账户可用模型时使用（部分套餐不含 Claude 模型）。
pub(crate) const KIRO_DEFAULT_MODEL: &str = "claude-opus-5";

/// OpenAI 兼容入口的路径前缀。与 `server.rs` 的路由共用同一常量，
/// 避免「界面显示的地址」与「服务实际监听的路径」漂移。
pub(crate) const KIRO_OPENAI_PATH_PREFIX: &str = "/openai/v1";
/// Anthropic 兼容入口的路径前缀，与 `/openai` 对称：客户端在接入地址后面自己拼
/// `/v1/messages` 等标准路径。同样是地址显示与路由的共用常量；根路径旧路由仍保留。
pub(crate) const KIRO_ANTHROPIC_PATH_PREFIX: &str = "/anthropic";

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
        active_credential_dir(),
        None,
        KIRO_RUNTIME_URL.to_string(),
        KIRO_MANAGEMENT_URL.to_string(),
    )
}

/// 本机是否有可用 Kiro 凭据（凭据文件或环境变量），不发网络请求。
/// 凭据文件缺失时先尝试从 kiro-cli 的凭据库桥接一份（只读 CLI 侧数据，
/// 且不覆盖已有文件），再按文件是否存在判定。
pub(crate) fn has_credential_file() -> bool {
    crate::paths::has_own_login() || super::cli_bridge::ensure_bridged()
}

/// 代理是否在运行（供登录模块判断凭据切换后要不要重启代理）。
pub(crate) fn proxy_running(state: &ProxyStateRef) -> bool {
    is_proxy_running(state)
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
pub(crate) fn current_api_key(state: &ProxyStateRef) -> Option<String> {
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
    pub config_dir: String,
    pub debug_log_path: String,
    /// 当前用的是 kiro2api 自己登录的凭据（界面据此显示「退出登录」）。
    pub own_login: bool,
}

pub(crate) fn build_status(state: &ProxyStateRef) -> ProxyStatus {
    let running = is_proxy_running(state);
    let port = state.port.lock().ok().and_then(|p| *p);
    let key = current_api_key(state);
    let auth = new_auth();
    let status = ProxyStatus {
        running,
        available: is_locally_available() || running,
        port,
        has_api_key: key.is_some(),
        auth_source: auth.describe_auth_source(),
        expires_at: auth.get_sso_token_expiry(),
        profile_arn: auth.get_sso_profile_arn(),
        config_dir: crate::paths::get_data_path().display().to_string(),
        debug_log_path: crate::paths::get_debug_log_path().display().to_string(),
        own_login: crate::paths::has_own_login(),
    };
    // 顺手让托盘跟上真实状态：正常启停各自会同步，这里兜住代理异常退出后的图标滞后
    crate::tray::sync_running(running, port);
    status
}

/// 账户可用的模型 ID 缓存。
///
/// 这里**没有**「当前选中的模型」——本应用是代理服务，具体用哪个模型由客户端在
/// 每次请求里自己指定（见 `server.rs` 的 `resolve_kiro_model`）。所以这里只是一份
/// 供人查看的清单：哪些模型这个账户能用。
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ModelsState {
    pub models: Vec<String>,
    /// 公共模型 id → 消耗倍率（rateMultiplier）。空表 = 上游没给过或还没同步过。
    #[serde(default)]
    pub rates: HashMap<String, f64>,
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
}

pub(crate) fn build_access(state: &ProxyStateRef) -> ProxyAccess {
    let running = is_proxy_running(state);
    // 接入地址是**配置**的函数，不是「进程活着」的函数：跑着用实际端口（被占时
    // bind_server 会向后找，可能与配置不同），停着用配置端口 —— 那是上次运行
    // 保存下来、下次启动优先尝试的值。地址照常给出，客户端可以先配好地址与密钥，
    // 等代理一启动就能通；逼用户「先跑一次再回来复制」没有收益。
    let port = if running {
        state.port.lock().ok().and_then(|p| *p)
    } else {
        Some(load_config().port)
    };
    let key = current_api_key(state);
    let origin = port.map(|port| format!("http://127.0.0.1:{port}"));
    ProxyAccess {
        running,
        port,
        anthropic_base_url: origin
            .clone()
            .map(|origin| format!("{origin}{KIRO_ANTHROPIC_PATH_PREFIX}"))
            .unwrap_or_default(),
        openai_base_url: origin
            .map(|origin| format!("{origin}{KIRO_OPENAI_PATH_PREFIX}"))
            .unwrap_or_default(),
        api_key_masked: key.as_deref().map(mask_api_key).unwrap_or_default(),
        has_api_key: key.is_some(),
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
            "未找到 Kiro 凭据：请在「Kiro 凭据」卡片里点「登录」，或先登录 Kiro IDE / 执行 \
             `kiro-cli login`，确认 {} 存在后再试。",
            kiro_sso_cache_dir().join("kiro-auth-token.json").display()
        ));
    }

    // 优先使用持久化的模型缓存挑默认（零网络）；缓存为空（首次使用）才问一次上游。
    // ⚠️ 不能无条件走 `pick_available_default_model`：它每次都要打上游控制面（最长 30s），
    // 而模型列表早就随每次同步落盘了 —— 弱网下「启动代理」会在这里 + 下面的后台同步
    // 各等一次 30s，前端 45 秒的保底闸门先触发，表现为「代理明明起来了却报启动超时」。
    let default_model = {
        let cached = cached_models();
        match super::auth::pick_default_model(&cached) {
            Some(model) => model,
            None => auth
                .pick_available_default_model()
                .unwrap_or_else(|| KIRO_DEFAULT_MODEL.to_string()),
        }
    };

    let key = ensure_api_key()?;
    let configured_port = port.unwrap_or_else(|| load_config().port);
    let config = super::server::ProxyConfig {
        host: "127.0.0.1".to_string(),
        port: configured_port,
        proxy_api_key: Some(key.clone()),
        default_model: default_model.clone(),
        runtime_url: KIRO_RUNTIME_URL.to_string(),
        management_url: KIRO_MANAGEMENT_URL.to_string(),
        sso_cache_dir: active_credential_dir(),
        profile_arn_override: None,
    };
    let handle = super::server::start_proxy(config)?;
    let actual_port = handle.port;

    *state.key.lock().map_err(|_| "lock failed".to_string())? = Some(key.clone());
    *state.port.lock().map_err(|_| "lock failed".to_string())? = Some(actual_port);
    *state.inner.lock().map_err(|_| "lock failed".to_string())? = Some(handle);

    // 记下端口，供下次启动恢复
    let _ = update_config(|config| {
        config.port = actual_port;
        Ok(())
    });

    // 启动后同步一次模型列表：账户可用的模型与套餐有关（部分套餐不含 Claude），
    // 上游也会不定期上新。这里顺手把最新清单落盘并推给界面。
    // 同步走的是上游控制面而不是本地反代，所以它失败不影响代理本身的可用性。
    //
    // ⚠️ 放后台线程，不让它挡在「启动代理」的返回路径上：它内部是一次完整的上游
    // HTTP（最长 30s），弱网下会把命令的总耗时顶过前端的 45 秒保底闸门 —— 端口
    // 早就绑好了，用户看到的却是「启动代理超时」。它只是顺手刷新，没资格阻塞启动。
    std::thread::spawn(refresh_models_and_notify);

    spawn_refresh_loop(state.clone());
    crate::tray::sync_running(true, Some(actual_port));
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
    crate::tray::sync_running(false, None);
    Ok(build_status(state))
}

// ── 凭据刷新 ────────────────────────────────────────────────────────────

/// 强制刷新共享 SSO 凭据（IDE / CLI 同一文件），写回 `~/.aws/sso/cache`。
/// 返回 `(是否执行了刷新, 最新 expiresAt)`。
///
/// `source` 标明这次刷新是谁发起的（手动 / 后台循环 / 预检恢复 / 请求重试）：
/// 刷新全程的分阶段耗时都会追加进 kiro-debug.log，并发刷新时靠它才分得清哪行是哪次。
/// 打包版没有控制台，`eprintln!` 的输出谁也看不见 —— 没有这份时间线，
/// 「刷新凭据超时」就只能靠猜。日志只记耗时、端点与到期时间，不记任何令牌内容。
pub(crate) fn refresh_credentials(source: &'static str) -> Result<(bool, Option<String>), String> {
    let started = Instant::now();
    let touch = touch_kiro_cli(source);
    let result = new_auth().force_refresh(source);
    let rest = started.elapsed().saturating_sub(touch);
    let total = crate::diag::secs(started.elapsed());
    let touch_secs = crate::diag::secs(touch);
    let rest_secs = crate::diag::secs(rest);
    match &result {
        Ok((true, expires_at)) => crate::diag::log(&format!(
            "[refresh:{source}] 刷新完成：{total}（轻触 {touch_secs} + 刷新 {rest_secs}），新到期 {}",
            expires_at.as_deref().unwrap_or("—"),
        )),
        Ok((false, _)) => crate::diag::log(&format!(
            "[refresh:{source}] 无可刷新的凭据：{total}（轻触 {touch_secs} + 刷新 {rest_secs}）",
        )),
        Err(error) => crate::diag::log(&format!(
            "[refresh:{source}] 刷新失败：{total}（轻触 {touch_secs} + 刷新 {rest_secs}）：{error}",
        )),
    }
    result
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
/// 返回这段「轻触」花了多久（两个二进制都不可用时 ≈ 0，总账里看得见）。
fn touch_kiro_cli(source: &'static str) -> Duration {
    let started = Instant::now();
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
                crate::diag::log(&format!(
                    "[refresh:{source}] 轻触 `{bin} whoami`：{}，成功",
                    crate::diag::secs(started.elapsed())
                ));
                return started.elapsed();
            }
            Some(_) => {}
            None => crate::diag::log(&format!(
                "[refresh:{source}] 轻触 `{bin} whoami`：{} 未退出，已终止（不影响本次刷新）",
                crate::diag::secs(KIRO_TOUCH_TIMEOUT)
            )),
        }
    }
    started.elapsed()
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

            match refresh_credentials("后台循环") {
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

/// 从上游控制面拉取账户可用模型，映射成公共 ID、落盘并返回（连同倍率表）。
///
/// 刻意**不**走本地反代 `/v1/models`：那条路要求代理已经在跑，于是「看看有哪些模型」
/// 这件只读的事被绑定成「先启动代理」。改成直接问上游之后，模型分区在没有代理时也能看、
/// 也能同步；顺带让启动时的后台刷新不依赖代理是否被自动拉起。
///
/// 唯一的前置条件是本机有 Kiro 凭据 —— 那是「能不能问上游」的前提，不是「代理有没有跑」。
pub(crate) fn sync_models_from_upstream() -> Result<(Vec<String>, HashMap<String, f64>), String> {
    let auth = new_auth();
    let ids = auth.list_available_models()?;
    let upstream_rates = auth.model_rates();
    let mut models: Vec<String> = ids
        .iter()
        .map(|id| kiro_model_to_public_model_id(id))
        .collect();
    models.sort();
    models.dedup();
    if models.is_empty() {
        return Err("上游没有返回任何可用模型".to_string());
    }
    // 倍率表跟着换算成公共 id：多个内部 id 折到同一公共 id 时保留先到的倍率，
    // 且只留清单里真的有的模型（上游可能给了倍率但模型已被去重/淘汰）
    let mut rates: HashMap<String, f64> = HashMap::new();
    for id in &ids {
        if let Some(rate) = upstream_rates.get(id) {
            rates
                .entry(kiro_model_to_public_model_id(id))
                .or_insert(*rate);
        }
    }
    rates.retain(|public, _| models.contains(public));
    store_models(&models, &rates)?;
    Ok((models, rates))
}

/// 同步模型并通知前端；失败只记日志（供后台刷新与代理启动后调用）。
pub(crate) fn refresh_models_and_notify() {
    match sync_models_from_upstream() {
        Ok((models, rates)) => {
            eprintln!("[proxy] 模型列表已同步: {} 个", models.len());
            notify("proxy://models-updated", ModelsState { models, rates });
        }
        Err(error) => eprintln!("[proxy] 同步模型列表失败: {error}"),
    }
}

/// 向前端广播事件。没有 AppHandle（例如单元测试里）时静默跳过。
fn notify<S: Serialize + Clone>(event: &str, payload: S) {
    if let Some(app) = crate::APP_HANDLE.get() {
        let _ = app.emit(event, payload);
    }
}

/// 额度落盘 + 推事件 + 回填 `fetchedAt`，命令与启动刷新共用这同一段。
///
/// 时间戳取落盘的那一刻（`store_usage` 的返回值）而不是「查询返回的此刻」：
/// 界面上「上次刷新」与缓存年龄说的是同一件事，两个时间源迟早会漂。
/// 写入失败时不填 `fetchedAt`，界面就不显示这一项，数字本身照常推。
fn stamp_usage_and_notify(mut info: super::auth::KiroUsageInfo) -> super::auth::KiroUsageInfo {
    match store_usage(&info) {
        Ok(stamp) => info.fetched_at = ms_to_iso(stamp),
        Err(error) => eprintln!("[proxy] 额度缓存写入失败: {error}"),
    }
    notify("proxy://usage-updated", info.clone());
    info
}

/// 应用启动时的后台静默刷新：把模型列表与账户额度更新到最新并推给界面。
///
/// 为什么放后台线程而不是让前端 await：两者都要打上游（几百毫秒到数秒），
/// 让首屏等它们会显得「应用开得慢」。缓存已经落在 `config.json` 里，界面先用旧值渲染，
/// 这里拿到新值再用事件替换 —— 用户看到的是数字自己更新了一下，而不是白屏或转圈。
///
/// 没有 Kiro 凭据时直接跳过：那不是错误，只是还没登录。
pub(crate) fn spawn_startup_refresh() {
    if !is_locally_available() {
        eprintln!("[proxy] 跳过启动刷新：本机无 Kiro 凭据");
        return;
    }
    thread::spawn(|| {
        refresh_models_and_notify();
        match new_auth().get_usage_limits() {
            // 返回值只为 `?` 的存在；事件已经推过了
            Ok(info) => drop(stamp_usage_and_notify(info)),
            Err(error) => eprintln!("[proxy] 启动刷新额度失败: {error}"),
        }
    });
}

// ── 接入配置文本 ────────────────────────────────────────────────────────

/// 该复制类型是否需要读取明文密钥。
///
/// 与前端「按钮是否可点」必须共用同一判据：地址这类不依赖密钥的复制项，
/// 在密钥缺失时也必须可用；否则会出现「按钮可点、点了报错」。
fn kind_needs_key(kind: &str) -> bool {
    matches!(
        kind,
        "api_key" | "anthropic_env" | "anthropic_json" | "openai_env" | "openai_json"
    )
}

/// 组装写入剪贴板的接入文本。
/// kind: anthropic_base_url | openai_base_url | api_key
///     | anthropic_env | anthropic_json | openai_env | openai_json
///
/// 片段里**刻意不含模型名**：本应用是代理服务，具体用哪个模型由客户端在每次请求里决定。
/// 写死一个模型会让人以为「必须用这一个」，而那只是应用在上游模型列表里顺手挑的兜底值。
pub(crate) fn format_access_text(
    kind: &str,
    anthropic_base_url: &str,
    openai_base_url: &str,
    key: &str,
) -> Result<String, String> {
    Ok(match kind.trim() {
        "anthropic_base_url" => anthropic_base_url.to_string(),
        "openai_base_url" => openai_base_url.to_string(),
        "api_key" => key.to_string(),
        // 通用 .env / shell 变量形式
        "anthropic_env" => {
            format!("ANTHROPIC_BASE_URL={anthropic_base_url}\nANTHROPIC_AUTH_TOKEN={key}")
        }
        // Claude Code settings.json 的 env 片段
        "anthropic_json" => serde_json::to_string_pretty(&serde_json::json!({
            "env": {
                "ANTHROPIC_BASE_URL": anthropic_base_url,
                "ANTHROPIC_AUTH_TOKEN": key,
            }
        }))
        .map_err(|error| format!("生成配置片段失败: {error}"))?,
        // OpenAI SDK / 兼容客户端通用变量名（openai-python、LangChain、各类 ChatBox 等）
        "openai_env" => format!("OPENAI_BASE_URL={openai_base_url}\nOPENAI_API_KEY={key}"),
        "openai_json" => serde_json::to_string_pretty(&serde_json::json!({
            "base_url": openai_base_url,
            "api_key": key,
        }))
        .map_err(|error| format!("生成配置片段失败: {error}"))?,
        other => return Err(format!("不支持的复制类型: {other}")),
    })
}

fn build_access_text(state: &ProxyStateRef, kind: &str) -> Result<String, String> {
    // 停止状态也照常复制：地址由配置端口决定（见 `build_access`），密钥持久化在
    // 配置里，两样都不依赖进程活着 —— 这道命令读的是同一份配置，不碰运行中的服务。
    let access = build_access(state);
    let key = if kind_needs_key(kind) {
        current_api_key(state).ok_or_else(|| "还没有代理密钥，请先启动一次代理生成密钥".to_string())?
    } else {
        String::new()
    };
    format_access_text(
        kind,
        &access.anthropic_base_url,
        &access.openai_base_url,
        &key,
    )
}

// ── Tauri 命令 ──────────────────────────────────────────────────────────

#[tauri::command]
pub fn k2a_status(state: State<'_, ProxyStateRef>) -> ProxyStatus {
    build_status(&state)
}

#[tauri::command]
pub fn k2a_models() -> ModelsState {
    ModelsState {
        models: cached_models(),
        rates: cached_model_rates(),
    }
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

/// 从上游同步账户可用模型（**不需要**代理在运行）。
///
/// 曾经这里要求「代理已在运行」，因为实现是去打本地反代的 `/v1/models`。
/// 但「看看有哪些模型」是只读的，把它绑在「先启动代理」上毫无必要 —— 而且代理一起
/// 就要占端口、就要替用户向上游发请求，代价远大于收益。现在直接问上游控制面。
#[tauri::command]
pub async fn k2a_sync_models() -> Result<ModelsState, String> {
    tauri::async_runtime::spawn_blocking(|| {
        sync_models_from_upstream().map(|(models, rates)| ModelsState { models, rates })
    })
    .await
    .map_err(|e| format!("同步任务失败: {e}"))?
}

/// 读取缓存的账户额度（纯本地读取，不发网络请求）。
///
/// 首屏用它把上次查询到的数值先画出来，随后 [`k2a_usage`] 的后台刷新会推来新值。
#[tauri::command]
pub fn k2a_cached_usage() -> Option<super::auth::KiroUsageInfo> {
    cached_usage()
}

#[tauri::command]
pub fn k2a_set_port(state: State<'_, ProxyStateRef>, port: u16) -> Result<u16, String> {
    if port < 1024 {
        return Err("端口需大于 1023，避免占用系统保留端口".to_string());
    }
    // 前端在运行中已禁用输入框，这里是 IPC 层的同一守卫：
    // 运行中改端口只会造成「界面显示的地址」与「服务实际监听的端口」分家。
    if is_proxy_running(&state) {
        return Err("代理运行中，端口修改需先停止代理".to_string());
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

/// 复制一段文本到系统剪贴板（当前只有「点模型名复制」在用）。
///
/// 与 [`k2a_copy_access`] 分开是刻意的：那条路径按 `kind` 在后端组装接入片段、
/// 独占明文密钥的读取；这条只把前端递过来的字符串写进剪贴板，不接受 `kind`，
/// 因此也没有办法被前端拿去绕过密钥门控。
#[tauri::command]
pub fn k2a_copy_text(text: String) -> Result<bool, String> {
    if text.is_empty() {
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
///
/// 成功即落盘：下次启动的首屏可以直接画出上次的值，不必等联网。前端因此不需要为
/// 「持久化」写任何特殊逻辑 —— 它只需要在事件到达时把值换掉。
#[tauri::command]
pub async fn k2a_usage() -> Result<super::auth::KiroUsageInfo, String> {
    let info = tauri::async_runtime::spawn_blocking(|| new_auth().get_usage_limits())
        .await
        .map_err(|e| format!("额度查询任务失败: {e}"))??;
    Ok(stamp_usage_and_notify(info))
}

/// 手动刷新共享 SSO 凭据。
#[tauri::command]
pub async fn k2a_refresh_token(app: AppHandle) -> Result<ProxyStatus, String> {
    let (refreshed, expires_at) = tauri::async_runtime::spawn_blocking(|| refresh_credentials("手动"))
        .await
        .map_err(|e| format!("凭据刷新任务失败: {e}"))??;
    if !refreshed {
        return Err(
            "未找到可刷新的 Kiro refreshToken，请先登录（凭据卡片「登录」/ Kiro IDE / `kiro-cli login`）"
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
        "sso" => active_credential_dir(),
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

    #[test]
    fn single_field_kinds() {
        assert_eq!(format_access_text("anthropic_base_url", A, O, K).unwrap(), A);
        assert_eq!(format_access_text("openai_base_url", A, O, K).unwrap(), O);
        assert_eq!(format_access_text("api_key", A, O, K).unwrap(), K);
    }

    /// 两套协议的入口地址必须不同：OpenAI 入口要带 `/openai/v1` 前缀。
    /// 若两者相同，OpenAI SDK 自带 `/v1` 会拼成 `/v1/chat/completions` 落到 Anthropic 路由上。
    #[test]
    fn two_protocol_urls_are_distinct() {
        let anthropic = format_access_text("anthropic_base_url", A, O, K).unwrap();
        let openai = format_access_text("openai_base_url", A, O, K).unwrap();
        assert_ne!(anthropic, openai);
        assert!(!anthropic.ends_with(KIRO_OPENAI_PATH_PREFIX));
        assert!(openai.ends_with(KIRO_OPENAI_PATH_PREFIX));
    }

    #[test]
    fn anthropic_env_uses_anthropic_variable_names() {
        let text = format_access_text("anthropic_env", A, O, K).unwrap();
        assert_eq!(
            text,
            "ANTHROPIC_BASE_URL=http://127.0.0.1:5680\n\
             ANTHROPIC_AUTH_TOKEN=k2a-test1234"
        );
        assert!(!text.contains("OPENAI_"));
    }

    #[test]
    fn openai_env_uses_openai_variable_names() {
        let text = format_access_text("openai_env", A, O, K).unwrap();
        assert_eq!(
            text,
            "OPENAI_BASE_URL=http://127.0.0.1:5680/openai/v1\n\
             OPENAI_API_KEY=k2a-test1234"
        );
        // 不能混入 Anthropic 变量名，否则复制到 OpenAI 客户端后不生效
        assert!(!text.contains("ANTHROPIC"));
    }

    /// 复制出来的配置片段里**不能**出现模型名：本应用不决定用哪个模型，
    /// 具体模型由客户端在每次请求里给出。写死一个名字会让人以为「必须用这一个」。
    #[test]
    fn access_snippets_never_pin_a_model() {
        for kind in [
            "anthropic_env",
            "anthropic_json",
            "openai_env",
            "openai_json",
        ] {
            let text = format_access_text(kind, A, O, K).unwrap();
            let lower = text.to_lowercase();
            assert!(
                !lower.contains("model"),
                "{kind} 不应包含任何模型字段: {text}"
            );
        }
    }

    #[test]
    fn json_snippets_are_valid_json() {
        let anthropic: serde_json::Value =
            serde_json::from_str(&format_access_text("anthropic_json", A, O, K).unwrap()).unwrap();
        assert_eq!(anthropic["env"]["ANTHROPIC_BASE_URL"], A);
        assert_eq!(anthropic["env"]["ANTHROPIC_AUTH_TOKEN"], K);

        let openai: serde_json::Value =
            serde_json::from_str(&format_access_text("openai_json", A, O, K).unwrap()).unwrap();
        assert_eq!(openai["base_url"], O);
        assert_eq!(openai["api_key"], K);
    }

    #[test]
    fn unknown_kind_is_rejected() {
        assert!(format_access_text("oauth", A, O, K).is_err());
    }

    /// 判据必须与前端一致：只有真正依赖密钥的 kind 才需要读密钥，
    /// 否则无密钥时「接口地址」按钮可点却报错。
    #[test]
    fn only_key_dependent_kinds_require_the_key() {
        for kind in ["anthropic_base_url", "openai_base_url"] {
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
    fn urls_copy_works_without_a_key() {
        assert_eq!(format_access_text("anthropic_base_url", A, O, "").unwrap(), A);
        assert_eq!(format_access_text("openai_base_url", A, O, "").unwrap(), O);
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
