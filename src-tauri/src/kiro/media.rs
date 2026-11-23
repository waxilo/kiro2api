//! 图片 URL 归一化：把公网 http(s) 图片下载成 base64，喂给只认 bytes 的 Kiro 上游。
//!
//! 三种协议都可能带「公网图片 URL」而不是内联 base64：
//! - Anthropic：`{"type":"image","source":{"type":"url","url":"https://..."}}`
//! - OpenAI Chat：`{"type":"image_url","image_url":{"url":"https://..."}}`
//! - Responses：`{"type":"input_image","image_url":"https://..."}`
//!
//! 历史上这三条路的处理各不相同（Anthropic 直接 400、OpenAI 静默丢图），同一个
//! 请求换个协议就得到不同结果。现在统一在进入 transform 之前替换成 `data:` URL，
//! 于是下游只需要认识 base64 一种形态；下载失败则统一报 400，不再有静默丢图。
//!
//! 代理代客户端出网，因此这里按 SSRF 的姿势收口：只允许 http/https、拒绝内网网段、
//! 限制重定向与体积、只接受 `image/*` 且落在 Kiro 支持的格式内。

use std::io::Read;
use std::net::{IpAddr, ToSocketAddrs};
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};

/// 单图字节上限。base64 膨胀 4/3 后在 8 MiB 请求体限制内还留有余量。
pub const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;
/// 连接 + 读取的总超时。
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
/// 最多跟随的重定向次数。
pub const MAX_REDIRECTS: usize = 3;

/// Kiro 上游只认这几种图片格式，提前拦掉比让上游报 400 更好排查。
const ALLOWED_IMAGE_TYPES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/jpg",
    "image/gif",
    "image/webp",
];

/// 下载回来的图片。
pub struct FetchedImage {
    pub media_type: String,
    pub bytes: Vec<u8>,
}

/// 下载器抽象：单测注入假实现，避免用例依赖外网。
pub trait ImageFetcher {
    fn fetch(&self, url: &str) -> Result<FetchedImage, String>;
}

/// 默认实现：reqwest 阻塞客户端。
pub struct HttpImageFetcher {
    client: reqwest::blocking::Client,
    max_bytes: usize,
}

impl HttpImageFetcher {
    pub fn new() -> Result<Self, String> {
        let client = reqwest::blocking::Client::builder()
            .timeout(FETCH_TIMEOUT)
            // 重定向也要过私网校验：否则公网域名 302 到 169.254.169.254 就绕过去了。
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.url().scheme() != "http" && attempt.url().scheme() != "https" {
                    return attempt.error("重定向到了非 http(s) 地址");
                }
                if attempt.previous().len() >= MAX_REDIRECTS {
                    return attempt.error("重定向次数过多");
                }
                if let Err(reason) = ensure_public_http_url(attempt.url()) {
                    return attempt.error(reason);
                }
                attempt.follow()
            }))
            .build()
            .map_err(|e| format!("初始化图片下载客户端失败: {e}"))?;
        Ok(Self {
            client,
            max_bytes: MAX_IMAGE_BYTES,
        })
    }
}

impl ImageFetcher for HttpImageFetcher {
    fn fetch(&self, url: &str) -> Result<FetchedImage, String> {
        let parsed = reqwest::Url::parse(url)
            .map_err(|e| format!("图片地址无法解析（{url}）: {e}"))?;
        ensure_public_http_url(&parsed)?;

        let response = self
            .client
            .get(parsed)
            .send()
            .map_err(|e| format!("图片下载失败（{url}）: {e}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "图片下载失败（{url}）: HTTP {}",
                response.status().as_u16()
            ));
        }
        if let Some(length) = response.content_length() {
            if length > self.max_bytes as u64 {
                return Err(too_large(url, self.max_bytes));
            }
        }
        let declared = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        let declared = normalize_content_type(declared);

        // 读的时候再卡一次：content-length 可能没给，或者给的是假值。
        let mut bytes = Vec::new();
        response
            .take((self.max_bytes + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("图片读取失败（{url}）: {e}"))?;
        let media_type = validate_image(&declared, &bytes, self.max_bytes)
            .map_err(|reason| format!("图片不可用（{url}）: {reason}"))?;
        Ok(FetchedImage { media_type, bytes })
    }
}

