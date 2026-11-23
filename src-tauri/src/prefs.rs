//! kiro2api 本地配置：端口、代理密钥、模型与额度缓存。
//!
//! 单一事实源是数据目录下的 `config.json`。所有读写都经过 [`update_config`]，
//! 它以进程内互斥锁串行化「读 → 改 → 原子写」，避免 Tauri 命令与后台刷新线程并发时互相覆盖。
//! 代理密钥属于敏感信息，落盘时按最小权限写入（类 Unix 下 0600）。
//!
//! 「模型列表」与「账户额度」都**只**是缓存：它们由上游同步得到，落盘纯粹是为了
//! 让应用一打开就有东西可显示（启动时的后台刷新随后静默替换成最新值）。
//! 真正的模型选择不在这里 —— 客户端在每次请求里自己指定模型。

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use serde::{Deserialize, Serialize};

use crate::kiro::auth::KiroUsageInfo;
use crate::paths::get_data_path;

pub(crate) const DEFAULT_PORT: u16 = 5680;

/// 持久化配置。字段全部带 `#[serde(default)]`，新增字段时旧配置文件仍可读。
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct AppConfig {
    /// 代理监听端口（被占用时自动向后找可用端口，实际端口以运行状态为准）。
    pub(crate) port: u16,
    /// 代理密钥。持久化后，复制到其它客户端（Claude Code / OpenAI SDK）的配置在重启后仍然可用。
    pub(crate) proxy_api_key: Option<String>,
    /// 上一次从上游同步到的可用模型。
    ///
    /// 落盘的意义是「打开应用就能看见有哪些模型」—— 不必先启动代理，也不必等联网。
    /// `alias` 保留旧键名 `displayModels`，让升级前写下的缓存不白丢。
    #[serde(alias = "displayModels")]
    pub(crate) models: Vec<String>,
    /// 上一次同步到的模型消耗倍率（公共模型 id → rateMultiplier，1 = 基准，0 = 不计费）。
    ///
    /// 与 `models` 同一次同步落盘（见 `store_models`），保证两份数据出自同一时刻；
    /// 空表 = 上游没给过或还没同步过，界面不显示倍率。
    pub(crate) model_rates: HashMap<String, f64>,
    /// 上一次同步到的模型输入模态（公共模型 id → 上游 `supportedInputTypes` 的小写值，
    /// 例如 `["text","image"]`）。
    ///
    /// 与 `models` / `model_rates` 同一次同步落盘。为什么要存：Codex 模型目录里的
    /// `input_modalities` 决定客户端允不允许发图，而那份目录是离线生成的 —— 没有这份
    /// 缓存就只能写死 `["text"]`，用户在 Codex 里贴图会被直接判定为「模型不支持」。
    /// 空表 = 还没同步过，调用方按模型名家族兜底（见 `models::default_input_modalities`）。
    pub(crate) model_input_types: HashMap<String, Vec<String>>,
    /// 曾经在清单里、这次同步却不在的模型（公共 id）。
    ///
    /// 上游把某个模型从账户清单里撤掉时，直接把它从 `models` 删掉会让界面凭空少一排，
    /// 用户只会以为「程序坏了 / 名字记错了」。留着这份名单，界面才能把「当前套餐不再提供」
    /// 讲明白，而不是静默消失。模型重新出现时同名条目会被自动清掉。
    pub(crate) unavailable_models: Vec<String>,
    /// 上一次查询到的账户额度快照。
    pub(crate) usage_cache: Option<UsageCache>,
    /// 应用内更新的下载源。
    ///
    /// `None` 就是「自动」：每次拉清单时并行探测各镜像，挑第一个真能传数据的。
    /// 其余取值是 `updater_accel` 里那份源清单的 id（`direct` 或某个镜像 id）——
    /// 认不出来的值一律按自动处理，这样将来下线某个镜像时，旧配置不会让更新彻底不可用。
    pub(crate) update_source: Option<String>,
    /// 客户端没指定思考级别时由代理补上的那一档；`None` = 内置默认（`high`）。
    ///
    /// 这里只存用户选过的值，**不做校验** —— 合法档位属于 `settings` 的知识，
    /// 两处都写一份就迟早漂移。读到不认识的档位时由 `settings` 按「没配」处理。
    pub(crate) default_effort: Option<String>,
    /// 写进 Codex 模型目录的上下文窗口大小（token）；`None` = 内置默认（300K）。
    pub(crate) context_window: Option<u32>,
    /// 当前选中的 Kiro 账号 id（见 `crate::accounts`）。
    ///
    /// `None` 就是「自动」：按「登录的账号 > Kiro 桌面端 > 本地 kiro-cli」挑第一个可用的。
    /// 这里只存 id，不存路径 —— 路径是账号扫描的产物，换个位置（CLI 副本重建、
    /// 旧版凭据目录）时 id 仍然指得回同一个人。
    pub(crate) active_account: Option<String>,
    /// 账号身份缓存：账号 id → 邮箱 / 套餐。
    ///
    /// 用户真正能用来区分多账号的是邮箱，而邮箱只能联网问上游（getUsageLimits）。
    /// 缓存住之后，进账号页先画本地信息，识别过的那几个直接带名字，不必每次都等网络。
    pub(crate) account_identities: HashMap<String, AccountIdentity>,
    /// 登录过的企业站点（最近使用的在前，见 [`LoginSite`]）。
    pub(crate) login_sites: Vec<LoginSite>,
}

