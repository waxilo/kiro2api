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

/// kiro2api 自己登录（设备码流程）得到的凭据目录：`数据目录/credentials`。
/// 文件格式与 `~/.aws/sso/cache` 完全一致（令牌文件 + `<clientIdHash>.json` 注册文件），
/// 所以 [`crate::kiro::auth::Auth`] 的读取与刷新链路不用区分来源。
/// 单独放一处是为了不覆盖 Kiro IDE 的登录：两边各自刷新同一个 refreshToken 会互相顶掉。
pub(crate) fn own_credential_dir() -> PathBuf {
    get_data_path().join("credentials")
}

/// 是否有 kiro2api 自有登录。
pub(crate) fn has_own_login() -> bool {
    own_credential_dir().join("kiro-auth-token.json").is_file()
}

/// 当前生效的凭据目录：自有登录优先，否则用 IDE / CLI 共用的 SSO 缓存。
pub(crate) fn active_credential_dir() -> PathBuf {
    if has_own_login() {
        own_credential_dir()
    } else {
        kiro_sso_cache_dir()
    }
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
