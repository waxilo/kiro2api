//! Kiro 账号管理：把「凭据」抽象成可枚举、可识别、可切换的账号。
//!
//! 本应用能用的 Kiro 身份有三个来源，格式完全一致（一份令牌文件 + 可选的注册文件），
//! 区别只在**它归谁管**：
//!
//! | 来源 | 目录 | 谁在续期 |
//! |------|------|----------|
//! | 登录的账号 | `数据目录/accounts/login/<hash>/`（旧版是 `数据目录/credentials/`） | 本应用 |
//! | 本地 kiro-cli | `数据目录/accounts/cli/<hash>/` + **回写** CLI 原库 | 本应用（两边共用一份） |
//! | Kiro 桌面端 | `~/.aws/sso/cache/`（就地使用） | IDE（本应用不代续） |
//!
//! 账号 = 一个凭据目录 + 一个稳定 id。选中某个账号，就是让代理去读那个目录；
//! 切换时代理按原端口重启，并清掉属于上一个账号的模型 / 额度缓存。
//!
//! 续期只有一个入口 [`refresh`]：代理循环、保活巡检、手动按钮都走它，副作用完全一致。
//! **未选中的账号也会被静默续期**（[`spawn_keepalive`]，30 分钟一轮）—— 否则它们会在
//! 一段时间不用之后随 refreshToken / 注册文件过期，切过去就得重新登录。
//!
//! 三条判断规则，改代码前先看一遍：
//! 1. **id 必须跨续期稳定**：令牌刷新会换 accessToken / refreshToken，id 只能由
//!    目录名或 clientIdHash 这类不变的东西推导，否则「选中的账号」一刷新就丢了。
//! 2. **桌面端绝不复制、也绝不代续**：与 IDE 共用同一个文件，双方看到同一份刷新结果；
//!    复制成独立副本或替它续期，都会让另一方手里的 refreshToken 失效。
//! 3. **CLI 以原库为准**：判断到期读 CLI 库，续期结果写回 CLI 库，副本只是同一份数据的
//!    文件形态投影。只动副本会让两边各持一个 refreshToken，谁轮换谁把对方顶死。

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager};

use crate::kiro::cli_bridge::{self, CliCredential};
use crate::kiro::service::{self, KiroProxyState, ProxyStatus};
use crate::paths;
use crate::prefs;

/// 规范令牌文件名，与 `~/.aws/sso/cache` 一致（`Auth` 只认这个名字）。
const TOKEN_FILE: &str = "kiro-auth-token.json";
/// 账号目录里的自述文件：来源、创建时间、CLI 副本的出处。
const META_FILE: &str = "account.json";
/// 距过期不足这个秒数就当作「要续期了」，与 `auth` / `cli_bridge` 的口径一致。
const USABLE_SKEW_SECS: i64 = 120;

/// 保活巡检间隔：账号的 accessToken 约 1 小时，30 分钟一轮才能在它死掉之前接上。
const KEEPALIVE_INTERVAL_SECS: u64 = 30 * 60;
/// 首轮巡检的延迟：避开启动时那次模型 / 额度刷新（两者会争同一份凭据的刷新）。
const KEEPALIVE_FIRST_DELAY_SECS: u64 = 60;
/// 距过期不足这么久就续（= 巡检间隔 + 15 分钟余量：下一轮巡检时 token 仍未过期）。
const RENEW_BEFORE_SECS: i64 = 45 * 60;

/// 账号来源。序列化成 `login` / `cli` / `desktop`，前端据此分组。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AccountSource {
    /// kiro2api 自己发起的设备码登录。
    Login,
    /// 从本机 kiro-cli 凭据库导入。
    Cli,
    /// Kiro 桌面端（IDE）的共享缓存。
    Desktop,
}

impl AccountSource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Login => "login",
            Self::Cli => "cli",
            Self::Desktop => "desktop",
        }
    }
}

/// 一个账号的展示信息。**不含任何令牌内容** —— 前端只需要知道「是谁、还能用多久」。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountInfo {
    pub id: String,
    pub source: AccountSource,
    /// 展示主标题：识别到邮箱就是邮箱，否则是「Builder ID 账号 · 3f2a」这类占位。
    pub label: String,
    pub email: Option<String>,
    /// 套餐名（上游 `subscriptionTitle`），识别过才有。
    pub plan: Option<String>,
    pub provider: Option<String>,
    pub auth_method: Option<String>,
    pub region: Option<String>,
    pub expires_at: Option<String>,
    pub profile_arn: Option<String>,
    /// 凭据目录（CLI 账号给的是副本目录，不是 sqlite 路径）。
    pub path: String,
    /// 当前代理正在用的就是这个账号。
    pub selected: bool,
    /// 现在就能用：访问令牌未过期，或持有可续期的 refreshToken（IdC 还要求注册文件有效）。
    pub usable: bool,
    /// 能否在本应用里移除。只有「登录的账号」可以 —— 桌面端归 IDE、CLI 归 kiro-cli，
    /// 在这边删掉也会被下一次扫描原样扫回来。
    pub removable: bool,
    /// 补充说明（目前用于「与 kiro-cli 同账号」这类去重提示）。
    pub note: Option<String>,
}

/// `k2a_accounts_list` 的返回。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountsView {
    pub accounts: Vec<AccountInfo>,
    /// 实际生效的账号 id（配置里选中的那个；没选过就是自动挑出来的那个）。
    pub active_id: Option<String>,
}

/// 账号目录里的自述文件。
///
/// 只记「怎么来的」这类目录本身说不清的事；账号身份（服务商 / 区域 / 到期）一律以
/// 令牌文件为准，避免两份数据互相漂移。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct AccountMeta {
    created_at_ms: i64,
    /// CLI 副本：来源库路径与导入时的 mtime，用来判断「CLI 是否重新登录过」。
    db_path: Option<String>,
    db_mtime_ms: Option<i64>,
}

/// 从令牌 + 注册文件里读出的事实。纯数据，便于单测。
#[derive(Debug, Clone, Default)]
struct TokenFacts {
    auth_method: Option<String>,
    provider: Option<String>,
    region: Option<String>,
    expires_at: Option<String>,
    profile_arn: Option<String>,
    /// IdC 注册文件的 clientId 哈希；同时是跨来源去重的钥匙。
    identity: Option<String>,
    has_refresh: bool,
    access_fresh: bool,
    registration_ok: bool,
}

impl TokenFacts {
    fn from_token(token: &Value, registration: Option<&Value>) -> Self {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let expiry_ms = token
            .get("expiresAt")
            .and_then(Value::as_str)
            .and_then(crate::kiro::auth::parse_expiry_ms);
        let access_fresh = expiry_ms.is_some_and(|ms| ms > now_ms + USABLE_SKEW_SECS * 1000);
        let identity = token
            .get("clientIdHash")
            .and_then(Value::as_str)
            .filter(|hash| !hash.is_empty() && hash.chars().all(|c| c.is_ascii_hexdigit()))
            .map(str::to_string);
        let has_refresh = token
            .get("refreshToken")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty());
        let auth_method = token
            .get("authMethod")
            .and_then(Value::as_str)
            .map(str::to_string);
        // IdC 续期要 clientId/clientSecret：没有有效的注册文件，refreshToken 也刷不动。
        let needs_registration = auth_method.as_deref() == Some("IdC")
            || (auth_method.is_none() && identity.is_some());
        let registration_ok = !needs_registration
            || registration
                .and_then(|reg| reg.get("expiresAt"))
                .and_then(Value::as_str)
                .and_then(crate::kiro::auth::parse_expiry_ms)
                .is_some_and(|ms| ms > now_ms);

