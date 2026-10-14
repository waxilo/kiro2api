//! 本机更新清单代理：绕开国内直连 GitHub Releases 超时。
//!
//! 流程：
//! 1. 应用启动后在 `127.0.0.1:47866` 提供 `/latest.json`
//! 2. 优先直连 GitHub 拉官方 `latest.json`；直连不可达时回退镜像拉取
//! 3. 按用户选定的**下载源**决定把清单里 `platforms.*.url` 改写成哪个镜像
//!    （默认「自动」= 每次并行探测、挑最快可用；也可以固定某个镜像或直连官方）
//! 4. `tauri-plugin-updater` 请求本机清单，资产下载走改写后的 URL
//!
//! 安全说明：更新包（含签名）最终仍由 `tauri-plugin-updater` 用内嵌 pubkey 做 minisign 校验。
//! 清单即便被镜像篡改，最坏结果也只是「下载失败」，无法注入任意代码。

use std::io::Read as _;
use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use reqwest::blocking::Client;
use serde::Serialize;
use serde_json::Value;
use tiny_http::{Header, Request, Response, Server, StatusCode};

use crate::prefs::{store_update_source, update_source_pref};

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

// ── 下载源 ──────────────────────────────────────────────────────────────
//
// 「下载源」是用户可见的概念，也是配置里存的那个值。三个来源合成一个下拉框：
// 自动（默认）、直连官方、以及各个 GitHub 反代。id 一旦写进 `config.json` 就不能再改，
// 否则升级一次就把用户的偏好丢了。

/// 一个可选的 GitHub 反代。
pub(crate) struct MirrorSpec {
    /// 配置里存的值，也是前端下拉框的 value。
    id: &'static str,
    /// 界面上显示的名字。
    label: &'static str,
    /// 完整 URL = 前缀 + 原始 GitHub URL。
    prefix: &'static str,
    /// 下拉框里那句补充说明。
    hint: &'static str,
}

/// 按优先级排列的镜像。**顺序即自动模式的探测顺序**（先返回的那个胜出）。
const MIRRORS: &[MirrorSpec] = &[
    MirrorSpec {
        id: "ghfast",
        label: "ghfast.top",
        prefix: "https://ghfast.top/",
        hint: "GitHub 反代，国内一般直连可用",
    },
    MirrorSpec {
        id: "ghproxy",
        label: "gh-proxy.com",
        prefix: "https://gh-proxy.com/",
        hint: "GitHub 反代，备用线路",
    },
    MirrorSpec {
        id: "mirror-ghproxy",
        label: "mirror.ghproxy.com",
        prefix: "https://mirror.ghproxy.com/",
        hint: "GitHub 反代，备用线路",
    },
];

/// 「自动」的 id。它就是没配过时的行为，因此也是配置缺省值。
const SOURCE_AUTO: &str = "auto";
/// 「直连 GitHub」的 id。
const SOURCE_DIRECT: &str = "direct";
/// 清单里没有任何镜像可用时实际走的也是直连，`last_pick` 里用这个名字。
const DIRECT_LABEL: &str = "GitHub 直连";

/// 用户选定的下载源，解析后的形状。
#[derive(Clone, Copy)]
enum Source {
    /// 并行探测全部镜像，挑第一个真能传数据的（默认）。
    Auto,
    /// 不改写下载地址。这是用户明确选的，所以失败就如实失败，不偷偷换成镜像。
    Direct,
    /// 固定某个镜像；它不可用时回退到别的源，并把「回退了」这件事告诉界面。
    Pinned(&'static MirrorSpec),
}

fn mirror_by_id(id: &str) -> Option<&'static MirrorSpec> {
    MIRRORS.iter().find(|mirror| mirror.id == id)
}

/// 配置值 → 下载源。认不出的值和没配过一样按「自动」处理。
fn resolve_source(configured: Option<&str>) -> Source {
    match configured.map(str::trim) {
        Some(SOURCE_DIRECT) => Source::Direct,
        Some(id) => match mirror_by_id(id) {
            Some(mirror) => Source::Pinned(mirror),
            None => {
                eprintln!("[updater] 未知的下载源 {id:?}，按自动处理");
                Source::Auto
            }
        },
        None => Source::Auto,
    }
}

