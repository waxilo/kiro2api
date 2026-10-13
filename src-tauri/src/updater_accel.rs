//! 本机更新清单代理：绕开国内直连 GitHub Releases 超时。
//!
//! 流程：
//! 1. 应用启动后在 `127.0.0.1:47866` 提供 `/latest.json`
//! 2. 优先直连 GitHub 拉官方 `latest.json`；直连不可达时回退镜像拉取
//! 3. 每次请求并行探测镜像，把清单里 `platforms.*.url` 改写为最快可用镜像
//! 4. `tauri-plugin-updater` 请求本机清单，资产下载走镜像 URL
//!
//! 安全说明：更新包（含签名）最终仍由 `tauri-plugin-updater` 用内嵌 pubkey 做 minisign 校验。
//! 清单即便被镜像篡改，最坏结果也只是「下载失败」，无法注入任意代码。

use std::io::Read as _;
use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use reqwest::blocking::Client;
use serde_json::Value;
use tiny_http::{Header, Request, Response, Server, StatusCode};

/// 与 kiro2api 主代理（默认 5680）分开，且避开 ccm 的 47865，两应用可同时运行。
pub(crate) const LISTEN_ADDR: &str = "127.0.0.1:47866";

/// 官方清单地址。本仓库为单产品仓库，`/releases/latest` 恒指向最新一次发布。
const UPSTREAM_LATEST_JSON: &str =
    "https://github.com/waxilo/kiro2api/releases/latest/download/latest.json";

/// 直连 GitHub 拉清单的超时；宁可快速失败，把时间留给镜像兜底。
const DIRECT_FETCH_TIMEOUT: Duration = Duration::from_secs(8);
/// 单个镜像拉取清单 / 探测的超时。
const MIRROR_FETCH_TIMEOUT: Duration = Duration::from_secs(6);
/// 等待任一镜像返回清单的最长时间。
const MANIFEST_FALLBACK_WAIT: Duration = Duration::from_secs(10);
/// 等待任一镜像探测结果的最长时间。
const PROBE_WAIT: Duration = Duration::from_secs(10);
/// 清单文本缓存时长：版本元数据短期不变，避免每次检查都重打 GitHub。
/// 镜像探测不缓存 —— 下载停滞重试时会重新探测，从而换到可用镜像。
const MANIFEST_CACHE_TTL: Duration = Duration::from_secs(300);

/// 按优先级排列的镜像前缀（完整 URL = 前缀 + 原始 GitHub URL）。
const MIRROR_PREFIXES: &[&str] = &[
    "https://ghfast.top/",
    "https://gh-proxy.com/",
    "https://mirror.ghproxy.com/",
];

struct CachedManifest {
    text: String,
    fetched_at: Instant,
}

static MANIFEST_CACHE: OnceLock<Mutex<Option<CachedManifest>>> = OnceLock::new();

fn manifest_cache() -> &'static Mutex<Option<CachedManifest>> {
    MANIFEST_CACHE.get_or_init(|| Mutex::new(None))
}

fn cached_manifest_text() -> Option<String> {
    let guard = manifest_cache().lock().ok()?;
    let cached = guard.as_ref()?;
    (cached.fetched_at.elapsed() < MANIFEST_CACHE_TTL).then(|| cached.text.clone())
}

fn store_manifest_text(text: String) {
    if let Ok(mut guard) = manifest_cache().lock() {
        *guard = Some(CachedManifest {
            text,
            fetched_at: Instant::now(),
        });
    }
}

static STARTED: OnceLock<bool> = OnceLock::new();

/// 启动本地清单代理（幂等）。失败只打日志，不阻断应用启动。
pub(crate) fn start_manifest_proxy() {
    if STARTED.set(true).is_err() {
        return;
    }

    thread::spawn(|| {
        let server = match Server::http(LISTEN_ADDR) {
            Ok(server) => server,
            Err(error) => {
                eprintln!("[updater] 本机清单代理绑定 {LISTEN_ADDR} 失败: {error}");
                return;
            }
        };
        eprintln!("[updater] 本机清单代理已监听 http://{LISTEN_ADDR}");

        let client = match Client::builder()
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::limited(8))
            .build()
        {
            Ok(client) => client,
            Err(error) => {
                eprintln!("[updater] HTTP 客户端创建失败: {error}");
                return;
            }
        };

        for request in server.incoming_requests() {
            // respond() 会取走 Request 的所有权，因此按值传入
            handle_request(request, &client);
        }
    });
}