        Self {
            auth_method,
            provider: token.get("provider").and_then(Value::as_str).map(str::to_string),
            region: token.get("region").and_then(Value::as_str).map(str::to_string),
            expires_at: token
                .get("expiresAt")
                .and_then(Value::as_str)
                .map(str::to_string),
            profile_arn: token
                .get("profileArn")
                .and_then(Value::as_str)
                .map(str::to_string),
            identity,
            has_refresh,
            access_fresh,
            registration_ok,
        }
    }

    fn usable(&self) -> bool {
        self.access_fresh || (self.has_refresh && self.registration_ok)
    }
}

/// CLI 副本的来源信息：库路径 + 键 + 导入时的 mtime。
#[derive(Debug, Clone)]
struct CliOrigin {
    db: PathBuf,
    key: String,
    mtime_ms: i64,
}

/// 扫描出来的一个候选账号（内部用，尚未带「选中 / 可移除」这类结论）。
#[derive(Debug, Clone)]
struct Candidate {
    id: String,
    source: AccountSource,
    dir: PathBuf,
    facts: TokenFacts,
    cli: Option<CliOrigin>,
    note: Option<String>,
}

// ── 对外入口 ────────────────────────────────────────────────────────────

/// 列出全部账号（纯本地读取，不发网络请求）。
pub(crate) fn list() -> AccountsView {
    let (candidates, active) = resolve(discover(), prefs::active_account_pref().as_deref());
    let identities = prefs::account_identities();
    AccountsView {
        accounts: candidates
            .iter()
            .map(|candidate| to_info(candidate, active.as_deref(), identities.get(&candidate.id)))
            .collect(),
        active_id: active,
    }
}

/// 当前生效的账号（不含路径物化，适合状态轮询）。
pub(crate) fn active_account() -> Option<AccountInfo> {
    let (candidates, active) = resolve(discover(), prefs::active_account_pref().as_deref());
    let id = active?;
    let candidate = candidates.iter().find(|candidate| candidate.id == id)?;
    let identities = prefs::account_identities();
    Some(to_info(candidate, Some(&id), identities.get(&id)))
}

/// 当前生效的凭据目录。CLI 账号会在这里按需（重新）导入副本。
pub(crate) fn active_dir() -> Option<PathBuf> {
    let (candidates, active) = resolve(discover(), prefs::active_account_pref().as_deref());
    let id = active?;
    let candidate = candidates.iter().find(|candidate| candidate.id == id)?;
    materialize(candidate).ok()
}

/// 本机有没有可用凭据（与旧的 `has_credential_file` 同义）。
///
/// 纯判断，不落盘：状态轮询每秒都可能走到这里，不该顺手往磁盘里写 CLI 副本。
/// 真要读凭据时（启动代理 / 识别账号）才由 [`active_dir`] 物化。
pub(crate) fn has_credential() -> bool {
    let (candidates, active) = resolve(discover(), prefs::active_account_pref().as_deref());
    let Some(id) = active else {
        return false;
    };
    candidates
        .iter()
        .find(|candidate| candidate.id == id)
        .is_some_and(|candidate| candidate.facts.usable())
}

/// 当前生效的账号 id（与 [`active_dir`] 同一口径，但不物化、不落盘）。
pub(crate) fn active_id() -> Option<String> {
    resolve(discover(), prefs::active_account_pref().as_deref()).1
}

// ── 续期（唯一入口） ────────────────────────────────────────────────────

/// 谁在发起续期。只有「用户明确点了按钮」才无视剩余有效期。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefreshMode {
    /// 代理运行中的定时续期。
    Loop,
    /// 账号保活巡检：未选中的账号也走这条。
    KeepAlive,
    /// 手动「刷新凭据」：用户要的就是现在换一份新的。
    Manual,
}

impl RefreshMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Loop => "代理循环",
            Self::KeepAlive => "保活",
            Self::Manual => "手动",
        }
    }

    /// 剩余有效期还够时要不要跳过。手动必须真的发请求，否则按钮看起来「没反应」。
    fn respects_threshold(self) -> bool {
        !matches!(self, Self::Manual)
    }
}

/// 跳过一次续期的原因。全部是正常情况，不是错误 —— 调用方据此决定说哪句话。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Skip {
    /// 账号已不存在（被删、或这次扫描不到）。
    Gone,
    /// 没有 refreshToken：只能等用户重新登录。
    NoRefreshToken,
    /// IdC 的注册文件缺失或已过期：refreshToken 也刷不动。
    NoRegistration,
    /// 剩余有效期还超过阈值。
    StillFresh,
    /// 桌面端与 IDE 共用一份文件，代续会互相顶掉。
    SharedWithIde,
    /// 同一账号已有一轮续期在跑。
    Concurrent,
}

impl Skip {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Gone => "账号已不存在",
            Self::NoRefreshToken => "没有 refreshToken",
            Self::NoRegistration => "注册文件缺失或已过期",
            Self::StillFresh => "剩余有效期仍充足",
            Self::SharedWithIde => "桌面端账号由 IDE 续期",
            Self::Concurrent => "该账号正在续期",
        }
    }
}

/// 一次续期的结果。`Skipped` 也带着当前的到期时间，调用方刷新界面时不用再读一遍文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RefreshOutcome {
    Refreshed { expires_at: Option<String> },
    Skipped { reason: Skip, expires_at: Option<String> },
}

impl RefreshOutcome {
    pub(crate) fn expires_at(&self) -> Option<String> {
        match self {
            Self::Refreshed { expires_at } | Self::Skipped { expires_at, .. } => expires_at.clone(),
        }
    }

    pub(crate) fn is_refreshed(&self) -> bool {
        matches!(self, Self::Refreshed { .. })
    }
}

/// 续期一个账号。**唯一**的续期入口：代理循环、保活巡检、手动按钮都从这里走。
pub(crate) fn refresh(id: &str, mode: RefreshMode) -> Result<RefreshOutcome, String> {
    let (candidates, _) = resolve(discover(), prefs::active_account_pref().as_deref());
    let Some(candidate) = candidates.iter().find(|candidate| candidate.id == id) else {
        return Ok(RefreshOutcome::Skipped { reason: Skip::Gone, expires_at: None });
    };
    refresh_candidate(candidate, mode)
}

