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

/// kiro2api 自己登录（设备码流程）留下的**旧版**凭据目录：`数据目录/credentials`。
///
/// 0.11 及以前只支持一个自有登录，位置就写死在这里。账号管理上线后新登录写进
/// [`accounts_root`] 下各自的目录；这个目录只作为「一条既有账号」被扫描与识别，
/// 不搬文件 —— 升级不该让用户重新登录一次。
pub(crate) fn own_credential_dir() -> PathBuf {
    get_data_path().join("credentials")
}

/// 账号凭据根目录：`数据目录/accounts`。
///
/// 布局按来源分层，一个账号一个目录（目录内 = 令牌 + 注册文件 + `account.json`）：
/// - `accounts/login/<clientId 哈希>/`：kiro2api 设备码登录，可并存多个；
/// - `accounts/cli/<哈希>/`：从 kiro-cli 凭据库导入的**副本**（原库只读）。
///
/// Kiro 桌面端（IDE）的账号不在这里 —— 它就地使用 [`kiro_sso_cache_dir`]，
/// 与 IDE 共用同一份令牌，避免两边各自续期把对方顶掉。
pub(crate) fn accounts_root() -> PathBuf {
    get_data_path().join("accounts")
}

/// kiro-cli 自己的凭据库候选路径，按顺序逐个试（先到先用）。
/// 只读桥接用，见 kiro::cli_bridge。
///
/// kiro-cli 把凭据库放在**平台各自的数据目录**，位置并不统一：
/// - macOS(实测 2.31)：`~/Library/Application Support/kiro-cli/data.sqlite3`
/// - Windows(实测 v3)：`%LOCALAPPDATA%\Kiro-Cli\data.sqlite3`
///
/// ⚠️ 曾经只按 `dirs::data_dir()` 拼 `kiro-cli/data.sqlite3`：在 macOS 上
/// `data_dir` 就是 `~/Library/Application Support`，一切正常；但在 Windows 上
/// `data_dir` 是 **Roaming**，而 CLI 实际写在 **Local**，于是桥接永远找不到库、
/// 静默退回 `~/.aws/sso/cache` 里那份可能已过期的旧令牌 —— 表现为「kiro-cli
/// 明明登录着，本应用却要求重新登录」。只测 macOS 时看不出这个洞。
pub(crate) fn kiro_cli_db_paths() -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    // Windows / Linux 的本机数据目录（Windows 上即 %LOCALAPPDATA%）
    if let Some(base) = dirs::data_local_dir() {
        candidates.push(base.join("Kiro-Cli").join("data.sqlite3"));
    }
    // macOS / Linux 的应用数据目录（macOS 上即 ~/Library/Application Support）
    if let Some(base) = dirs::data_dir() {
        let path = base.join("kiro-cli").join("data.sqlite3");
        if !candidates.contains(&path) {
            candidates.push(path);
        }
    }
    candidates
}

/// 上游请求诊断日志（排查 502 / schema 报错时用）。
pub(crate) fn get_debug_log_path() -> PathBuf {
    get_data_path().join("kiro-debug.log")
}