/// 把请求体里所有「公网图片 URL」换成 `data:` 内联，返回替换个数（用于日志）。
///
/// 只处理图片；`file_id` / 文档等其它形态原样留给下游。空 URL 与已经是 `data:`
/// 的保持不动，避免把客户端的本地内联再编码一遍。
pub fn normalize_image_urls(
    body: &mut Value,
    fetcher: &dyn ImageFetcher,
) -> Result<usize, String> {
    let mut replaced = 0;

    // Anthropic 与 OpenAI Chat 的消息都在 `messages[]`，块类型互不重叠。
    if let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) {
        for message in messages.iter_mut() {
            let Some(content) = message.get_mut("content").and_then(Value::as_array_mut) else {
                continue;
            };
            for block in content.iter_mut() {
                if replace_anthropic_image(block, fetcher)?
                    || replace_openai_style_image(block, "image_url", fetcher)?
                {
                    replaced += 1;
                }
            }
        }
    }

    // Responses 的输入项：`input[]` 每项可能是 message（content 数组），
    // 也可能被客户端直接塞成 input_image。
    if let Some(items) = body.get_mut("input").and_then(Value::as_array_mut) {
        for item in items.iter_mut() {
            if replace_openai_style_image(item, "input_image", fetcher)? {
                replaced += 1;
                continue;
            }
            let Some(content) = item.get_mut("content").and_then(Value::as_array_mut) else {
                continue;
            };
            for block in content.iter_mut() {
                if replace_openai_style_image(block, "input_image", fetcher)? {
                    replaced += 1;
                }
            }
        }
    }

    Ok(replaced)
}

/// Anthropic `image` 块：`source.type == "url"` 且不是 data URL 时下载。
fn replace_anthropic_image(
    block: &mut Value,
    fetcher: &dyn ImageFetcher,
) -> Result<bool, String> {
    if block.get("type").and_then(Value::as_str) != Some("image") {
        return Ok(false);
    }
    let Some(source) = block.get("source") else {
        return Ok(false);
    };
    if source.get("type").and_then(Value::as_str) != Some("url") {
        return Ok(false);
    }
    let url = source.get("url").and_then(Value::as_str).unwrap_or("");
    let Some((media_type, data)) = fetch_to_base64(url, fetcher)? else {
        return Ok(false);
    };
    block["source"] = json!({
        "type": "base64",
        "media_type": media_type,
        "data": data,
    });
    Ok(true)
}

/// OpenAI Chat 的 `image_url` / Responses 的 `input_image`：`image_url` 既可能是
/// 字符串，也可能是 `{ "url": ... }` 对象，替换后保持原有形状，免得下游解析漏掉。
fn replace_openai_style_image(
    block: &mut Value,
    block_type: &str,
    fetcher: &dyn ImageFetcher,
) -> Result<bool, String> {
    if block.get("type").and_then(Value::as_str) != Some(block_type) {
        return Ok(false);
    }
    let Some(current) = image_url_of(block) else {
        return Ok(false);
    };
    let Some((media_type, data)) = fetch_to_base64(&current, fetcher)? else {
        return Ok(false);
    };
    let data_url = format!("data:{media_type};base64,{data}");
    match block.get("image_url") {
        Some(Value::Object(_)) => block["image_url"]["url"] = json!(data_url),
        Some(Value::String(_)) | None => block["image_url"] = json!(data_url),
        // 其余类型是畸形输入，不猜，留给下游按原样处理
        _ => return Ok(false),
    }
    Ok(true)
}

fn image_url_of(block: &Value) -> Option<String> {
    match block.get("image_url") {
        Some(Value::String(url)) => Some(url.clone()),
        Some(value @ Value::Object(_)) => value
            .get("url")
            .and_then(Value::as_str)
            .map(str::to_string),
        _ => None,
    }
}

/// 需要下载时返回 `(media_type, base64_data)`；不需要下载（空 / 已内联）返回 `None`。
fn fetch_to_base64(
    url: &str,
    fetcher: &dyn ImageFetcher,
) -> Result<Option<(String, String)>, String> {
    let url = url.trim();
    if url.is_empty() || url.starts_with("data:") {
        return Ok(None);
    }
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(format!(
            "不支持的图片地址（{url}）：只接受 http(s) 链接、data: 内联图片或 base64"
        ));
    }
    let image = fetcher.fetch(url)?;
    let data = base64::engine::general_purpose::STANDARD.encode(&image.bytes);
    Ok(Some((image.media_type, data)))
}

/// 只允许 http/https，且目标主机必须解析到公网地址。
fn ensure_public_http_url(url: &reqwest::Url) -> Result<(), String> {
    match url.scheme() {
        "http" | "https" => {}
        other => {
            return Err(format!("不支持的图片协议 {other}：只接受 http(s)"));
        }
    }
    let host = url
        .host_str()
        .ok_or_else(|| format!("图片地址缺少主机名: {url}"))?;
    // IPv6 字面量在 Url::host_str 里带方括号
    let bare_host = host.trim_start_matches('[').trim_end_matches(']');

    if let Ok(ip) = bare_host.parse::<IpAddr>() {
        return ensure_public_ip(ip, host);
    }

    let port = url.port_or_known_default().unwrap_or(443);
    let addrs = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("无法解析图片域名 {host}: {e}"))?;
    let mut resolved = false;
    for addr in addrs {
        resolved = true;
        ensure_public_ip(addr.ip(), host)?;
    }
    if !resolved {
        return Err(format!("图片域名 {host} 没有解析结果"));
    }
    Ok(())
}