/// 校验并归一化前端传来的下载源 id（用于写入配置前）。
fn canonical_source_id(id: &str) -> Result<&str, String> {
    let trimmed = id.trim();
    if trimmed == SOURCE_AUTO || trimmed == SOURCE_DIRECT {
        return Ok(trimmed);
    }
    mirror_by_id(trimmed)
        .map(|mirror| mirror.id)
        .ok_or_else(|| format!("未知的下载源: {trimmed}"))
}

/// 最近一次清单请求实际用的是哪个源。
///
/// 只为一件事存在：用户固定了某个镜像、它却不可用时要**说出来**。否则界面显示「首选
/// ghfast.top」，实际却在走另一个镜像，用户会以为自己选的没生效，或者反过来以为是坏的。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LastPick {
    /// 实际使用的源：镜像 id 或 `direct`。
    pub(crate) source_id: String,
    /// 界面上显示的名字。
    pub(crate) label: String,
    /// 用户选的首选源没能用上（选了固定镜像却回退了）。
    pub(crate) fallback: bool,
}

static LAST_PICK: OnceLock<Mutex<Option<LastPick>>> = OnceLock::new();

fn last_pick_slot() -> &'static Mutex<Option<LastPick>> {
    LAST_PICK.get_or_init(|| Mutex::new(None))
}

fn record_pick(source_id: &str, label: &str, fallback: bool) {
    if let Ok(mut guard) = last_pick_slot().lock() {
        *guard = Some(LastPick {
            source_id: source_id.to_string(),
            label: label.to_string(),
            fallback,
        });
    }
}

/// 读取「最近一次实际使用的源」。从未请求过时为 `None`。
fn last_pick() -> Option<LastPick> {
    last_pick_slot().lock().ok()?.clone()
}

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