/// 登录过的企业站点：IAM Identity Center 的 Start URL + 它用的区域。
///
/// 为什么要记：Start URL 是 `https://d-xxxxxxxxxx.awsapps.com/start` 这样一串随机目录 id，
/// 没人记得住 —— 重新登录、换机器、加第二个域时只能翻旧邮件或旧截图。记下来之后
/// 登录弹窗里给一个下拉，点一下就把 URL 与区域一起填回去。
///
/// 只服务于输入便利：不参与账号选择，也不含任何令牌（凭据仍在 `accounts/` 下）。
#[derive(Debug, Serialize, Deserialize, Clone, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct LoginSite {
    pub(crate) start_url: String,
    /// 该站点用的 OIDC 区域。跟 URL 一起记，省得用户挑回旧域却忘了区域对不上。
    pub(crate) region: String,
    /// 最近一次登录成功的时刻（Unix 毫秒）。列表按它倒序，前端不展示。
    pub(crate) used_at_ms: i64,
}

/// 历史站点最多记几条。
///
/// 上限是「够用，且下拉不至于翻半天」：一个人不会登录很多个企业域。记满之后再登录
/// 新的，最久未用的那条让位 —— 比起无限增长，用户更可能在翻十年前的域时选错。
const MAX_LOGIN_SITES: usize = 8;

/// 一个账号的「人话身份」快照。
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct AccountIdentity {
    pub(crate) email: Option<String>,
    /// 套餐名（上游 `subscriptionTitle`，如 `Kiro Pro`）。
    pub(crate) plan: Option<String>,
    /// 取得时刻（Unix 毫秒）。
    pub(crate) fetched_at_ms: i64,
}

/// 额度快照 + 写入时刻。重启后先拿它渲染，再等后台静默刷新替换。
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct UsageCache {
    pub(crate) info: KiroUsageInfo,
    /// 写入时刻（Unix 毫秒）。用于诊断，以及将来判断缓存是否已过期。
    pub(crate) updated_at_ms: i64,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            proxy_api_key: None,
            models: Vec::new(),
            model_rates: HashMap::new(),
            model_input_types: HashMap::new(),
            unavailable_models: Vec::new(),
            usage_cache: None,
            update_source: None,
            default_effort: None,
            context_window: None,
            active_account: None,
            account_identities: HashMap::new(),
            login_sites: Vec::new(),
        }
    }
}

static CONFIG_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn config_lock() -> &'static Mutex<()> {
    CONFIG_LOCK.get_or_init(|| Mutex::new(()))
}