fn ensure_public_ip(ip: IpAddr, host: &str) -> Result<(), String> {
    if is_blocked_ip(ip) {
        return Err(format!(
            "拒绝访问内网地址（{host} → {ip}）：代理只下载公网图片"
        ));
    }
    Ok(())
}

fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                // 100.64.0.0/10（CGNAT）与 0.0.0.0/8 也是常见的内网/占位段
                || (octets[0] == 100 && (64..128).contains(&octets[1]))
                || octets[0] == 0
        }
        IpAddr::V6(v6) => {
            let head = v6.segments()[0];
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (head & 0xffc0) == 0xfe80 // fe80::/10 链路本地
                || (head & 0xfe00) == 0xfc00 // fc00::/7 唯一本地
                || v6
                    .to_ipv4_mapped()
                    .is_some_and(|v4| is_blocked_ip(IpAddr::V4(v4)))
        }
    }
}

fn too_large(url: &str, max: usize) -> String {
    let max_mb = max as f64 / (1024.0 * 1024.0);
    format!(
        "图片超过 {max_mb:.0} MB 上限（{url}）：请压缩后重发或改用 data: 内联小图"
    )
}

/// `HTTP` 头里的 content-type 可能带参数（`image/png; charset=binary`）。
fn normalize_content_type(raw: &str) -> String {
    raw.split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_lowercase()
}

/// 判定图片是否可用，返回规范化后的 media_type。
///
/// 缺失 content-type 时按字节魔数兜底：有些 CDN 不给头，直接拒掉会让正常图片用不了。
fn validate_image(declared: &str, bytes: &[u8], max: usize) -> Result<String, String> {
    if bytes.is_empty() {
        return Err("下载到的内容为空".to_string());
    }
    if bytes.len() > max {
        return Err(format!(
            "体积 {} 字节超过 {} MB 上限",
            bytes.len(),
            max / (1024 * 1024)
        ));
    }
    if !declared.is_empty() {
        if !declared.starts_with("image/") {
            return Err(format!("目标不是图片（Content-Type: {declared}）"));
        }
        if !ALLOWED_IMAGE_TYPES.contains(&declared) {
            return Err(format!(
                "不支持的图片类型 {declared}（仅支持 png / jpeg / gif / webp）"
            ));
        }
        return Ok(declared.to_string());
    }
    sniff_image_type(bytes).ok_or_else(|| "无法识别的图片格式（缺少 Content-Type）".to_string())
}