/// 对一个已扫描到的候选账号执行续期。
///
/// 四层闸门，顺序不能换：
/// 1. 桌面端直接退出（连读都不用读）；
/// 2. 先把 CLI 副本与**原库**对齐：副本是库的投影，留着「库里新鲜、副本过期」时跳过续期，
///    代理读的正是副本 —— 它会把一次本来能用的请求变成 401；
/// 3. 原库 / 令牌的到期时间还够 → 不发请求；
/// 4. 同一账号串行 + **锁内重新读一次**事实源：并发时后到的那轮看到「刚续过」自己退出，
///    避免两轮各拿一个 refreshToken 去轮换（必然有一轮失败，还会把对方顶掉）。
fn refresh_candidate(
    candidate: &Candidate,
    mode: RefreshMode,
) -> Result<RefreshOutcome, String> {
    if candidate.source == AccountSource::Desktop {
        return Ok(RefreshOutcome::Skipped {
            reason: Skip::SharedWithIde,
            expires_at: candidate.facts.expires_at.clone(),
        });
    }

    let dir = materialize(candidate)?;
    let Some(facts) = source_facts(candidate)? else {
        return Ok(RefreshOutcome::Skipped { reason: Skip::Gone, expires_at: None });
    };
    if let Some(reason) = renewal_gate(&facts, mode, now_ms()) {
        return Ok(RefreshOutcome::Skipped { reason, expires_at: facts.expires_at });
    }

    let Some(_in_flight) = InFlight::acquire(&candidate.id) else {
        return Ok(RefreshOutcome::Skipped {
            reason: Skip::Concurrent,
            expires_at: facts.expires_at,
        });
    };
    let Some(facts) = source_facts(candidate)? else {
        return Ok(RefreshOutcome::Skipped { reason: Skip::Gone, expires_at: None });
    };
    if let Some(reason) = renewal_gate(&facts, mode, now_ms()) {
        return Ok(RefreshOutcome::Skipped { reason, expires_at: facts.expires_at });
    }

    if let Some(origin) = &candidate.cli {
        let (_, expires_at) = service::auth_for(dir.clone()).force_refresh(mode.as_str())?;
        // 续期结果写回 CLI 原库：两边共用一份，否则 CLI 侧那份 refreshToken 就此作废
        let refreshed = read_json(&dir.join(TOKEN_FILE))
            .ok_or_else(|| "续期后读不到令牌文件，请刷新后重试".to_string())?;
        cli_bridge::write_token_fields(&origin.db, &origin.key, &refreshed)?;
        sync_copy_meta(&dir, origin)?;
        return Ok(RefreshOutcome::Refreshed { expires_at });
    }

    let (refreshed, expires_at) = service::auth_for(dir).force_refresh(mode.as_str())?;
    if !refreshed {
        return Ok(RefreshOutcome::Skipped { reason: Skip::NoRefreshToken, expires_at });
    }
    Ok(RefreshOutcome::Refreshed { expires_at })
}

/// 事实源里「现在这份凭据」是什么样。
///
/// CLI 账号以**原库**为准：副本是上一轮的投影，拿它判断会出现「库里刚续过、副本还旧」
/// → 多续一次 → 用已轮换的 refreshToken 把 CLI 顶死。
fn source_facts(candidate: &Candidate) -> Result<Option<TokenFacts>, String> {
    if let Some(origin) = &candidate.cli {
        let Some(credential) = cli_bridge::read_one(&origin.db, Some(&origin.key))? else {
            return Ok(None);
        };
        return Ok(Some(TokenFacts::from_token(
            &credential.token,
            credential.registration.as_ref().map(|(_, value)| value),
        )));
    }
    Ok(read_dir_facts(&candidate.dir))
}

/// 这一轮该不该真的发请求。`None` = 该续；`Some(reason)` = 跳过。
///
/// 注册文件过期也必须挡在发请求之前：IdC 续期要 clientId/clientSecret，没有它必然失败，
/// 白白等满一次 30 秒超时 —— 保活每 30 分钟跑一轮，这种等待会一直拖住后面的账号。
fn renewal_gate(facts: &TokenFacts, mode: RefreshMode, now_ms: i64) -> Option<Skip> {
    if !facts.has_refresh {
        return Some(Skip::NoRefreshToken);
    }
    if !facts.registration_ok {
        return Some(Skip::NoRegistration);
    }
    if mode.respects_threshold() && !needs_renewal(facts.expires_at.as_deref(), now_ms) {
        return Some(Skip::StillFresh);
    }
    None
}

/// 到期时间还值不值得等：剩余不足 [`RENEW_BEFORE_SECS`] 就续。
///
/// `expiresAt` 缺失或不可解析时返回 `true`：宁可白续一次，也不能让账号悄悄过期。
fn needs_renewal(expires_at: Option<&str>, now_ms: i64) -> bool {
    let Some(expires_ms) = expires_at.and_then(crate::kiro::auth::parse_expiry_ms) else {
        return true;
    };
    expires_ms - now_ms <= RENEW_BEFORE_SECS * 1000
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// 正在续期的账号 id 集合：同一账号同时只允许一轮续期。
fn in_flight() -> &'static Mutex<HashSet<String>> {
    static IN_FLIGHT: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    IN_FLIGHT.get_or_init(|| Mutex::new(HashSet::new()))
}

/// 续期期间的占位符；`Drop` 时自动出列（含 panic 展开），不会把账号永久锁死。
struct InFlight(String);

impl InFlight {
    fn acquire(id: &str) -> Option<Self> {
        let mut set = in_flight().lock().ok()?;
        if !set.insert(id.to_string()) {
            return None;
        }
        Some(Self(id.to_string()))
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        if let Ok(mut set) = in_flight().lock() {
            set.remove(&self.0);
        }
    }
}

/// 回写原库之后把副本的「已同步」标记刷新一遍。
///
/// `materialize` 靠 `db_mtime_ms` 判断要不要重新导入；不回填的话，回写改出来的新 mtime
/// 会让下一次扫描把刚续好的副本再覆盖一遍 —— 内容相同，纯属多一轮 IO。
fn sync_copy_meta(dir: &Path, origin: &CliOrigin) -> Result<(), String> {
    let created_at_ms = read_meta(dir)
        .map(|meta| meta.created_at_ms)
        .filter(|value| *value > 0)
        .unwrap_or_else(now_ms);
    write_meta(
        dir,
        AccountMeta {
            created_at_ms,
            db_path: Some(origin.db.to_string_lossy().to_string()),
            db_mtime_ms: Some(cli_bridge::file_mtime_ms(&origin.db)),
        },
    )
}

/// 后台保活：每 [`KEEPALIVE_INTERVAL_SECS`] 轮一遍所有账号，把快过期的静默续上。
///
/// 与代理生命周期**解耦**：以前只有代理在跑时才续、而且只续选中的那一个，于是没被选中的
/// 账号会一直放到 refreshToken / 注册文件过期，切过去就得重新登录。
///
/// 静默是刻意的：用户没发起这个动作，失败只写 `kiro-debug.log`；真的续上了才发一次
/// `accounts://refreshed`，让账号页的有效期自己变新。
pub(crate) fn spawn_keepalive() {
    if crate::APP_HANDLE.get().is_none() {
        return; // 单测里没有 AppHandle，不启线程
    }
    thread::spawn(|| {
        thread::sleep(Duration::from_secs(KEEPALIVE_FIRST_DELAY_SECS));
        loop {
            let renewed = keepalive_round();
            if renewed > 0 {
                if let Some(app) = crate::APP_HANDLE.get() {
                    let _ = app.emit("accounts://refreshed", renewed);
                }
            }
            thread::sleep(Duration::from_secs(KEEPALIVE_INTERVAL_SECS));
        }
    });
}

/// 走一轮巡检，返回真的续上的账号数。
fn keepalive_round() -> usize {
    let (candidates, _) = resolve(discover(), prefs::active_account_pref().as_deref());
    let mut renewed = 0;
    for candidate in &candidates {
        match refresh_candidate(candidate, RefreshMode::KeepAlive) {
            Ok(RefreshOutcome::Refreshed { expires_at }) => {
                renewed += 1;
                crate::diag::log(&format!(
                    "[keepalive] 已续期 {}，新到期 {}",
                    candidate.id,
                    expires_at.as_deref().unwrap_or("—")
                ));
            }
            Ok(RefreshOutcome::Skipped { reason, expires_at }) => crate::diag::log(&format!(
                "[keepalive] 跳过 {}：{}（到期 {}）",
                candidate.id,
                reason.as_str(),
                expires_at.as_deref().unwrap_or("—")
            )),
            Err(error) => {
                crate::diag::log(&format!("[keepalive] {} 续期失败：{error}", candidate.id))
            }
        }
    }
    renewed
}

/// 选中一个账号。会先把凭据准备好（CLI 立即导入副本），再落盘选择并清掉旧缓存。
pub(crate) fn select(id: &str) -> Result<(), String> {
    let (candidates, _) = resolve(discover(), prefs::active_account_pref().as_deref());
    let candidate = candidates
        .iter()
        .find(|candidate| candidate.id == id)
        .ok_or_else(|| "账号不存在或已失效，请刷新后重试".to_string())?;
    materialize(candidate)?;
    prefs::store_active_account(Some(id))?;
    prefs::clear_account_caches()?;
    Ok(())
}