fn lock() -> Result<MutexGuard<'static, ()>, String> {
    config_lock()
        .lock()
        .map_err(|_| "配置锁已损坏，请重启应用".to_string())
}

pub(crate) fn config_path() -> PathBuf {
    get_data_path().join("config.json")
}

/// 读取配置；文件缺失或损坏时回退到默认值（不报错，保证应用总能启动）。
pub(crate) fn load_config() -> AppConfig {
    let path = config_path();
    let Ok(text) = fs::read_to_string(&path) else {
        return AppConfig::default();
    };
    match serde_json::from_str::<AppConfig>(&text) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("[config] {} 解析失败，回退默认值: {error}", path.display());
            AppConfig::default()
        }
    }
}

/// 读 → 改 → 原子写。返回值是闭包产物，便于「改完直接把结果给调用方」。
pub(crate) fn update_config<F, R>(update: F) -> Result<R, String>
where
    F: FnOnce(&mut AppConfig) -> Result<R, String>,
{
    let _guard = lock()?;
    let path = config_path();
    let mut config = load_config();
    let result = update(&mut config)?;
    atomic_write_json(&path, &config)?;
    Ok(result)
}

/// 生成新的代理密钥。
pub(crate) fn generate_api_key() -> String {
    format!("k2a-{}", uuid::Uuid::new_v4().simple())
}

/// 读取持久化密钥；缺失时生成并写回。
pub(crate) fn ensure_api_key() -> Result<String, String> {
    update_config(|config| {
        if let Some(existing) = config
            .proxy_api_key
            .as_deref()
            .map(str::trim)
            .filter(|key| !key.is_empty())
        {
            return Ok(existing.to_string());
        }
        let key = generate_api_key();
        config.proxy_api_key = Some(key.clone());
        Ok(key)
    })
}

/// 脱敏展示：首尾各留 4 位，中间用圆点填充。
pub(crate) fn mask_api_key(key: &str) -> String {    let trimmed = key.trim();
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.is_empty() {
        return String::new();
    }
    if chars.len() <= 8 {
        return "•".repeat(chars.len());
    }
    let head: String = chars[..4].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    let dots = 6usize.max(12.min(chars.len() - 8));
    format!("{head}{}{tail}", "•".repeat(dots))
}

// ── 缓存读写（模型列表 / 额度快照）──────────────────────────────────────
//
// 统一走 `load_config` / `update_config`，与其它字段共用同一把锁 ——
// 后台刷新线程与前端命令可能同时写，绕过锁就会互相覆盖。

/// 读取缓存的模型列表（纯本地读取，不发网络请求）。
pub(crate) fn cached_models() -> Vec<String> {
    load_config().models
}

/// 读取缓存的模型倍率（纯本地读取，不发网络请求）。
pub(crate) fn cached_model_rates() -> HashMap<String, f64> {
    load_config().model_rates
}

/// 读取「曾经有、这次没有」的模型名单（纯本地读取）。
pub(crate) fn cached_unavailable_models() -> Vec<String> {
    load_config().unavailable_models
}

/// 算出「曾经有、这次没有」的模型集合：`上次清单 ∪ 上次灰名单 − 这次清单`。
///
/// 三处细节都是有意的：
/// - 并入上次的灰名单，是因为模型一旦被撤下就会持续缺席 —— 只看单次 diff 的话，
///   第二次同步就会把它忘掉，界面上的灰卡跟着消失；
/// - 减去这次清单，模型回来时灰态自动消失；
/// - 排序去重，避免上游顺序变化让同一份名单在配置里反复改写成不同顺序。
fn stale_models(previous: &[String], hidden: &[String], current: &[String]) -> Vec<String> {
    let mut out: Vec<String> = previous
        .iter()
        .chain(hidden.iter())
        .filter(|id| !current.iter().any(|shown| shown == *id))
        .cloned()
        .collect();
    out.sort();
    out.dedup();
    out
}