fn handle_request(request: Request, client: &Client) {
    let path = request.url().split('?').next().unwrap_or("/").to_string();

    // Host 校验：仅接受本机回环地址，阻断 DNS rebinding
    let host_ok = request
        .headers()
        .iter()
        .find(|header| header.field.equiv("host"))
        .map(|header| is_loopback_host(header.value.as_str()))
        .unwrap_or(false);
    if !host_ok {
        respond_json(request, StatusCode(400), r#"{"error":"invalid host"}"#, Vec::new());
        return;
    }

    if request.method() == &tiny_http::Method::Options {
        let mut headers = vec![
            Header::from_bytes("Access-Control-Allow-Methods", "GET,OPTIONS").expect("valid header"),
            Header::from_bytes("Access-Control-Allow-Headers", "content-type").expect("valid header"),
        ];
        headers.extend(cors_headers_for(&request));
        let mut response = Response::from_string("").with_status_code(StatusCode(204));
        for header in headers {
            response = response.with_header(header);
        }
        let _ = request.respond(response);
        return;
    }

    if path != "/latest.json" && path != "/" {
        let cors = cors_headers_for(&request);
        respond_json(request, StatusCode(404), r#"{"error":"not found"}"#, cors);
        return;
    }

    match fetch_and_rewrite_manifest(client) {
        Ok(json) => {
            let cors = cors_headers_for(&request);
            let mut response = Response::from_string(json)
                .with_status_code(StatusCode(200))
                .with_header(json_header())
                .with_header(Header::from_bytes("Cache-Control", "no-store").expect("valid header"));
            for header in cors {
                response = response.with_header(header);
            }
            let _ = request.respond(response);
        }
        Err(error) => {
            eprintln!("[updater] 拉取更新清单失败: {error}");
            let body = format!(
                r#"{{"error":{}}}"#,
                serde_json::to_string(&error).unwrap_or_else(|_| "\"error\"".into())
            );
            let cors = cors_headers_for(&request);
            respond_json(request, StatusCode(502), &body, cors);
        }
    }
}

fn respond_json(request: Request, code: StatusCode, body: &str, extra: Vec<Header>) {
    let mut response = Response::from_string(body)
        .with_status_code(code)
        .with_header(json_header());
    for header in extra {
        response = response.with_header(header);
    }
    let _ = request.respond(response);
}

fn json_header() -> Header {
    Header::from_bytes("Content-Type", "application/json; charset=utf-8").expect("valid header")
}

/// 提取 Host 头里的裸主机名（去掉端口与 IPv6 方括号）。
fn bare_host(host: &str) -> String {
    let host = host.trim();
    if let Some(rest) = host.strip_prefix('[') {
        rest.split(']').next().unwrap_or(rest).to_string()
    } else {
        host.split(':').next().unwrap_or(host).to_string()
    }
}

fn is_loopback_host(host: &str) -> bool {
    matches!(
        bare_host(host).as_str(),
        "127.0.0.1" | "localhost" | "::1" | "0:0:0:0:0:0:0:1"
    )
}

fn request_origin(request: &Request) -> Option<&str> {
    request
        .headers()
        .iter()
        .find(|header| header.field.equiv("origin"))
        .map(|header| header.value.as_str())
}

fn is_loopback_origin(origin: &str) -> bool {
    origin == "tauri://localhost"
        || origin == "http://tauri.localhost"
        || origin == "https://tauri.localhost"
        || origin.starts_with("http://localhost:")
        || origin.starts_with("https://localhost:")
        || origin.starts_with("http://127.0.0.1:")
        || origin.starts_with("https://127.0.0.1:")
        || origin.starts_with("http://[::1]:")
        || origin.starts_with("https://[::1]:")
}

/// 仅对本机来源回显 CORS 头；原生客户端（无 Origin）不返回任何 CORS 头。
fn cors_headers_for(request: &Request) -> Vec<Header> {
    let Some(origin) = request_origin(request) else {
        return Vec::new();
    };
    if !is_loopback_origin(origin) {
        return Vec::new();
    }
    vec![
        Header::from_bytes("Access-Control-Allow-Origin", origin).expect("valid header"),
        Header::from_bytes("Vary", "Origin").expect("valid header"),
    ]
}

fn fetch_and_rewrite_manifest(client: &Client) -> Result<String, String> {
    let text = match cached_manifest_text() {
        Some(text) => text,
        None => {
            let text = fetch_manifest_text(client)?;
            store_manifest_text(text.clone());
            text
        }
    };
    let mut value: Value =
        serde_json::from_str(&text).map_err(|e| format!("官方 latest.json 无效: {e}"))?;
    if let Some(prefix) = pick_mirror_prefix(client) {
        rewrite_platform_urls(&mut value, prefix);
    }
    // 所有镜像都不可用时不改写，保持直连 GitHub 作为兜底
    serde_json::to_string(&value).map_err(|e| e.to_string())
}

/// 镜像上的清单地址（完整 URL = 镜像前缀 + 原始 GitHub URL）。
fn mirror_manifest_urls() -> Vec<String> {
    MIRROR_PREFIXES
        .iter()
        .map(|prefix| format!("{prefix}{UPSTREAM_LATEST_JSON}"))
        .collect()
}

/// 拉取官方清单：直连优先，失败则并行试镜像，首个成功者胜出。
fn fetch_manifest_text(client: &Client) -> Result<String, String> {
    match fetch_url(client, UPSTREAM_LATEST_JSON, DIRECT_FETCH_TIMEOUT) {
        Ok(text) => return Ok(text),
        Err(error) => eprintln!("[updater] 直连 GitHub 清单失败，回退镜像: {error}"),
    }

    let (tx, rx) = mpsc::channel::<Result<String, String>>();
    for url in mirror_manifest_urls() {
        let client = client.clone();
        let tx = tx.clone();
        // 不 join：线程带自身超时，收到首个成功即返回，其余线程自灭，避免慢镜像拖慢请求。
        thread::spawn(move || {
            let result = fetch_url(&client, &url, MIRROR_FETCH_TIMEOUT);
            let _ = tx.send(result);
        });
    }
    drop(tx);

    let mut last_error: Option<String> = None;
    let deadline = Instant::now() + MANIFEST_FALLBACK_WAIT;
    for _ in 0..MIRROR_PREFIXES.len() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining) {
            Ok(Ok(text)) => return Ok(text),
            Ok(Err(error)) => last_error = Some(error),
            Err(_) => break,
        }
    }
    Err(last_error.unwrap_or_else(|| "等待镜像清单超时".to_string()))
}