/// 移除一个账号。**只允许登录账号**：桌面端与 CLI 账号分别归 IDE / kiro-cli 管，
/// 在这里删掉目录既不会让它们退出登录，下一次扫描又会原样出现 —— 那只会骗人。
pub(crate) fn remove(id: &str) -> Result<(), String> {
    let (candidates, _) = resolve(discover(), prefs::active_account_pref().as_deref());
    let candidate = candidates
        .iter()
        .find(|candidate| candidate.id == id)
        .ok_or_else(|| "账号不存在或已失效，请刷新后重试".to_string())?;
    match candidate.source {
        AccountSource::Desktop => {
            return Err(
                "Kiro 桌面端账号由 IDE 管理，请先在 IDE 内退出登录，再回本页刷新".to_string(),
            )
        }
        AccountSource::Cli => {
            return Err(
                "kiro-cli 账号由 kiro-cli 管理，请执行 kiro-cli logout 后再回本页刷新".to_string(),
            )
        }
        AccountSource::Login => {}
    }

    if candidate.dir == paths::own_credential_dir() {
        // 旧版自有凭据目录：只删令牌与它引用的注册文件，目录本身留着（可能还有别的文件）
        let token = read_json(&candidate.dir.join(TOKEN_FILE));
        if let Some(hash) = token
            .as_ref()
            .and_then(|token| token.get("clientIdHash"))
            .and_then(Value::as_str)
            .filter(|hash| hash.chars().all(|c| c.is_ascii_hexdigit()))
        {
            let _ = fs::remove_file(candidate.dir.join(format!("{hash}.json")));
        }
        let token_path = candidate.dir.join(TOKEN_FILE);
        if token_path.exists() {
            fs::remove_file(&token_path).map_err(|e| format!("删除凭据失败：{e}"))?;
        }
    } else if candidate.dir.exists() {
        fs::remove_dir_all(&candidate.dir).map_err(|e| format!("删除账号目录失败：{e}"))?;
    }

    if prefs::active_account_pref().as_deref() == Some(id) {
        prefs::store_active_account(None)?;
    }
    prefs::clear_account_caches()?;
    crate::diag::log(&format!("[accounts] 已移除账号 {id}"));
    Ok(())
}

/// 联网识别一个账号的邮箱与套餐（走 getUsageLimits），结果落进身份缓存。
pub(crate) fn identify(id: &str) -> Result<AccountInfo, String> {
    let (candidates, active) = resolve(discover(), prefs::active_account_pref().as_deref());
    let candidate = candidates
        .iter()
        .find(|candidate| candidate.id == id)
        .ok_or_else(|| "账号不存在或已失效，请刷新后重试".to_string())?;
    let dir = materialize(candidate)?;
    let info = service::auth_for(dir).get_usage_limits()?;
    prefs::store_account_identity(id, info.email.as_deref(), info.subscription_title.as_deref())?;
    let identities = prefs::account_identities();
    Ok(to_info(candidate, active.as_deref(), identities.get(id)))
}

/// 账号来源的一句话说明（`ProxyStatus.authSource` 用）。
pub(crate) fn describe(account: &AccountInfo) -> String {
    match account.source {
        AccountSource::Login => format!(
            "Kiro2API 登录（{}）",
            provider_label(account.provider.as_deref(), account.auth_method.as_deref())
        ),
        AccountSource::Cli => "kiro-cli（本机凭据库）".to_string(),
        AccountSource::Desktop => "Kiro 桌面端（IDE 共用缓存）".to_string(),
    }
}

/// 新登录账号的目录：`accounts/login/<clientId 哈希>`。
pub(crate) fn login_dir(hash: &str) -> PathBuf {
    paths::accounts_root().join("login").join(hash)
}

/// 登录成功后补一份自述文件（记录创建时刻，便于将来排查「这是哪次登录」）。
pub(crate) fn record_login(dir: &Path) -> Result<(), String> {
    write_meta(
        dir,
        AccountMeta {
            created_at_ms: chrono::Utc::now().timestamp_millis(),
            ..Default::default()
        },
    )
}

// ── 扫描 ────────────────────────────────────────────────────────────────

fn discover() -> Vec<Candidate> {
    candidates(
        &paths::accounts_root(),
        &paths::own_credential_dir(),
        &paths::kiro_sso_cache_dir(),
        &cli_bridge::scan(),
    )
}

/// 扫描四类位置，产出候选账号（顺序 = 自动选择的优先级：登录 > 桌面端 > CLI）。
fn candidates(
    root: &Path,
    legacy_login: &Path,
    desktop: &Path,
    cli: &[CliCredential],
) -> Vec<Candidate> {
    let mut list: Vec<Candidate> = Vec::new();

    // 旧版自有登录（0.11 及以前只有这一个位置）
    if let Some(candidate) = from_dir(AccountSource::Login, "login:legacy", legacy_login) {
        list.push(candidate);
    }

    // 新版：每个登录账号一个目录，目录名即 clientId 哈希
    let mut login_dirs: Vec<PathBuf> = fs::read_dir(root.join("login"))
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.is_dir())
                .collect()
        })
        .unwrap_or_default();
    login_dirs.sort();
    for dir in login_dirs {
        let Some(name) = dir.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if let Some(candidate) = from_dir(AccountSource::Login, &format!("login:{name}"), &dir) {
            list.push(candidate);
        }
    }

    // Kiro 桌面端：就地使用共享缓存
    if let Some(candidate) = from_dir(AccountSource::Desktop, "desktop:default", desktop) {
        list.push(candidate);
    }

    // kiro-cli：列出即可，副本等到真正选中时才写
    for credential in cli {
        let suffix = cli_suffix(credential);
        let dir = root.join("cli").join(&suffix);
        list.push(Candidate {
            id: format!("cli:{suffix}"),
            source: AccountSource::Cli,
            dir,
            facts: TokenFacts::from_token(
                &credential.token,
                credential.registration.as_ref().map(|(_, value)| value),
            ),
            cli: Some(CliOrigin {
                db: credential.db.clone(),
                key: credential.key.clone(),
                mtime_ms: credential.db_mtime_ms,
            }),
            note: None,
        });
    }

    mark_shared_identity(list)
}

fn from_dir(source: AccountSource, id: &str, dir: &Path) -> Option<Candidate> {
    Some(Candidate {
        id: id.to_string(),
        source,
        dir: dir.to_path_buf(),
        facts: read_dir_facts(dir)?,
        cli: None,
        note: None,
    })
}

/// CLI 副本的目录名 / id 后缀：由库路径与来源键推导，CLI 重新登录也不变。
fn cli_suffix(credential: &CliCredential) -> String {
    let mut hasher = Sha256::new();
    hasher.update(credential.db.to_string_lossy().as_bytes());
    hasher.update(b"\0");
    hasher.update(credential.key.as_bytes());
    format!("{:x}", hasher.finalize())[..12].to_string()
}