/// 丢掉清单缓存与「本次使用」记录。**切换下载源时必须调用** ——
/// 缓存里存的是「已按旧源改写好的 JSON」，不清的话用户切完源最长 5 分钟内看到的仍是旧线路。
fn clear_cached_manifest() {
    if let Ok(mut guard) = manifest_cache().lock() {
        *guard = None;
    }
    if let Ok(mut guard) = last_pick_slot().lock() {
        *guard = None;
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

    // 每次请求都重新读配置：用户在「应用更新」里改了下载源，下一次检查就该生效，
    // 不必重启应用。（切换本身还会清掉清单缓存，见 `k2a_update_source_set`。）
    let source = resolve_source(update_source_pref().as_deref());
    match fetch_and_rewrite_manifest(client, source) {
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

/// 拉官方清单，按用户选定的下载源改写里面的下载地址。
///
/// 三种源的行为差别就在这一步：
/// * 自动：并行探测，挑一个真能传数据的镜像改写（探测不到就保持直连地址）
/// * 直连：原样返回，一个字节都不改 —— 用户明确要求走官方，就不该悄悄给他套代理
/// * 固定镜像：先用它；它不可用才回退，并且**把回退记录下来**给界面看
fn fetch_and_rewrite_manifest(client: &Client, source: Source) -> Result<String, String> {
    let text = match cached_manifest_text() {
        Some(text) => text,
        None => {
            let text = fetch_manifest_text(client, source)?;
            store_manifest_text(text.clone());
            text
        }
    };
    let mut value: Value =
        serde_json::from_str(&text).map_err(|e| format!("官方 latest.json 无效: {e}"))?;

    match source {
        Source::Direct => record_pick(SOURCE_DIRECT, DIRECT_LABEL, false),
        Source::Auto => match pick_mirror(client) {
            Some(mirror) => {
                record_pick(mirror.id, mirror.label, false);
                rewrite_platform_urls(&mut value, mirror.prefix);
            }
            // 所有镜像都不可用时不改写，保持直连 GitHub 作为兜底
            None => record_pick(SOURCE_DIRECT, DIRECT_LABEL, false),
        },
        Source::Pinned(preferred) => {
            if probe_mirror(client, preferred.prefix) {
                record_pick(preferred.id, preferred.label, false);
                rewrite_platform_urls(&mut value, preferred.prefix);
            } else if let Some(fallback) = pick_mirror(client) {
                // 首选不可用 → 换一个能用的，但要把「换过了」标出来（fallback = true）
                record_pick(fallback.id, fallback.label, true);
                rewrite_platform_urls(&mut value, fallback.prefix);
            } else {
                record_pick(SOURCE_DIRECT, DIRECT_LABEL, true);
            }
        }
    }

    serde_json::to_string(&value).map_err(|e| e.to_string())
}

/// 某个镜像上的清单地址（完整 URL = 镜像前缀 + 原始 GitHub URL）。
fn mirror_manifest_url(prefix: &str) -> String {
    format!("{prefix}{UPSTREAM_LATEST_JSON}")
}

/// 拉取官方清单，按下载源决定先试哪条路。
fn fetch_manifest_text(client: &Client, source: Source) -> Result<String, String> {
    match source {
        // 用户选了直连：只走官方地址。失败了如实报错，不偷偷换成镜像 ——
        // 换成镜像虽然大概能成功，但那不是他要的（例如他可能正挂着公司代理）。
        Source::Direct => fetch_url(client, UPSTREAM_LATEST_JSON, DIRECT_FETCH_TIMEOUT),
        // 固定镜像时先只试那一个：选它的人通常正是直连不通，
        // 没必要让他先干等 8 秒的直连超时；失败再按默认顺序兜底。
        Source::Pinned(mirror) => {
            match fetch_url(
                client,
                &mirror_manifest_url(mirror.prefix),
                MIRROR_FETCH_TIMEOUT,
            ) {
                Ok(text) => Ok(text),
                Err(error) => {
                    eprintln!(
                        "[updater] 首选镜像 {} 拉清单失败，回退默认顺序: {error}",
                        mirror.id
                    );
                    fetch_manifest_default(client)
                }
            }
        }
        Source::Auto => fetch_manifest_default(client),
    }
}

/// 默认顺序：直连优先，失败则并行试镜像，首个成功者胜出。
fn fetch_manifest_default(client: &Client) -> Result<String, String> {
    match fetch_url(client, UPSTREAM_LATEST_JSON, DIRECT_FETCH_TIMEOUT) {
        Ok(text) => return Ok(text),
        Err(error) => eprintln!("[updater] 直连 GitHub 清单失败，回退镜像: {error}"),
    }

    let (tx, rx) = mpsc::channel::<Result<String, String>>();
    for mirror in MIRRORS {
        let url = mirror_manifest_url(mirror.prefix);
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
    for _ in 0..MIRRORS.len() {
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

/// 并行探测所有镜像，返回第一个能**真实传输内容**的那个。
///
/// 每次清单请求都重新探测（不缓存），保证前端下载停滞重试时能换镜像；
/// 全部失败返回 `None`，调用方保持直连 GitHub 兜底。
fn pick_mirror(client: &Client) -> Option<&'static MirrorSpec> {
    if MIRRORS.is_empty() {
        return None;
    }
    let (tx, rx) = mpsc::channel::<&'static MirrorSpec>();
    for mirror in MIRRORS {
        let client = client.clone();
        let tx = tx.clone();
        thread::spawn(move || {
            if probe_mirror(&client, mirror.prefix) {
                let _ = tx.send(mirror);
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
    if MIRRORS
        .iter()
        .any(|mirror| trimmed.starts_with(mirror.prefix))
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

// ── Tauri 命令 ──────────────────────────────────────────────────────────

/// 下拉框里的一个选项。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateSourceOption {
    pub(crate) id: &'static str,
    pub(crate) label: &'static str,
    pub(crate) hint: &'static str,
}

/// 「应用更新」分区需要的下载源全貌：当前选择 + 可选项 + 本次实际走的源。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateSourceState {
    pub(crate) current: String,
    pub(crate) options: Vec<UpdateSourceOption>,
    /// 最近一次清单请求实际用的源；还没请求过时为 `None`。
    pub(crate) last: Option<LastPick>,
}

fn source_options() -> Vec<UpdateSourceOption> {
    let mut options = vec![
        UpdateSourceOption {
            id: SOURCE_AUTO,
            label: "自动（探测最快）",
            hint: "每次检查时并行探测各镜像，挑第一个真能传数据的；都不可用则走 GitHub 直连",
        },
        UpdateSourceOption {
            id: SOURCE_DIRECT,
            label: DIRECT_LABEL,
            hint: "不改写下载地址，直接走 GitHub 官方。网络不通时会失败（不会偷偷换成镜像）",
        },
    ];
    options.extend(MIRRORS.iter().map(|mirror| UpdateSourceOption {
        id: mirror.id,
        label: mirror.label,
        hint: mirror.hint,
    }));
    options
}

fn current_source_state() -> UpdateSourceState {
    // 存的值认不出来时对外显示「自动」—— 与 `resolve_source` 的兜底保持一致，
    // 否则界面会显示一个实际并不存在的源。
    let current = match resolve_source(update_source_pref().as_deref()) {
        Source::Direct => SOURCE_DIRECT.to_string(),
        Source::Pinned(mirror) => mirror.id.to_string(),
        Source::Auto => SOURCE_AUTO.to_string(),
    };
    UpdateSourceState {
        current,
        options: source_options(),
        last: last_pick(),
    }
}

/// 读取当前的下载源设置。
#[tauri::command]
pub(crate) fn k2a_update_source_get() -> UpdateSourceState {
    current_source_state()
}

/// 切换下载源并落盘；返回切换后的完整状态。
///
/// 顺带清掉清单缓存：缓存里是「已按旧源改写好的 JSON」，不清的话用户会看到自己刚选完
/// 却什么变化都没有（最长可以持续一个缓存周期）。
#[tauri::command]
pub(crate) fn k2a_update_source_set(id: String) -> Result<UpdateSourceState, String> {
    let normalized = canonical_source_id(&id)?;
    store_update_source(normalized)?;
    clear_cached_manifest();
    Ok(current_source_state())
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
        assert!(!MIRRORS
            .iter()
            .any(|mirror| UPSTREAM_LATEST_JSON.starts_with(mirror.prefix)));
    }

    #[test]
    fn source_ids_are_unique_and_resolvable() {
        let mut ids: Vec<&str> = MIRRORS.iter().map(|mirror| mirror.id).collect();
        ids.push(SOURCE_AUTO);
        ids.push(SOURCE_DIRECT);
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "下载源 id 不能重名");
        for id in ids {
            assert_eq!(canonical_source_id(id).unwrap(), id);
        }
    }

    #[test]
    fn unknown_source_falls_back_to_auto() {
        // 认不出来就当自动：将来下线某个镜像时，旧配置不该让更新彻底不可用
        assert!(matches!(resolve_source(Some("no-such-mirror")), Source::Auto));
        assert!(matches!(resolve_source(None), Source::Auto));
        assert!(canonical_source_id("no-such-mirror").is_err());
    }

    #[test]
    fn each_source_option_is_resolvable_from_its_own_id() {
        // 下拉框里的每一项都必须能被 `resolve_source` 认出来，
        // 否则「选了却没生效」会是个静默的坑
        for option in source_options() {
            let resolved = resolve_source(Some(option.id));
            let echoed = match resolved {
                Source::Direct => SOURCE_DIRECT,
                Source::Pinned(mirror) => mirror.id,
                Source::Auto => SOURCE_AUTO,
            };
            assert_eq!(echoed, option.id);
        }
    }

    #[test]
    fn direct_source_is_recognized_and_pinned_mirror_carries_its_prefix() {
        assert!(matches!(resolve_source(Some("direct")), Source::Direct));
        match resolve_source(Some("ghproxy")) {
            Source::Pinned(mirror) => {
                assert_eq!(mirror.prefix, "https://gh-proxy.com/");
                assert_eq!(mirror_manifest_url(mirror.prefix), format!("{}{UPSTREAM_LATEST_JSON}", mirror.prefix));
            }
            _ => panic!("ghproxy 应该解析成固定镜像"),
        }
    }

    #[test]
    fn last_pick_records_which_source_actually_ran() {
        clear_cached_manifest();
        assert!(last_pick().is_none());
        record_pick("ghfast", "ghfast.top", false);
        let pick = last_pick().expect("刚记录过");
        assert_eq!(pick.source_id, "ghfast");
        assert!(!pick.fallback);
        // 切换下载源时必须把这条记录一起丢掉，否则界面会拿旧线路的结果解释新选择
        clear_cached_manifest();
        assert!(last_pick().is_none());
    }

    #[test]
    fn clearing_the_cache_drops_the_stored_manifest() {
        store_manifest_text("cached-json".to_string());
        assert_eq!(cached_manifest_text().as_deref(), Some("cached-json"));
        clear_cached_manifest();
        assert!(cached_manifest_text().is_none());
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
        for mirror in MIRRORS {
            let url = mirror_manifest_url(mirror.prefix);
            assert!(url.starts_with(mirror.prefix), "{url} 应带镜像前缀");
            assert!(url.ends_with(UPSTREAM_LATEST_JSON));
        }
    }
}