fn fetch_url(client: &Client, url: &str, timeout: Duration) -> Result<String, String> {
    let response = client
        .get(url)
        .timeout(timeout)
        .send()
        .map_err(|e| format!("请求 {url} 失败: {e}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("{url} 返回 HTTP {status}"));
    }
    response
        .text()
        .map_err(|e| format!("读取 {url} 响应失败: {e}"))
}

/// 并行探测所有镜像，返回第一个能**真实传输内容**的镜像前缀。
///
/// 每次清单请求都重新探测（不缓存），保证前端下载停滞重试时能换镜像；
/// 全部失败返回 `None`，调用方保持直连 GitHub 兜底。
fn pick_mirror_prefix(client: &Client) -> Option<&'static str> {
    if MIRROR_PREFIXES.is_empty() {
        return None;
    }
    let (tx, rx) = mpsc::channel::<&'static str>();
    for prefix in MIRROR_PREFIXES.iter().copied() {
        let client = client.clone();
        let tx = tx.clone();
        thread::spawn(move || {
            if probe_mirror(&client, prefix) {
                let _ = tx.send(prefix);
            }
        });
    }
    drop(tx);
    rx.recv_timeout(PROBE_WAIT).ok()
}

/// 真实内容传输探测：对清单路径发 `Range` 请求，必须实际读到 ≥1KB 才算可用。
/// 仅 TCP 连通但传输停滞的镜像（曾实际遇到）会被这里淘汰。
fn probe_mirror(client: &Client, prefix: &str) -> bool {
    let probe_url = format!("{prefix}{UPSTREAM_LATEST_JSON}");
    let Ok(response) = client
        .get(&probe_url)
        .header(reqwest::header::RANGE, "bytes=0-2048")
        .timeout(MIRROR_FETCH_TIMEOUT)
        .send()
    else {
        return false;
    };
    let mut buf = Vec::with_capacity(2048);
    matches!(response.take(2048).read_to_end(&mut buf), Ok(n) if n >= 1024)
}

