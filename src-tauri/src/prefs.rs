//! kiro2api 本地配置：端口、代理密钥、模型与额度缓存。
//!
//! 单一事实源是数据目录下的 `config.json`。所有读写都经过 [`update_config`]，
//! 它以进程内互斥锁串行化「读 → 改 → 原子写」，避免 Tauri 命令与后台刷新线程并发时互相覆盖。
//! 代理密钥属于敏感信息，落盘时按最小权限写入（类 Unix 下 0600）。
//!
//! 「模型列表」与「账户额度」都**只**是缓存：它们由上游同步得到，落盘纯粹是为了
//! 让应用一打开就有东西可显示（启动时的后台刷新随后静默替换成最新值）。
//! 真正的模型选择不在这里 —— 客户端在每次请求里自己指定模型。

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
    /// 上一次查询到的账户额度快照。
    pub(crate) usage_cache: Option<UsageCache>,
    /// 应用内更新的下载源。
    ///
    /// `None` 就是「自动」：每次拉清单时并行探测各镜像，挑第一个真能传数据的。
    /// 其余取值是 `updater_accel` 里那份源清单的 id（`direct` 或某个镜像 id）——
    /// 认不出来的值一律按自动处理，这样将来下线某个镜像时，旧配置不会让更新彻底不可用。
    pub(crate) update_source: Option<String>,
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
            usage_cache: None,
            update_source: None,
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

/// 写入模型缓存。
pub(crate) fn store_models(models: &[String]) -> Result<(), String> {
    update_config(|config| {
        config.models = models.to_vec();
        Ok(())
    })
}

/// 读取缓存的额度快照（纯本地读取）。从未查询过时为 `None`。
pub(crate) fn cached_usage() -> Option<KiroUsageInfo> {
    load_config().usage_cache.map(|cache| cache.info)
}

/// 写入额度快照（连同写入时刻）。
pub(crate) fn store_usage(info: &KiroUsageInfo) -> Result<(), String> {
    update_config(|config| {
        config.usage_cache = Some(UsageCache {
            info: info.clone(),
            updated_at_ms: now_ms(),
        });
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
}
