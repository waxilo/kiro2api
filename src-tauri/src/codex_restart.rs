//! 强制重启 Codex（现名 ChatGPT）桌面应用。
//!
//! Codex 只在启动时读一次配置（config.toml / auth.json / 模型目录），
//! 接入或清空之后不重启不生效 —— 这条命令把「重启」做成一步：
//! 找到 OpenAI 桌面应用的主进程，强制结束整个进程树，再按包装身份重新拉起。
//!
//! 两个刻意的收窄：
//! * 只结束「有窗口 + 进程名 codex*/chatgpt* + 路径属于 OpenAI 桌面应用」的进程：
//!   终端里跑的 codex CLI（没有自己的窗口）与 codex-windows-sandbox-service
//!   这类无窗口辅助进程不会被误伤；
//! * 拉起不走 exe 路径：OpenAI 桌面应用是 MSIX 包（装在 ACL 保护的
//!   WindowsApps 下），直接 spawn 不可靠 —— 解析 AUMID 后走
//!   `explorer shell:appsFolder\<AUMID>`，与「打开数据目录」同款机制。

use std::process::Command;
use std::time::Duration;

use serde::Serialize;

use crate::proc_guard::suppress_console_window;

/// `k2a_codex_restart` 的结果：应用当时在不在跑、有没有完成重启、主进程名。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexRestartOutcome {
    pub running: bool,
    pub restarted: bool,
    pub app: Option<String>,
}

#[tauri::command]
pub async fn k2a_codex_restart() -> Result<CodexRestartOutcome, String> {
    tauri::async_runtime::spawn_blocking(restart_codex_app)
        .await
        .map_err(|e| format!("重启任务失败: {e}"))?
}

fn restart_codex_app() -> Result<CodexRestartOutcome, String> {
    #[cfg(target_os = "windows")]
    {
        let apps = discover_app_processes()?;
        if apps.is_empty() {
            return Ok(CodexRestartOutcome {
                running: false,
                restarted: false,
                app: None,
            });
        }
        let name = apps[0].name.clone();
        kill_app_processes(&apps)?;
        // 文件句柄与单实例锁的释放需要一点时间；立刻拉起会偶发「起了又退」
        std::thread::sleep(Duration::from_millis(800));
        relaunch_app(&apps)?;
        Ok(CodexRestartOutcome {
            running: true,
            restarted: true,
            app: Some(name),
        })
    }
    #[cfg(not(target_os = "windows"))]
    {
        Err("重启 Codex 应用目前只支持 Windows".to_string())
    }
}

/// 一个可识别为「OpenAI 桌面应用」的运行中进程。
#[cfg(target_os = "windows")]
#[derive(Debug, Clone, PartialEq, Eq)]
struct AppProcess {
    pid: u32,
    name: String,
    path: String,
}

/// 有窗口（排除后台服务与无窗辅助进程）+ 名字像 Codex/ChatGPT +
/// 路径属于 OpenAI 桌面应用（MSIX 包目录，或传统的 Programs 安装位置）。
#[cfg(target_os = "windows")]
fn discover_app_processes() -> Result<Vec<AppProcess>, String> {
    let script = r#"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
Get-Process | Where-Object {
  $_.MainWindowTitle -and $_.Path -and
  ($_.ProcessName -match '^(codex|chatgpt)') -and
  ($_.Path -match '\\WindowsApps\\OpenAI\.|\\OpenAI\\(Codex|ChatGPT)\\')
} | ForEach-Object { "{0}`t{1}`t{2}" -f $_.Id, $_.ProcessName, $_.Path }
"#;
    let stdout = run_powershell(script)?;
    Ok(parse_app_processes(&stdout))
}

/// 解析 PowerShell 的「pid\t名字\t路径」行；坏行（取不满三段或 pid 不是数字）直接丢弃。
#[cfg(target_os = "windows")]
fn parse_app_processes(stdout: &str) -> Vec<AppProcess> {
    stdout
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            let pid = parts.next()?.trim().parse::<u32>().ok()?;
            let name = parts.next()?.trim().to_string();
            let path = parts.next()?.trim().to_string();
            if name.is_empty() || path.is_empty() {
                return None;
            }
            Some(AppProcess { pid, name, path })
        })
        .collect()
}

