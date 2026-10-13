//! kiro2api 本地配置：端口、代理密钥、模型偏好、更新清单缓存。
//!
//! 单一事实源是数据目录下的 `config.json`。所有读写都经过 [`update_config`]，
//! 它以进程内互斥锁串行化「读 → 改 → 原子写」，避免 Tauri 命令与后台刷新线程并发时互相覆盖。
//! 代理密钥属于敏感信息，落盘时按最小权限写入（类 Unix 下 0600）。

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use serde::{Deserialize, Serialize};

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
    /// 应用启动时是否自动拉起代理。
    pub(crate) auto_start: bool,
    /// 上次退出时代理是否处于运行态。用于「自动启动」开启时恢复上次状态。
    pub(crate) last_running: bool,
    /// 当前默认模型（写入 `/v1/models` 之外的默认值，并作为接入配置里的建议值）。
    pub(crate) default_model: String,
    /// 从 Kiro 上游同步到的可用模型。
    pub(crate) display_models: Vec<String>,
    /// 用户手工补充的模型 ID。
    pub(crate) custom_models: Vec<String>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            proxy_api_key: None,
            auto_start: false,
            last_running: false,
            default_model: String::new(),
            display_models: Vec::new(),
            custom_models: Vec::new(),
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
pub(crate) fn mask_api_key(key: &str) -> String {
    let trimmed = key.trim();
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
        assert!(!config.auto_start);
        assert!(config.proxy_api_key.is_none());
    }

    #[test]
    fn config_roundtrip_keeps_unknown_fields_out_but_known_fields_in() {
        let json = r#"{"port":6000,"proxyApiKey":"k2a-x","defaultModel":"claude-sonnet-5"}"#;
        let config: AppConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.port, 6000);
        assert_eq!(config.proxy_api_key.as_deref(), Some("k2a-x"));
        assert_eq!(config.default_model, "claude-sonnet-5");
        // 未提供的字段落到默认值，而不是解析失败
        assert!(config.display_models.is_empty());
    }
}