/// 同一个 IdC 账号可能同时躺在桌面端缓存（以前从 CLI 桥接过去）与 CLI 库里。
///
/// 这里**不合并**它们 —— 用户要看的就是「登录 / kiro-cli / 桌面端」三者的区别，
/// 合并等于把其中一个来源藏起来。只在两边都有的那条上加一句说明，让人知道
/// 「选哪个都行，它们是同一个账号，只是归不同客户端管」。
fn mark_shared_identity(mut list: Vec<Candidate>) -> Vec<Candidate> {
    for left in 0..list.len() {
        for right in (left + 1)..list.len() {
            let same_identity = match (&list[left].facts.identity, &list[right].facts.identity) {
                (Some(left_id), Some(right_id)) => left_id == right_id,
                _ => false,
            };
            if !same_identity || !is_cli_desktop_pair(list[left].source, list[right].source) {
                continue;
            }
            let left_note = format!("同一账号也在{}", source_label(list[right].source));
            let right_note = format!("同一账号也在{}", source_label(list[left].source));
            if list[left].note.is_none() {
                list[left].note = Some(left_note);
            }
            if list[right].note.is_none() {
                list[right].note = Some(right_note);
            }
        }
    }
    list
}

fn source_label(source: AccountSource) -> &'static str {
    match source {
        AccountSource::Login => "登录的账号",
        AccountSource::Cli => "kiro-cli",
        AccountSource::Desktop => "Kiro 桌面端",
    }
}

fn is_cli_desktop_pair(left: AccountSource, right: AccountSource) -> bool {
    matches!(
        (left, right),
        (AccountSource::Desktop, AccountSource::Cli) | (AccountSource::Cli, AccountSource::Desktop)
    )
}

/// 选中谁：配置里选中的优先；它不存在（被删 / 换机器）时按「登录 > 桌面端 > CLI」
/// 挑第一个可用的，都不「可用」则退回第一个 —— 界面上总得有个东西可展示。
fn resolve(
    list: Vec<Candidate>,
    configured: Option<&str>,
) -> (Vec<Candidate>, Option<String>) {
    if let Some(id) = configured {
        if list.iter().any(|candidate| candidate.id == id) {
            return (list, Some(id.to_string()));
        }
    }
    for source in [AccountSource::Login, AccountSource::Desktop, AccountSource::Cli] {
        let found = list
            .iter()
            .find(|candidate| candidate.source == source && candidate.facts.usable())
            .map(|candidate| candidate.id.clone());
        if let Some(id) = found {
            return (list, Some(id));
        }
    }
    let fallback = list.first().map(|candidate| candidate.id.clone());
    (list, fallback)
}

// ── 物化 / 展示 ────────────────────────────────────────────────────────

/// 确保账号目录里有一份可用凭据。CLI 副本按库 mtime 判断是否需要重新导入。
fn materialize(candidate: &Candidate) -> Result<PathBuf, String> {
    let Some(origin) = &candidate.cli else {
        if candidate.dir.join(TOKEN_FILE).is_file() {
            return Ok(candidate.dir.clone());
        }
        return Err("账号凭据已不存在，请刷新后重试".to_string());
    };

    let meta = read_meta(&candidate.dir);
    let db_path = origin.db.to_string_lossy().to_string();
    let fresh = meta.as_ref().is_some_and(|meta| {
        meta.db_path.as_deref() == Some(db_path.as_str())
            && meta.db_mtime_ms == Some(origin.mtime_ms)
            && candidate.dir.join(TOKEN_FILE).is_file()
    });
    if fresh {
        return Ok(candidate.dir.clone());
    }

    let credential = cli_bridge::read_one(&origin.db, Some(&origin.key))?
        .ok_or_else(|| "kiro-cli 的凭据已失效，请在 kiro-cli 中重新登录".to_string())?;
    cli_bridge::import(&credential, &candidate.dir)?;
    write_meta(
        &candidate.dir,
        AccountMeta {
            created_at_ms: chrono::Utc::now().timestamp_millis(),
            db_path: Some(db_path),
            db_mtime_ms: Some(credential.db_mtime_ms),
        },
    )?;
    crate::diag::log(&format!(
        "[accounts] 已从 kiro-cli 导入账号副本 {}",
        candidate.dir.display()
    ));
    Ok(candidate.dir.clone())
}

fn to_info(
    candidate: &Candidate,
    active: Option<&str>,
    identity: Option<&prefs::AccountIdentity>,
) -> AccountInfo {
    let email = identity.and_then(|identity| identity.email.clone());
    let label = email.clone().unwrap_or_else(|| {
        let provider = provider_label(
            candidate.facts.provider.as_deref(),
            candidate.facts.auth_method.as_deref(),
        );
        match short_id(&candidate.id) {
            Some(suffix) => format!("{provider} · {suffix}"),
            None => provider.to_string(),
        }
    });
    AccountInfo {
        id: candidate.id.clone(),
        source: candidate.source,
        label,
        email,
        plan: identity.and_then(|identity| identity.plan.clone()),
        provider: candidate.facts.provider.clone(),
        auth_method: candidate.facts.auth_method.clone(),
        region: candidate.facts.region.clone(),
        expires_at: candidate.facts.expires_at.clone(),
        profile_arn: candidate.facts.profile_arn.clone(),
        path: candidate.dir.display().to_string(),
        selected: active == Some(candidate.id.as_str()),
        usable: candidate.facts.usable(),
        removable: candidate.source == AccountSource::Login,
        note: candidate.note.clone(),
    }
}

fn provider_label(provider: Option<&str>, auth_method: Option<&str>) -> &'static str {
    match provider {
        Some("BuilderId") => "Builder ID 账号",
        Some("Enterprise") => "IAM Identity Center 账号",
        _ if auth_method == Some("social") => "Kiro 账号",
        _ => "IdC 账号",
    }
}

/// 占位名的尾缀：多账号时用来区分同服务商的两个账号。
///
/// `desktop:default` 与旧的 `login:legacy` 没有区分意义（各自只有一条），
/// 取尾字母只会得到「ault」「gacy」这种看不懂的东西，因此返回 `None`。
fn short_id(id: &str) -> Option<&str> {
    let tail = id.rsplit(':').next().unwrap_or(id);
    if tail == "default" || tail == "legacy" {
        return None;
    }
    Some(if tail.len() > 4 { &tail[tail.len() - 4..] } else { tail })
}

// ── 文件 ────────────────────────────────────────────────────────────────

fn read_json(path: &Path) -> Option<Value> {
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

/// 从凭据目录里读出事实（令牌 + 它引用的注册文件）。目录里没有令牌 → `None`。
///
/// 扫描与续期共用这一份口径：判断「能不能续、还差多久到期」必须看同一处数据。
fn read_dir_facts(dir: &Path) -> Option<TokenFacts> {
    let token = read_json(&dir.join(TOKEN_FILE))?;
    let registration = token
        .get("clientIdHash")
        .and_then(Value::as_str)
        .and_then(|hash| read_json(&dir.join(format!("{hash}.json"))));
    Some(TokenFacts::from_token(&token, registration.as_ref()))
}

fn read_meta(dir: &Path) -> Option<AccountMeta> {
    let raw = fs::read_to_string(dir.join(META_FILE)).ok()?;
    serde_json::from_str(&raw).ok()
}

fn write_meta(dir: &Path, meta: AccountMeta) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| format!("创建账号目录失败：{e}"))?;
    let payload = serde_json::to_value(&meta).map_err(|e| format!("序列化账号信息失败：{e}"))?;
    cli_bridge::write_secret_file(&dir.join(META_FILE), &payload)
}

// ── Tauri 命令 ──────────────────────────────────────────────────────────

#[tauri::command]
pub fn k2a_accounts_list() -> AccountsView {
    list()
}

/// 切换账号：落盘 → 清掉旧账号的模型/额度缓存 → 按原端口重启代理 → 后台刷新。
#[tauri::command]
pub async fn k2a_account_select(app: AppHandle, id: String) -> Result<ProxyStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        select(&id)?;
        service::restart_proxy_if_running(&app);
        service::spawn_startup_refresh();
        let state = app.state::<KiroProxyState>();
        Ok(service::build_status(&state))
    })
    .await
    .map_err(|e| format!("切换账号任务失败：{e}"))?
}