/// /T 连子进程树一起结束（WebView2、runtime 等辅助进程随之退场）；
/// 逐个尝试、只要有一个成功就算数 —— 全军覆没才报错。
#[cfg(target_os = "windows")]
fn kill_app_processes(apps: &[AppProcess]) -> Result<(), String> {
    let mut killed = 0usize;
    let mut last_error = String::new();
    for app in apps {
        let mut cmd = Command::new("taskkill");
        cmd.args(["/F", "/T", "/PID", &app.pid.to_string()]);
        suppress_console_window(&mut cmd);
        match cmd.output() {
            Ok(out) if out.status.success() => killed += 1,
            Ok(out) => {
                last_error = String::from_utf8_lossy(&out.stderr).trim().to_string();
            }
            Err(e) => last_error = e.to_string(),
        }
    }
    if killed == 0 {
        return Err(format!("结束 Codex 进程失败：{last_error}"));
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn relaunch_app(apps: &[AppProcess]) -> Result<(), String> {
    // 优先走包装身份：MSIX 应用的 exe 在 ACL 保护的 WindowsApps 下，直接 spawn 不可靠
    if resolve_app_aumid().is_ok_and(|aumid| spawn_via_aumid(&aumid)) {
        return Ok(());
    }
    // 解析不到 AUMID（例如非商店版安装）再退回 exe 直启；失败时错误里带上路径，
    // 用户至少知道自己可以手动点哪个图标。
    let exe = apps
        .first()
        .map(|app| app.path.clone())
        .ok_or_else(|| "没有可用于重新启动的 Codex 进程信息".to_string())?;
    let mut cmd = Command::new(&exe);
    suppress_console_window(&mut cmd);
    cmd.spawn().map(|_| ()).map_err(|e| {
        format!("已结束 Codex 进程，但重新启动失败：{e}（可手动启动 {exe}）")
    })
}

#[cfg(target_os = "windows")]
fn spawn_via_aumid(aumid: &str) -> bool {
    let mut cmd = Command::new("explorer");
    cmd.arg(format!("shell:appsFolder\\{aumid}"));
    suppress_console_window(&mut cmd);
    cmd.spawn().is_ok()
}

/// 解析 OpenAI 桌面应用的 AUMID。不写死包族名 —— 包名可能换代
/// （OpenAI.Codex → OpenAI.ChatGPT），前缀匹配两代通吃。
#[cfg(target_os = "windows")]
fn resolve_app_aumid() -> Result<String, String> {
    let script = r#"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
Get-StartApps |
  Where-Object { $_.AppID -match '^OpenAI\.(Codex|ChatGPT)_[^!]+!' } |
  Select-Object -First 1 -ExpandProperty AppID
"#;
    let stdout = run_powershell(script)?;
    let id = stdout.trim();
    if id.is_empty() {
        Err("没有找到 ChatGPT（Codex）应用的启动标识".to_string())
    } else {
        Ok(id.to_string())
    }
}

#[cfg(target_os = "windows")]
fn run_powershell(script: &str) -> Result<String, String> {
    let mut cmd = Command::new("powershell");
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", script]);
    suppress_console_window(&mut cmd);
    let output = cmd
        .output()
        .map_err(|e| format!("无法启动 PowerShell：{e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!("PowerShell 查询失败：{stderr}"));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;

    #[test]
    fn parse_reads_pid_name_path_lines() {
        let out = concat!(
            "1234\tChatGPT\tC:\\Program Files\\WindowsApps\\OpenAI.Codex_1.0_x64__pub\\ChatGPT.exe\r\n",
            "5678\tCodex\tC:\\Users\\w\\AppData\\Local\\Programs\\OpenAI\\Codex\\Codex.exe\r\n",
        );
        let apps = parse_app_processes(out);
        assert_eq!(
            apps,
            vec![
                AppProcess {
                    pid: 1234,
                    name: "ChatGPT".to_string(),
                    path: "C:\\Program Files\\WindowsApps\\OpenAI.Codex_1.0_x64__pub\\ChatGPT.exe"
                        .to_string(),
                },
                AppProcess {
                    pid: 5678,
                    name: "Codex".to_string(),
                    path: "C:\\Users\\w\\AppData\\Local\\Programs\\OpenAI\\Codex\\Codex.exe"
                        .to_string(),
                },
            ]
        );
    }

    #[test]
    fn parse_drops_malformed_lines() {
        let out = "not-a-pid\tX\tY\n\n42\tSandbox\t\n\t\t\n99\tCodex\tC:\\a.exe\n";
        assert_eq!(
            parse_app_processes(out),
            vec![AppProcess {
                pid: 99,
                name: "Codex".to_string(),
                path: "C:\\a.exe".to_string(),
            }]
        );
    }
}
