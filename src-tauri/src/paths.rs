//! 应用路径：数据目录、Kiro SSO 缓存目录、诊断日志。
//!
//! 数据目录用于存放 kiro2api 自己的配置（端口、代理密钥、模型偏好、更新清单缓存）。

use std::path::PathBuf;

/// 应用数据目录（Windows: `%APPDATA%\Kiro2API`，macOS: `~/Library/Application Support/Kiro2API`）。
pub(crate) fn get_data_path() -> PathBuf {
    let mut path = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("Kiro2API");
    path
}

/// Kiro IDE / CLI 共用的 SSO 凭据缓存目录：`~/.aws/sso/cache`。
pub(crate) fn kiro_sso_cache_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".aws")
        .join("sso")
        .join("cache")
}

/// kiro-cli（>= 2.31）自己的凭据库：macOS 实测为
/// `~/Library/Application Support/kiro-cli/data.sqlite3`。
/// 只读桥接用，见 kiro::cli_bridge。
pub(crate) fn kiro_cli_db_path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("kiro-cli")
        .join("data.sqlite3")
}

/// 上游请求诊断日志（排查 502 / schema 报错时用）。
pub(crate) fn get_debug_log_path() -> PathBuf {
    get_data_path().join("kiro-debug.log")
}