fn sniff_image_type(bytes: &[u8]) -> Option<String> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png".to_string())
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg".to_string())
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif".to_string())
    } else if bytes.len() > 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("image/webp".to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 记账用的假下载器：断言「该发的请求发了、不该发的没发」。
    struct FakeFetcher {
        result: Result<FetchedImage, String>,
    }

    impl FakeFetcher {
        fn ok(media_type: &str, bytes: &[u8]) -> Self {
            Self {
                result: Ok(FetchedImage {
                    media_type: media_type.to_string(),
                    bytes: bytes.to_vec(),
                }),
            }
        }

        fn err(message: &str) -> Self {
            Self {
                result: Err(message.to_string()),
            }
        }
    }

    impl ImageFetcher for FakeFetcher {
        fn fetch(&self, _url: &str) -> Result<FetchedImage, String> {
            match &self.result {
                Ok(image) => Ok(FetchedImage {
                    media_type: image.media_type.clone(),
                    bytes: image.bytes.clone(),
                }),
                Err(message) => Err(message.clone()),
            }
        }
    }

    /// 调用就 panic：用于断言「这个 URL 根本不该被下载」。
    struct PanicFetcher;

    impl ImageFetcher for PanicFetcher {
        fn fetch(&self, url: &str) -> Result<FetchedImage, String> {
            panic!("不应该下载 {url}");
        }
    }

    #[test]
    fn data_url_is_left_untouched() {
        let mut body = json!({
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "image", "source": { "type": "url", "url": "data:image/png;base64,AAAA" } },
                    { "type": "image_url", "image_url": { "url": "data:image/jpeg;base64,BBBB" } },
                ]
            }]
        });
        let replaced = normalize_image_urls(&mut body, &PanicFetcher).expect("no fetch");
        assert_eq!(replaced, 0);
        assert_eq!(
            body["messages"][0]["content"][0]["source"]["url"],
            "data:image/png;base64,AAAA"
        );
    }

    #[test]
    fn anthropic_http_url_is_downloaded() {
        let mut body = json!({
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "text", "text": "看图" },
                    { "type": "image", "source": { "type": "url", "url": "https://example.com/a.png" } },
                ]
            }]
        });
        let replaced =
            normalize_image_urls(&mut body, &FakeFetcher::ok("image/png", b"abc")).unwrap();
        assert_eq!(replaced, 1);
        let source = &body["messages"][0]["content"][1]["source"];
        assert_eq!(source["type"], "base64");
        assert_eq!(source["media_type"], "image/png");
        assert_eq!(source["data"], "YWJj");
    }

    #[test]
    fn openai_and_responses_urls_are_downloaded() {
        let mut body = json!({
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "image_url", "image_url": { "url": "https://example.com/a.jpg" } },
                    { "type": "image_url", "image_url": "https://example.com/b.jpg" },
                ]
            }],
            "input": [{
                "type": "message",
                "role": "user",
                "content": [
                    { "type": "input_image", "image_url": "https://example.com/c.jpg" },
                    { "type": "input_image", "image_url": { "url": "https://example.com/d.jpg" } },
                ]
            }]
        });
        let replaced =
            normalize_image_urls(&mut body, &FakeFetcher::ok("image/jpeg", b"xy")).unwrap();
        assert_eq!(replaced, 4);
        // 形状保持：对象还是对象、字符串还是字符串
        assert_eq!(
            body["messages"][0]["content"][0]["image_url"]["url"],
            "data:image/jpeg;base64,eHk="
        );
        assert_eq!(
            body["messages"][0]["content"][1]["image_url"],
            "data:image/jpeg;base64,eHk="
        );
        assert_eq!(
            body["input"][0]["content"][0]["image_url"],
            "data:image/jpeg;base64,eHk="
        );
        assert_eq!(
            body["input"][0]["content"][1]["image_url"]["url"],
            "data:image/jpeg;base64,eHk="
        );
    }

    #[test]
    fn fetch_failure_is_reported_with_url() {
        let mut body = json!({
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "image", "source": { "type": "url", "url": "https://example.com/gone.png" } }
                ]
            }]
        });
        let error = normalize_image_urls(&mut body, &FakeFetcher::err("HTTP 404"))
            .expect_err("下载失败必须报错");
        assert!(error.contains("HTTP 404"), "{error}");
    }

    #[test]
    fn unsupported_scheme_is_rejected() {
        let mut body = json!({
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "image", "source": { "type": "url", "url": "file:///etc/passwd" } }
                ]
            }]
        });
        let error = normalize_image_urls(&mut body, &PanicFetcher).expect_err("必须拒绝");
        assert!(error.contains("file:///etc/passwd"), "{error}");
    }

    #[test]
    fn private_hosts_are_rejected_before_any_request() {
        for url in [
            "http://127.0.0.1/a.png",
            "http://localhost/a.png",
            "http://169.254.169.254/latest/meta-data",
            "http://192.168.1.10/a.png",
            "http://[::1]/a.png",
        ] {
            let parsed = reqwest::Url::parse(url).unwrap();
            let error = ensure_public_http_url(&parsed).expect_err(url);
            assert!(error.contains("内网") || error.contains("无法解析"), "{url}: {error}");
        }
        // 公网字面量放行
        assert!(ensure_public_http_url(&reqwest::Url::parse("https://1.1.1.1/a.png").unwrap()).is_ok());
    }

    #[test]
    fn oversized_or_wrong_type_is_rejected() {
        let error = validate_image("image/png", &vec![0u8; 11], 10).expect_err("超限");
        assert!(error.contains("超过"), "{error}");

        let error = validate_image("text/html", b"<html>", 1024).expect_err("非图片");
        assert!(error.contains("不是图片"), "{error}");

        let error = validate_image("image/svg+xml", b"<svg/>", 1024).expect_err("不支持的类型");
        assert!(error.contains("不支持的图片类型"), "{error}");

        assert_eq!(
            validate_image(&normalize_content_type("image/png; charset=binary"), b"x", 16),
            Ok("image/png".to_string())
        );
        assert!(validate_image("", &vec![0u8; 4], 10).is_err(), "空内容/无法识别应报错");
    }

    #[test]
    fn content_type_is_sniffed_when_missing() {
        let png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";
        assert_eq!(validate_image("", png, 1024), Ok("image/png".to_string()));
        assert_eq!(
            validate_image("", &[0xff, 0xd8, 0xff, 0xe0], 1024),
            Ok("image/jpeg".to_string())
        );
        assert_eq!(
            validate_image("", b"GIF89a..........", 1024),
            Ok("image/gif".to_string())
        );
        let mut webp = b"RIFF\0\0\0\0WEBP".to_vec();
        webp.extend_from_slice(b"VP8 ");
        assert_eq!(validate_image("", &webp, 1024), Ok("image/webp".to_string()));
        assert!(validate_image("", b"not-an-image", 1024).is_err());
    }
}