/// 移除账号（登录账号 / CLI 副本）。正在使用时按原端口切到下一条可用账号。
#[tauri::command]
pub async fn k2a_account_remove(app: AppHandle, id: String) -> Result<ProxyStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        remove(&id)?;
        service::restart_proxy_if_running(&app);
        service::spawn_startup_refresh();
        let state = app.state::<KiroProxyState>();
        Ok(service::build_status(&state))
    })
    .await
    .map_err(|e| format!("移除账号任务失败：{e}"))?
}

/// 识别账号：联网取邮箱与套餐，写进身份缓存。失败只影响这一行，界面给 toast。
#[tauri::command]
pub async fn k2a_account_identify(id: String) -> Result<AccountInfo, String> {
    tauri::async_runtime::spawn_blocking(move || identify(&id))
        .await
        .map_err(|e| format!("识别账号任务失败：{e}"))?
}

// ── 测试 ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 用完即删的临时目录：断言失败时靠 Drop 也能收走，不给机器留垃圾。
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "k2a-accounts-{tag}-{}",
                uuid::Uuid::new_v4().simple()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn write(&self, name: &str, value: &Value) -> PathBuf {
            let path = self.0.join(name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn token(hash: &str, minutes: i64) -> Value {
        let expires = chrono::Utc::now() + chrono::Duration::minutes(minutes);
        json!({
            "accessToken": "AT",
            "refreshToken": "RT",
            "expiresAt": expires.to_rfc3339(),
            "clientIdHash": hash,
            "authMethod": "IdC",
            "region": "us-east-1",
        })
    }

    fn registration() -> Value {
        let expires = chrono::Utc::now() + chrono::Duration::days(30);
        json!({ "clientId": "CID", "clientSecret": "SECRET", "expiresAt": expires.to_rfc3339() })
    }

    #[test]
    fn token_facts_require_registration_for_idc() {
        let fresh = TokenFacts::from_token(&token("aa11", 30), None);
        assert!(fresh.usable(), "访问令牌没过期就能用");
        assert!(
            !fresh.registration_ok,
            "IdC 没有注册文件时，只是「现在能用」，到期后刷不动"
        );

        let expired = TokenFacts::from_token(&token("aa11", -30), None);
        assert!(!expired.usable(), "IdC 过期且没有注册文件 → 刷不动");

        let expired_with_registration =
            TokenFacts::from_token(&token("aa11", -30), Some(&registration()));
        assert!(expired_with_registration.usable(), "有注册文件就能续期");

        let expired_registration = json!({ "clientId": "CID", "expiresAt": "2000-01-01T00:00:00Z" });
        let stale = TokenFacts::from_token(&token("aa11", -30), Some(&expired_registration));
        assert!(!stale.usable(), "注册文件过期同样不可续期");
    }

    #[test]
    fn social_token_needs_no_registration() {
        let social = json!({
            "accessToken": "AT", "refreshToken": "RT",
            "expiresAt": "2000-01-01T00:00:00Z",
            "authMethod": "social", "provider": "BuilderId"
        });
        let facts = TokenFacts::from_token(&social, None);
        assert!(facts.usable());
        assert!(facts.identity.is_none());
    }

    #[test]
    fn resolve_prefers_usable_account_in_source_order() {
        let make = |id: &str, source: AccountSource, usable: bool| Candidate {
            id: id.to_string(),
            source,
            dir: PathBuf::from("."),
            facts: TokenFacts {
                access_fresh: usable,
                ..Default::default()
            },
            cli: None,
            note: None,
        };

        // 登录存在但不可用 → 越过它挑可用的桌面端
        let list = vec![
            make("login:x", AccountSource::Login, false),
            make("desktop:default", AccountSource::Desktop, true),
            make("cli:x", AccountSource::Cli, true),
        ];
        let (_, active) = resolve(list, None);
        assert_eq!(active.as_deref(), Some("desktop:default"));

        // 配置里指定的账号只要还在就优先
        let list = vec![
            make("login:x", AccountSource::Login, true),
            make("cli:x", AccountSource::Cli, true),
        ];
        let (_, active) = resolve(list.clone(), Some("cli:x"));
        assert_eq!(active.as_deref(), Some("cli:x"));

        // 配置里的账号被删掉 → 回退自动选择
        let (_, active) = resolve(list, Some("login:gone"));
        assert_eq!(active.as_deref(), Some("login:x"));

        // 一条都没有 → None
        assert_eq!(resolve(Vec::new(), Some("login:gone")).1, None);
    }

    #[test]
    fn same_identity_across_sources_is_marked_not_merged() {
        let desktop = Candidate {
            id: "desktop:default".into(),
            source: AccountSource::Desktop,
            dir: PathBuf::from("d"),
            facts: TokenFacts {
                identity: Some("hash1".into()),
                ..Default::default()
            },
            cli: None,
            note: None,
        };
        let cli = Candidate {
            id: "cli:abc".into(),
            source: AccountSource::Cli,
            dir: PathBuf::from("c"),
            facts: TokenFacts {
                identity: Some("hash1".into()),
                ..Default::default()
            },
            cli: None,
            note: None,
        };
        // 两个来源都保留（用户要看的就是它们的区别），各自加一句「同一账号也在…」
        let marked = mark_shared_identity(vec![desktop.clone(), cli.clone()]);
        assert_eq!(marked.len(), 2);
        assert_eq!(marked[0].note.as_deref(), Some("同一账号也在kiro-cli"));
        assert_eq!(marked[1].note.as_deref(), Some("同一账号也在Kiro 桌面端"));

        // 身份不同 / 没有身份时都不加说明
        let other = Candidate {
            id: "cli:other".into(),
            source: AccountSource::Cli,
            dir: PathBuf::from("o"),
            facts: TokenFacts::default(),
            cli: None,
            note: None,
        };
        let unrelated = mark_shared_identity(vec![desktop, other]);
        assert_eq!(unrelated.len(), 2);
        assert!(unrelated.iter().all(|candidate| candidate.note.is_none()));
    }

    #[test]
    fn candidates_read_three_sources_and_ignore_orphans() {
        let root = TempDir::new("root");
        let legacy = TempDir::new("legacy");
        let desktop = TempDir::new("desktop");

        // 旧版自有登录
        legacy.write(TOKEN_FILE, &token("aa", 30));
        // 新版登录（目录名 = clientId 哈希）
        root.write("login/bb/kiro-auth-token.json", &token("bb", 30));
        let hash = "cc".repeat(32);
        root.write(
            &format!("login/{hash}/kiro-auth-token.json"),
            &token(&hash, 30),
        );
        root.write(&format!("login/{hash}/{hash}.json"), &registration());
        // 桌面端：有令牌 + 注册文件
        desktop.write(TOKEN_FILE, &token(&hash, 30));
        desktop.write(&format!("{hash}.json"), &registration());
        // 孤立的注册文件（上一次登录的残留）：没有令牌就不是账号
        desktop.write("orphan-hash.json", &registration());

        let cli = vec![CliCredential {
            db: PathBuf::from("C:/fake/kiro-cli/data.sqlite3"),
            key: "kirocli:odic:token".to_string(),
            db_mtime_ms: 1,
            token: token("dd", 30),
            registration: None,
        }];

        let list = candidates(root.path(), legacy.path(), desktop.path(), &cli);
        assert!(list.iter().any(|candidate| candidate.id == "login:legacy"));
        assert!(list.iter().any(|candidate| candidate.id == "login:bb"));
        // 目录名就是 id 的后半段，不多加一层前缀
        assert!(list
            .iter()
            .any(|candidate| candidate.id == format!("login:{hash}")));
        assert_eq!(
            list.iter()
                .filter(|candidate| candidate.source == AccountSource::Desktop)
                .count(),
            1,
            "孤立注册文件不能变成账号"
        );
        assert!(list
            .iter()
            .any(|candidate| candidate.source == AccountSource::Cli));
        // 登录账号与桌面端即便 clientIdHash 相同也各自保留（续期归属不同）
        assert_eq!(
            list.iter()
                .filter(|candidate| candidate.source == AccountSource::Login)
                .count(),
            3
        );
    }

    #[test]
    fn materialize_imports_cli_copy_and_follows_database_changes() {
        let root = TempDir::new("cli-copy");
        let db = root.path().join("data.sqlite3");
        write_cli_db(&db, "AT");
        let credential = cli_bridge::read_one(&db, None).unwrap().unwrap();
        let dir = root.path().join("accounts/cli/test");
        let candidate = Candidate {
            id: "cli:test".into(),
            source: AccountSource::Cli,
            dir: dir.clone(),
            facts: TokenFacts::from_token(
                &credential.token,
                credential.registration.as_ref().map(|(_, value)| value),
            ),
            cli: Some(CliOrigin {
                db: db.clone(),
                key: credential.key.clone(),
                mtime_ms: credential.db_mtime_ms,
            }),
            note: None,
        };

        materialize(&candidate).unwrap();
        assert!(dir.join(TOKEN_FILE).is_file());
        let imported = |dir: &Path| -> Value {
            serde_json::from_str(&fs::read_to_string(dir.join(TOKEN_FILE)).unwrap()).unwrap()
        };
        assert_eq!(imported(&dir)["accessToken"], "AT");
        // IdC 的注册文件也要一起落地，否则续期链路不完整
        assert_eq!(
            fs::read_dir(&dir)
                .unwrap()
                .flatten()
                .filter(|entry| {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    name.ends_with(".json") && name != TOKEN_FILE && name != META_FILE
                })
                .count(),
            1
        );

        // CLI 重新登录（库内容变了、mtime 也变了）→ 副本跟着更新
        write_cli_db(&db, "AT2");
        let refreshed = cli_bridge::read_one(&db, None).unwrap().unwrap();
        let candidate = Candidate {
            cli: Some(CliOrigin {
                db: db.clone(),
                key: refreshed.key.clone(),
                mtime_ms: refreshed.db_mtime_ms,
            }),
            ..candidate
        };
        materialize(&candidate).unwrap();
        assert_eq!(imported(&dir)["accessToken"], "AT2");
    }

    /// 造一个最小可用的 kiro-cli 凭据库（IdC 令牌 + 30 天有效的注册）。
    fn write_cli_db(path: &Path, access_token: &str) {
        write_cli_db_rows(path, access_token, 60, Some(30))
    }

    /// 同上，可指定令牌到期分钟数与注册记录的有效期（`None` = 不带注册记录）。
    fn write_cli_db_rows(
        path: &Path,
        access_token: &str,
        expires_in_minutes: i64,
        registration_days: Option<i64>,
    ) {
        let con = rusqlite::Connection::open(path).unwrap();
        con.execute("create table if not exists auth_kv (key text primary key, value text)", [])
            .unwrap();
        let mut rows = vec![(
            "kirocli:odic:token",
            json!({
                "access_token": access_token,
                "refresh_token": "RT",
                "expires_at": (chrono::Utc::now() + chrono::Duration::minutes(expires_in_minutes))
                    .to_rfc3339(),
                "region": "us-east-1",
                "start_url": "https://d-1.awsapps.com/start"
            })
            .to_string(),
        )];
        if let Some(days) = registration_days {
            rows.push((
                "kirocli:odic:device-registration",
                json!({
                    "client_id": "CID", "client_secret": "SECRET",
                    "client_secret_expires_at": (chrono::Utc::now() + chrono::Duration::days(days)).to_rfc3339(),
                    "region": "us-east-1"
                })
                .to_string(),
            ));
        }
        for (key, value) in rows {
            con.execute(
                "insert or replace into auth_kv values (?1, ?2)",
                [key, value.as_str()],
            )
            .unwrap();
        }
    }

    #[test]
    fn cli_suffix_is_stable_and_short() {
        let make = |db: &str, key: &str| CliCredential {
            db: PathBuf::from(db),
            key: key.to_string(),
            db_mtime_ms: 0,
            token: Value::Null,
            registration: None,
        };
        let a = cli_suffix(&make("C:/x/data.sqlite3", "kirocli:odic:token"));
        let b = cli_suffix(&make("C:/x/data.sqlite3", "kirocli:odic:token"));
        let c = cli_suffix(&make("C:/y/data.sqlite3", "kirocli:odic:token"));
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 12);
    }

    // ── 续期 / 保活 ──────────────────────────────────────────────────────

    /// 固定时间点 + 相对它的到期时间，保证边界断言与真实时钟无关。
    fn at(minutes_from_now: i64) -> (i64, String) {
        use chrono::TimeZone;
        let now = 1_760_000_000_000i64;
        let expires = now + minutes_from_now * 60_000;
        let stamp = chrono::Utc
            .timestamp_millis_opt(expires)
            .single()
            .unwrap()
            .to_rfc3339();
        (now, stamp)
    }

    #[test]
    fn needs_renewal_only_when_close_to_expiry() {
        let cases = [
            (120, false, "还剩 2 小时不用续"),
            (46, false, "46 分钟仍在阈值外"),
            (45, true, "45 分钟正好到阈值（下一轮巡检前会死）"),
            (10, true, "只剩 10 分钟"),
            (-5, true, "已经过期"),
        ];
        for (minutes, expected, hint) in cases {
            let (now, stamp) = at(minutes);
            assert_eq!(
                needs_renewal(Some(&stamp), now),
                expected,
                "{hint}（{minutes} 分钟）"
            );
        }
        let (now, _) = at(0);
        assert!(needs_renewal(None, now), "读不到到期时间就续一次");
        assert!(needs_renewal(Some("not-a-time"), now), "解析不了同样续一次");
    }

    /// 闸门：不可续的原因要挡住发请求；剩余充足时只有手动能强行续。
    #[test]
    fn renewal_gate_blocks_or_defers() {
        let (now, _) = at(120);
        let (_, stale) = at(10);

        let usable = TokenFacts::from_token(&token("aa11", 120), Some(&registration()));
        assert_eq!(renewal_gate(&usable, RefreshMode::Loop, now), Some(Skip::StillFresh));
        assert_eq!(
            renewal_gate(&usable, RefreshMode::KeepAlive, now),
            Some(Skip::StillFresh)
        );
        assert_eq!(renewal_gate(&usable, RefreshMode::Manual, now), None, "手动不看阈值");

        let mut expired = usable.clone();
        expired.expires_at = Some(stale);
        assert_eq!(renewal_gate(&expired, RefreshMode::Loop, now), None);

        let mut no_refresh = usable.clone();
        no_refresh.has_refresh = false;
        assert_eq!(
            renewal_gate(&no_refresh, RefreshMode::Manual, now),
            Some(Skip::NoRefreshToken),
            "手动也不能凭空续"
        );

        let mut no_registration = usable.clone();
        no_registration.registration_ok = false;
        assert_eq!(
            renewal_gate(&no_registration, RefreshMode::Manual, now),
            Some(Skip::NoRegistration)
        );
    }

    /// 桌面端与 IDE 共用一份文件：连「要不要续」都不判断，直接跳过。
    #[test]
    fn desktop_account_is_never_refreshed() {
        let dir = TempDir::new("desktop-skip");
        dir.write(TOKEN_FILE, &token("aa11", -30));
        let candidate = Candidate {
            id: "desktop:default".into(),
            source: AccountSource::Desktop,
            dir: dir.path().to_path_buf(),
            facts: read_dir_facts(dir.path()).unwrap(),
            cli: None,
            note: None,
        };
        let outcome = refresh_candidate(&candidate, RefreshMode::KeepAlive).unwrap();
        assert_eq!(
            outcome,
            RefreshOutcome::Skipped {
                reason: Skip::SharedWithIde,
                expires_at: candidate.facts.expires_at.clone(),
            }
        );
    }

    /// 登录账号：剩余充足时既不发请求、也不改文件（连 mtime 都不动）。
    #[test]
    fn login_account_with_fresh_token_is_left_alone() {
        let dir = TempDir::new("login-fresh");
        let hash = "ab".repeat(32);
        dir.write(TOKEN_FILE, &token(&hash, 120));
        dir.write(&format!("{hash}.json"), &registration());
        let candidate = Candidate {
            id: "login:fresh".into(),
            source: AccountSource::Login,
            dir: dir.path().to_path_buf(),
            facts: read_dir_facts(dir.path()).unwrap(),
            cli: None,
            note: None,
        };
        let before = fs::read_to_string(dir.path().join(TOKEN_FILE)).unwrap();

        let outcome = refresh_candidate(&candidate, RefreshMode::KeepAlive).unwrap();
        assert!(matches!(
            outcome,
            RefreshOutcome::Skipped { reason: Skip::StillFresh, .. }
        ));
        assert_eq!(
            fs::read_to_string(dir.path().join(TOKEN_FILE)).unwrap(),
            before,
            "跳过的轮次不能碰令牌文件"
        );
    }

    /// CLI 账号：**判据取原库**。库里刚续过（2 小时）时，即使副本已经过期也不该再续；
    /// 但副本要立刻对齐库 —— 代理读的是副本，留着过期副本等于把可用请求变成 401。
    #[test]
    fn cli_account_judges_by_database_not_copy() {
        let dir = TempDir::new("cli-db-fresh");
        let db = dir.path().join("data.sqlite3");
        write_cli_db_rows(&db, "FRESH_AT", 120, Some(30));

        // 副本还停在上一轮导入的过期状态
        let copy = dir.path().join("accounts/cli/test");
        fs::create_dir_all(&copy).unwrap();
        fs::write(
            copy.join(TOKEN_FILE),
            serde_json::to_vec(&token("cd", -30)).unwrap(),
        )
        .unwrap();

        let candidate = cli_candidate(&db, &copy);
        let outcome = refresh_candidate(&candidate, RefreshMode::KeepAlive).unwrap();
        assert!(
            matches!(outcome, RefreshOutcome::Skipped { reason: Skip::StillFresh, .. }),
            "库里还剩 2 小时，不该因为副本旧就再续一次：{outcome:?}"
        );

        // 副本被对齐成库里那份（说明判断依据不是它，而它也不会继续过期下去）
        let copy_token: Value =
            serde_json::from_str(&fs::read_to_string(copy.join(TOKEN_FILE)).unwrap()).unwrap();
        assert_eq!(copy_token["accessToken"], "FRESH_AT");

        // 原库没被续期污染：access_token / refresh_token 逐字未变
        let raw: String = rusqlite::Connection::open(&db)
            .unwrap()
            .query_row(
                "select value from auth_kv where key = 'kirocli:odic:token'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let stored: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(stored["access_token"], "FRESH_AT");
        assert_eq!(stored["refresh_token"], "RT");
    }

    /// CLI 账号的注册记录过期后，refreshToken 也刷不动 —— 必须挡在发请求之前。
    #[test]
    fn cli_account_with_expired_registration_is_skipped() {
        let dir = TempDir::new("cli-expired-reg");
        let db = dir.path().join("data.sqlite3");
        // 令牌过期 + 注册记录也过期：只能重新登录
        write_cli_db_rows(&db, "AT", -30, Some(-1));

        let candidate = cli_candidate(&db, &dir.path().join("copy"));
        assert!(!candidate.facts.registration_ok, "测试数据本身要过期");
        let outcome = refresh_candidate(&candidate, RefreshMode::KeepAlive).unwrap();
        assert!(
            matches!(outcome, RefreshOutcome::Skipped { reason: Skip::NoRegistration, .. }),
            "注册记录过期时应当跳过而不是白等一次超时：{outcome:?}"
        );
    }

    /// 同一账号同时只能有一轮续期；锁在 Drop 时释放。
    #[test]
    fn only_one_refresh_per_account_at_a_time() {
        let id = format!("login:lock-{}", uuid::Uuid::new_v4().simple());
        let first = InFlight::acquire(&id).expect("第一轮应拿到锁");
        assert!(InFlight::acquire(&id).is_none(), "第二轮必须被挡下");
        drop(first);
        assert!(InFlight::acquire(&id).is_some(), "释放后可以重新拿");
    }

    /// 回写原库后要把副本的 mtime 标记同步过来，否则下一次扫描会白覆盖一遍。
    #[test]
    fn sync_copy_meta_keeps_copy_in_step_with_database() {
        let dir = TempDir::new("cli-sync");
        let db = dir.path().join("data.sqlite3");
        write_cli_db_rows(&db, "AT2", 120, Some(30));
        let copy = dir.path().join("accounts/cli/test");
        fs::create_dir_all(&copy).unwrap();
        fs::write(
            copy.join(TOKEN_FILE),
            serde_json::to_vec(&token("ef", 120)).unwrap(),
        )
        .unwrap();
        write_meta(
            &copy,
            AccountMeta {
                created_at_ms: 111,
                db_path: Some(db.to_string_lossy().to_string()),
                db_mtime_ms: Some(999), // 故意写一个错的值，模拟「刚回写过」
            },
        )
        .unwrap();

        let origin = CliOrigin {
            db: db.clone(),
            key: "kirocli:odic:token".to_string(),
            mtime_ms: cli_bridge::file_mtime_ms(&db),
        };
        sync_copy_meta(&copy, &origin).unwrap();

        let meta = read_meta(&copy).unwrap();
        assert_eq!(meta.created_at_ms, 111, "创建时刻不能被改写");
        assert_eq!(meta.db_mtime_ms, Some(cli_bridge::file_mtime_ms(&db)));

        // 标记同步之后，materialize 认为副本是新的，不会再导一遍
        let candidate = Candidate {
            id: "cli:test".into(),
            source: AccountSource::Cli,
            dir: copy.clone(),
            facts: read_dir_facts(&copy).unwrap(),
            cli: Some(origin),
            note: None,
        };
        materialize(&candidate).unwrap();
        let copy_token: Value =
            serde_json::from_str(&fs::read_to_string(copy.join(TOKEN_FILE)).unwrap()).unwrap();
        assert_eq!(copy_token["accessToken"], "AT", "副本不应被库里的 AT2 覆盖");
    }

    /// 用 CLI 库造一个候选账号（副本目录可以还不存在）。
    fn cli_candidate(db: &Path, copy: &Path) -> Candidate {
        let credential = cli_bridge::read_one(db, None).unwrap().unwrap();
        Candidate {
            id: "cli:test".into(),
            source: AccountSource::Cli,
            dir: copy.to_path_buf(),
            facts: TokenFacts::from_token(
                &credential.token,
                credential.registration.as_ref().map(|(_, value)| value),
            ),
            cli: Some(CliOrigin {
                db: db.to_path_buf(),
                key: credential.key.clone(),
                mtime_ms: credential.db_mtime_ms,
            }),
            note: None,
        }
    }
}