/// 读取缓存的模型输入模态（纯本地读取，不发网络请求）。
pub(crate) fn cached_model_input_types() -> HashMap<String, Vec<String>> {
    load_config().model_input_types
}

/// 写入模型缓存：清单、倍率与输入模态同一次落盘，保证三份数据出自同一次同步。
///
/// 顺带维护 [`AppConfig::unavailable_models`]，并把这份名单返回给调用方
/// （要一起推给界面，见 `ModelsState::unavailable`）。
pub(crate) fn store_models(
    models: &[String],
    rates: &HashMap<String, f64>,
    input_types: &HashMap<String, Vec<String>>,
) -> Result<Vec<String>, String> {
    let mut unavailable: Vec<String> = Vec::new();
    update_config(|config| {
        let stale = stale_models(&config.models, &config.unavailable_models, models);
        unavailable = stale.clone();
        config.models = models.to_vec();
        config.model_rates = rates.clone();
        // 只留当前清单里还有的模型：上游撤掉的模型不该继续占着模态表
        config.model_input_types = input_types
            .iter()
            .filter(|(id, _)| models.contains(id))
            .map(|(id, types)| (id.clone(), types.clone()))
            .collect();
        config.unavailable_models = stale;
        Ok(())
    })?;
    Ok(unavailable)
}

/// 读取缓存的额度快照（纯本地读取）。从未查询过时为 `None`。
///
/// `fetchedAt` 在这里用**缓存写入时刻**补上：它描述的是「这份数据有多老」，
/// 读盘时填才能让上次启动前写下的快照也带得上年龄。
pub(crate) fn cached_usage() -> Option<KiroUsageInfo> {
    load_config().usage_cache.map(|cache| KiroUsageInfo {
        fetched_at: ms_to_iso(cache.updated_at_ms),
        ..cache.info
    })
}

/// 写入额度快照（连同写入时刻），返回该时刻的 Unix 毫秒 —— 调用方把它填进
/// `fetchedAt`，让返回给前端的值与落盘的缓存出自同一时间源，不会各说各话。
pub(crate) fn store_usage(info: &KiroUsageInfo) -> Result<i64, String> {
    let stamped = now_ms();
    update_config(|config| {
        config.usage_cache = Some(UsageCache {
            info: info.clone(),
            updated_at_ms: stamped,
        });
        Ok(())
    })?;
    Ok(stamped)
}