fn rewrite_platform_urls(manifest: &mut Value, mirror_prefix: &str) {
    let Some(platforms) = manifest.get_mut("platforms").and_then(|v| v.as_object_mut()) else {
        return;
    };
    for platform in platforms.values_mut() {
        let Some(url) = platform
            .get("url")
            .and_then(|v| v.as_str())
            .map(str::to_string)
        else {
            continue;
        };
        let rewritten = rewrite_github_url(&url, mirror_prefix);
        if let Some(object) = platform.as_object_mut() {
            object.insert("url".to_string(), Value::String(rewritten));
        }
    }
}

fn rewrite_github_url(url: &str, mirror_prefix: &str) -> String {
    let trimmed = url.trim();
    if trimmed.is_empty() || mirror_prefix.is_empty() {
        return trimmed.to_string();
    }
    // 已是镜像地址则不重复包裹
    if MIRROR_PREFIXES
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
    {
        return trimmed.to_string();
    }
    if trimmed.starts_with("https://github.com/")
        || trimmed.starts_with("https://objects.githubusercontent.com/")
        || trimmed.starts_with("https://release-assets.githubusercontent.com/")
    {
        return format!("{mirror_prefix}{trimmed}");
    }
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ARTIFACT: &str = "https://github.com/waxilo/kiro2api/releases/download/v0.1.0/x.tar.gz";

    #[test]
    fn upstream_manifest_only_points_at_trusted_github() {
        assert!(UPSTREAM_LATEST_JSON.starts_with("https://github.com/waxilo/kiro2api/"));
        assert!(UPSTREAM_LATEST_JSON.ends_with("/latest.json"));
        assert!(!MIRROR_PREFIXES
            .iter()
            .any(|prefix| UPSTREAM_LATEST_JSON.starts_with(prefix)));
    }

    #[test]
    fn rewrites_github_platform_urls() {
        let mut manifest = json!({
            "version": "0.1.1",
            "platforms": { "darwin-aarch64": { "signature": "sig", "url": ARTIFACT } }
        });
        rewrite_platform_urls(&mut manifest, "https://ghfast.top/");
        assert_eq!(
            manifest["platforms"]["darwin-aarch64"]["url"],
            format!("https://ghfast.top/{ARTIFACT}")
        );
    }

    #[test]
    fn does_not_double_wrap_mirrored_urls() {
        let already = format!("https://ghfast.top/{ARTIFACT}");
        assert_eq!(rewrite_github_url(&already, "https://ghfast.top/"), already);
    }

    #[test]
    fn empty_prefix_is_a_noop_so_direct_github_survives() {
        assert_eq!(rewrite_github_url(ARTIFACT, ""), ARTIFACT);
    }

    #[test]
    fn non_github_urls_are_left_alone() {
        let other = "https://example.com/x.tar.gz";
        assert_eq!(rewrite_github_url(other, "https://ghfast.top/"), other);
    }

    #[test]
    fn loopback_hosts_accepted_foreign_rejected() {
        assert!(is_loopback_host("127.0.0.1:47866"));
        assert!(is_loopback_host("localhost:47866"));
        assert!(is_loopback_host("[::1]:47866"));
        assert!(!is_loopback_host("evil.example.com"));
        assert!(!is_loopback_host("evil.example.com:47866"));
        assert!(!is_loopback_host(""));
    }

    #[test]
    fn cors_headers_only_for_loopback_origins() {
        assert!(is_loopback_origin("http://localhost:1420"));
        assert!(is_loopback_origin("tauri://localhost"));
        assert!(!is_loopback_origin("https://evil.example.com"));
        assert!(!is_loopback_origin(""));
    }

    #[test]
    fn mirror_manifest_urls_wrap_upstream_with_each_mirror() {
        let urls = mirror_manifest_urls();
        assert_eq!(urls.len(), MIRROR_PREFIXES.len());
        for url in &urls {
            assert!(
                MIRROR_PREFIXES.iter().any(|prefix| url.starts_with(prefix)),
                "{url} 应带镜像前缀"
            );
            assert!(url.ends_with(UPSTREAM_LATEST_JSON));
        }
    }

    #[test]
    fn manifest_cache_roundtrip_within_ttl() {
        store_manifest_text("cached-json".to_string());
        assert_eq!(cached_manifest_text().as_deref(), Some("cached-json"));
        if let Ok(mut guard) = manifest_cache().lock() {
            *guard = None;
        }
    }
}