/// Unix 毫秒 → UTC 的 RFC3339（秒级，`Z` 结尾）。前端会转成本地时区再展示。
pub(crate) fn ms_to_iso(ms: i64) -> Option<String> {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

// ── 账号选择与身份缓存 ──────────────────────────────────────────────────

/// 读取手动选中的账号 id；`None` 表示按优先级自动挑（见 `crate::accounts`）。
pub(crate) fn active_account_pref() -> Option<String> {
    load_config().active_account
}

/// 写入 / 清空手动选中的账号 id。
pub(crate) fn store_active_account(id: Option<&str>) -> Result<(), String> {
    update_config(|config| {
        config.active_account = id.map(str::to_string);
        Ok(())
    })
}

/// 全部账号的身份缓存（纯本地读取）。
pub(crate) fn account_identities() -> HashMap<String, AccountIdentity> {
    load_config().account_identities
}

/// 记下一个账号识别出的邮箱与套餐。
pub(crate) fn store_account_identity(
    id: &str,
    email: Option<&str>,
    plan: Option<&str>,
) -> Result<(), String> {
    update_config(|config| {
        config.account_identities.insert(
            id.to_string(),
            AccountIdentity {
                email: email.map(str::to_string),
                plan: plan.map(str::to_string),
                fetched_at_ms: now_ms(),
            },
        );
        Ok(())
    })
}

/// 清掉属于「某一个账号」的缓存：模型清单、倍率、额度快照。
///
/// 三份数据都随账号而变 —— 换了账号还留着上一个的模型与额度，用户会以为新账号
/// 已经同步过了。切换账号时由 `accounts::select` 调用。
pub(crate) fn clear_account_caches() -> Result<(), String> {
    update_config(|config| {
        config.models.clear();
        config.model_rates.clear();
        // 换账号就换了一份套餐：上一个账号「不再提供」的名单对新账号毫无意义，
        // 留着只会在新账号的清单里挂一串无主的灰卡。
        config.unavailable_models.clear();
        config.usage_cache = None;
        Ok(())
    })
}

// ── 登录过的域（登录弹窗的历史下拉）───────────────────────────────────
//
// 只做存取与去重，不校验 URL 是否可用（那是 `kiro::login::resolve_target` 的事，
// 两处各写一份规则迟早漂移）。

/// 历史站点清单（最近使用的在前）。
pub(crate) fn login_sites() -> Vec<LoginSite> {
    load_config().login_sites
}

/// 两个 Start URL 是否是「同一个域」。
///
/// 忽略大小写（主机名本就大小写不敏感）、末尾的 `#` 与 `/` —— 用户手抄时经常
/// 多带一个斜杠，或者从浏览器地址栏复制时末尾挂着锚点。
pub(crate) fn same_login_site(left: &str, right: &str) -> bool {
    normalize_site_url(left) == normalize_site_url(right)
}

fn normalize_site_url(url: &str) -> String {
    url.trim()
        .trim_end_matches('#')
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

/// 并进一条历史：同域只留一条、挪到最前、超上限丢掉最久未用的。
fn merge_login_site(list: &mut Vec<LoginSite>, site: LoginSite, max: usize) {
    list.retain(|existing| !same_login_site(&existing.start_url, &site.start_url));
    list.insert(0, site);
    list.truncate(max);
}

/// 记下一次成功的登录（由 `kiro::login` 在凭据写盘之后调用）。
///
/// 空值直接忽略而不是报错：这是「锦上添花」的记录，不该让一次已经成功的登录失败。
pub(crate) fn remember_login_site(start_url: &str, region: &str) -> Result<(), String> {
    let site = LoginSite {
        start_url: start_url.trim().trim_end_matches('#').to_string(),
        region: region.trim().to_ascii_lowercase(),
        used_at_ms: now_ms(),
    };
    if site.start_url.is_empty() || site.region.is_empty() {
        return Ok(());
    }
    update_config(|config| {
        merge_login_site(&mut config.login_sites, site, MAX_LOGIN_SITES);
        Ok(())
    })
}

// ── 更新下载源 ──────────────────────────────────────────────────────────
//
// 取值是 `updater_accel` 那份源清单的 id。这里刻意只做存取、不做校验：
// 「哪些 id 是有效的」属于清单代理的知识，放在同一处才不会两处漂移。

/// 读取更新下载源偏好；`None` 表示「自动」。
pub(crate) fn update_source_pref() -> Option<String> {
    load_config().update_source
}

/// 写入更新下载源偏好。
pub(crate) fn store_update_source(id: &str) -> Result<(), String> {
    update_config(|config| {
        config.update_source = Some(id.to_string());
        Ok(())
    })
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

// ── 原子写入 ────────────────────────────────────────────────────────────

fn atomic_write_json<T>(path: &Path, value: &T) -> Result<(), String>
where
    T: Serialize + ?Sized,
{
    let content = serde_json::to_vec_pretty(value)
        .map_err(|e| format!("配置序列化失败: {e}"))?;
    let parent = path
        .parent()
        .ok_or_else(|| format!("路径无父目录: {}", path.display()))?;
    fs::create_dir_all(parent).map_err(|e| format!("创建 {} 失败: {e}", parent.display()))?;

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("非法文件名: {}", path.display()))?;

    let temp_path = parent.join(format!(".{file_name}.tmp-{}", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let result = (|| -> Result<(), String> {
        let mut file = options
            .open(&temp_path)
            .map_err(|e| format!("创建 {} 失败: {e}", temp_path.display()))?;
        file.write_all(&content)
            .map_err(|e| format!("写入 {} 失败: {e}", temp_path.display()))?;
        file.flush()
            .map_err(|e| format!("刷盘 {} 失败: {e}", temp_path.display()))?;
        file.sync_all()
            .map_err(|e| format!("同步 {} 失败: {e}", temp_path.display()))?;
        // 必须先关闭句柄再 rename：Windows 上占用中的文件无法被替换
        drop(file);
        fs::rename(&temp_path, path)
            .map_err(|e| format!("替换 {} 失败: {e}", path.display()))?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_key_is_prefixed_and_unique() {
        let first = generate_api_key();
        let second = generate_api_key();
        assert!(first.starts_with("k2a-"));
        assert_ne!(first, second);
    }

    #[test]
    fn mask_hides_middle() {
        let key = "k2a-0123456789abcdef";
        let masked = mask_api_key(key);
        assert!(masked.contains('•'));
        assert!(!masked.contains(key));
        assert_eq!(mask_api_key(""), "");
    }

    #[test]
    fn default_config_is_usable() {
        let config = AppConfig::default();
        assert_eq!(config.port, DEFAULT_PORT);
        assert!(config.proxy_api_key.is_none());
        assert!(config.unavailable_models.is_empty());
    }

    // ── 模型灰名单：`stale_models` 是纯函数，直接喂三份清单验算 ──

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn stale_models_is_empty_on_first_sync() {
        // 首次同步：没有「上次」，不该凭空造出一串灰卡
        assert!(stale_models(&[], &[], &ids(&["gpt-5"])).is_empty());
    }

    #[test]
    fn stale_models_records_models_that_left_the_catalog() {
        let previous = ids(&["claude-opus-5", "gpt-5"]);
        assert_eq!(
            stale_models(&previous, &[], &ids(&["gpt-5"])),
            ids(&["claude-opus-5"])
        );
    }

    #[test]
    fn stale_models_keeps_remembering_across_repeated_syncs() {
        // 第二次同步时「上次清单」已经不含 claude 了；只看单次 diff 会把它忘掉，
        // 所以必须并上上一次的灰名单。
        assert_eq!(
            stale_models(&ids(&["gpt-5"]), &ids(&["claude-opus-5"]), &ids(&["gpt-5"])),
            ids(&["claude-opus-5"])
        );
    }

    #[test]
    fn stale_models_forgets_a_model_once_it_returns() {
        assert!(stale_models(
            &ids(&["gpt-5"]),
            &ids(&["claude-opus-5"]),
            &ids(&["gpt-5", "claude-opus-5"])
        )
        .is_empty());
    }

    #[test]
    fn stale_models_is_sorted_and_deduped() {
        // 同一模型既在旧清单又在旧灰名单里（重名时）只留一条，且顺序稳定，
        // 否则同一份名单会每次都写回不同的 key 顺序，配置文件反复变动。
        assert_eq!(
            stale_models(
                &ids(&["b-model", "a-model"]),
                &ids(&["a-model", "c-model"]),
                &ids(&["gpt-5"])
            ),
            ids(&["a-model", "b-model", "c-model"])
        );
    }

    #[test]
    fn config_roundtrip_keeps_unknown_fields_out_but_known_fields_in() {
        let json = r#"{"port":6000,"proxyApiKey":"k2a-x","retiredField":"ignored"}"#;
        let config: AppConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.port, 6000);
        assert_eq!(config.proxy_api_key.as_deref(), Some("k2a-x"));
        // 未提供的字段落到默认值，而不是解析失败
        assert!(config.models.is_empty());
        assert!(config.usage_cache.is_none());
    }

    /// 旧版本的键名 `displayModels` 必须继续读得进来，否则升级一次就把用户
    /// 上次同步到的模型列表丢了（第一次启动时若恰好没网，界面会空着）。
    #[test]
    fn cached_models_survive_the_key_rename() {
        let legacy = r#"{"displayModels":["claude-opus-5","claude-sonnet-5"]}"#;
        let config: AppConfig = serde_json::from_str(legacy).unwrap();
        assert_eq!(config.models, vec!["claude-opus-5", "claude-sonnet-5"]);
    }

    #[test]
    fn usage_cache_roundtrips_with_its_timestamp() {
        let json = r#"{
            "usageCache": {
                "info": {
                    "subscriptionTitle": "Kiro Pro",
                    "currentUsage": 412.0,
                    "usageLimit": 1000.0,
                    "remaining": 588.0,
                    "percentUsed": 41.0,
                    "currency": "USD"
                },
                "updatedAtMs": 1760300000000
            }
        }"#;
        let config: AppConfig = serde_json::from_str(json).unwrap();
        let cache = config.usage_cache.expect("缓存应被解析出来");
        assert_eq!(cache.info.percent_used, 41.0);
        assert_eq!(cache.info.subscription_title.as_deref(), Some("Kiro Pro"));
        assert_eq!(cache.updated_at_ms, 1_760_300_000_000);
        // 上游没给的可选字段落空，而不是解析失败
        assert!(cache.info.email.is_none());
    }

    #[test]
    fn ms_to_iso_renders_utc_seconds() {
        assert_eq!(ms_to_iso(0).as_deref(), Some("1970-01-01T00:00:00Z"));
        assert_eq!(
            ms_to_iso(1_767_225_600_000).as_deref(),
            Some("2026-01-01T00:00:00Z")
        );
        // 纪元之前也是合法时刻，正常成环而不是落 None
        assert_eq!(
            ms_to_iso(-1).as_deref(),
            Some("1969-12-31T23:59:59Z")
        );
    }

    // ── 登录过的域：最近优先、同域只留一条 ──

    fn site(url: &str, region: &str) -> LoginSite {
        LoginSite {
            start_url: url.to_string(),
            region: region.to_string(),
            used_at_ms: 0,
        }
    }

    #[test]
    fn merge_login_site_moves_a_reused_site_to_the_front() {
        let mut list = vec![
            site("https://a.awsapps.com/start", "us-east-1"),
            site("https://b.awsapps.com/start", "eu-west-1"),
        ];
        merge_login_site(&mut list, site("https://b.awsapps.com/start", "eu-west-1"), 8);
        // 重新用一次就排回最前：下拉的第一条应该是「上次用的那个」
        assert_eq!(list[0].start_url, "https://b.awsapps.com/start");
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn merge_login_site_overwrites_the_region_of_the_same_site() {
        let mut list = vec![site("https://a.awsapps.com/start", "us-east-1")];
        merge_login_site(&mut list, site("https://a.awsapps.com/start", "ap-northeast-1"), 8);
        // 同一个域换过区域时，最后一次成功的才对
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].region, "ap-northeast-1");
    }

    #[test]
    fn merge_login_site_treats_case_trailing_hash_and_slash_as_the_same_site() {
        let mut list = vec![site("https://A.awsapps.com/start/", "us-east-1")];
        merge_login_site(&mut list, site("https://a.awsapps.com/start#", "us-east-1"), 8);
        assert_eq!(list.len(), 1, "{}", list[0].start_url);
    }

    #[test]
    fn merge_login_site_drops_the_least_recently_used_beyond_the_cap() {
        let mut list: Vec<LoginSite> = (0..8)
            .map(|i| site(&format!("https://d{i}.awsapps.com/start"), "us-east-1"))
            .collect();
        merge_login_site(&mut list, site("https://new.awsapps.com/start", "us-east-1"), 8);
        assert_eq!(list.len(), 8);
        assert_eq!(list[0].start_url, "https://new.awsapps.com/start");
        // 最久未用的那条（列表末尾）被挤掉
        assert!(!list.iter().any(|s| s.start_url == "https://d7.awsapps.com/start"));
    }

    /// 旧配置文件里没有 `loginSites`：读到空表，而不是解析失败把整份配置丢掉。
    #[test]
    fn login_sites_default_to_empty_on_old_configs() {
        let config: AppConfig = serde_json::from_str(r#"{"port":6000}"#).unwrap();
        assert_eq!(config.port, 6000);
        assert!(config.login_sites.is_empty());
    }
}
