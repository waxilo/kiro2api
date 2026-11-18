//! 出站请求失败的可读归因。
//!
//! `reqwest` 的 `Display` 只到「error sending request for url (…)」——真正的原因
//! （DNS / TLS / 连接被拒 / 超时）全在 `source()` 链里。不展开就只能靠猜，而这几类
//! 在 macOS 上特别容易同时出现：系统代理没被读取、或代理 MITM 换掉了证书，报错一模一样。
//!
//! 这里把整条链展开、归到一个类别，文案直接给出排查方向，让用户不必回到终端复现。

use std::error::Error as StdError;

/// 传输失败的归因类别。刻意做成枚举而不是直接拼字符串：分类逻辑可以脱离网络单测。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransportKind {
    /// 超时：连不上、或者连上了但没有响应。
    Timeout,
    /// 域名解析失败（DNS 污染 / 分流规则把域名交给了不存在的解析器）。
    Dns,
    /// TLS 握手 / 证书链校验失败。
    Tls,
    /// 目标端口拒绝连接。
    Refused,
    /// 其它建连阶段的失败。
    Connect,
    /// 请求已经建连、但发送/读取阶段失败。
    Other,
}

/// 按错误链文本归类。
///
/// 判定顺序有讲究：先超时（`is_timeout` 是 reqwest 自己的结论，最可靠），再 DNS、
/// 再 TLS，最后才看「是不是建连阶段」——hyper 把 DNS 失败也包在 `Connect` 里，
/// 反过来先判 `connect` 会把原因吞掉。
pub(crate) fn classify(causes: &[String], timeout: bool, connect: bool) -> TransportKind {
    let joined = causes.join(" | ").to_ascii_lowercase();
    let hit = |needles: &[&str]| needles.iter().any(|needle| joined.contains(needle));

    if timeout || hit(&["timed out", "timeout", "deadline has elapsed"]) {
        return TransportKind::Timeout;
    }
    if hit(&[
        "dns error",
        "failed to lookup address",
        "name or service not known",
        "nodename nor servname",
        "temporary failure in name resolution",
        "no address associated with hostname",
    ]) {
        return TransportKind::Dns;
    }
    if hit(&[
        "certificate",
        "unknownissuer",
        "unknown issuer",
        "invalid peer certificate",
        "tls handshake",
        "handshake failure",
        "received fatal alert",
        "ssl error",
    ]) {
        return TransportKind::Tls;
    }
    if hit(&["connection refused", "actively refused", "os error 10061"]) {
        return TransportKind::Refused;
    }
    if connect {
        TransportKind::Connect
    } else {
        TransportKind::Other
    }
}

/// 把超长 / 多行的底层消息压成一行，避免 toast 被撑开。
fn squeeze(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > 200 {
        let mut clipped: String = flat.chars().take(200).collect();
        clipped.push('…');
        clipped
    } else {
        flat
    }
}

fn render(kind: TransportKind, host: &str, detail: &str) -> String {
    match kind {
        TransportKind::Timeout => {
            format!("连接超时：{host} 无响应（代理未生效、网络被拦截，或目标不可达）")
        }
        TransportKind::Dns => {
            format!("域名解析失败：{host} 无法解析（检查 DNS 与代理分流规则）")
        }
        TransportKind::Tls => format!(
            "TLS 证书校验失败：{host}（系统根证书不受信任，常见于代理 MITM 或公司 SSL 检查）"
        ),
        TransportKind::Refused => {
            format!("连接被拒绝：{host}（端口无服务监听，或被防火墙拦截）")
        }
        TransportKind::Connect => match squeeze(detail) {
            detail if detail.is_empty() => format!("连接失败：{host}"),
            detail => format!("连接失败：{host}（{detail}）"),
        },
        TransportKind::Other => match squeeze(detail) {
            detail if detail.is_empty() => "请求未发出".to_string(),
            detail => format!("请求未发出：{detail}"),
        },
    }
}

/// 把 `reqwest` 的传输错误翻成一句能照着排查的话。
///
/// 返回文本**不含动作名**：调用方（前端 `run()` / `登录失败：` 前缀）已经带了，
/// 这里再带一遍会变成「同步模型失败：同步模型失败：…」。
pub(crate) fn describe_reqwest_error(error: &reqwest::Error) -> String {
    let mut chain = vec![error.to_string()];
    let mut cursor = StdError::source(error);
    while let Some(source) = cursor {
        chain.push(source.to_string());
        cursor = source.source();
    }
    let host = error.url().and_then(|url| url.host_str()).unwrap_or("上游");
    let kind = classify(&chain, error.is_timeout(), error.is_connect());
    render(kind, host, chain.last().map(String::as_str).unwrap_or(""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn causes(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn timeout_wins_over_other_signals() {
        // 超时时 hyper 常同时给出 "client error (Connect)"，不能因此判成 Connect
        let chain = causes(&["client error (Connect)", "operation timed out"]);
        assert_eq!(classify(&chain, false, true), TransportKind::Timeout);
        assert_eq!(
            classify(&causes(&[]), true, true),
            TransportKind::Timeout,
            "reqwest 自己判定的超时优先级最高"
        );
    }

    #[test]
    fn dns_failure_is_not_swallowed_by_connect() {
        let chain = causes(&[
            "error sending request for url (https://management.us-east-1.kiro.dev/ListAvailableModels)",
            "client error (Connect)",
            "dns error: failed to lookup address information: Name or service not known",
        ]);
        assert_eq!(classify(&chain, false, true), TransportKind::Dns);
    }

    #[test]
    fn tls_failures_cover_mitm_and_unknown_issuer() {
        for text in [
            "invalid peer certificate: UnknownIssuer",
            "tls handshake eof",
            "received fatal alert: handshake_failure",
        ] {
            assert_eq!(
                classify(&causes(&["client error (Connect)", text]), false, true),
                TransportKind::Tls,
                "{text}"
            );
        }
    }

    #[test]
    fn refused_is_reported_before_generic_connect() {
        let chain = causes(&[
            "client error (Connect)",
            "tcp connect error: Connection refused (os error 10061)",
        ]);
        assert_eq!(classify(&chain, false, true), TransportKind::Refused);
    }

    #[test]
    fn unknown_connect_error_keeps_raw_detail() {
        let chain = causes(&[
            "client error (Connect)",
            "tcp connect error: network is unreachable",
        ]);
        assert_eq!(classify(&chain, false, true), TransportKind::Connect);
        let text = render(TransportKind::Connect, "example.com", &chain[1]);
        assert!(text.contains("example.com"), "{text}");
        assert!(text.contains("network is unreachable"), "{text}");
    }

    #[test]
    fn host_and_advice_are_in_the_message() {
        let dns = render(TransportKind::Dns, "management.us-east-1.kiro.dev", "");
        assert!(dns.contains("management.us-east-1.kiro.dev"), "{dns}");
        assert!(dns.contains("域名解析失败"), "{dns}");

        let tls = render(TransportKind::Tls, "management.us-east-1.kiro.dev", "");
        assert!(tls.contains("根证书"), "{tls}");
    }

    /// 真实的一次失败请求：不依赖外网，只要本机 1 端口没有服务监听就会立刻被拒。
    /// 关掉代理，避免 CI 上 `HTTPS_PROXY` 把 `127.0.0.1` 也劫走导致分类漂移。
    #[test]
    fn real_refused_connection_is_described() {
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("client builds");
        let error = client
            .get("http://127.0.0.1:1/")
            .send()
            .expect_err("端口 1 不应有服务");
        let text = describe_reqwest_error(&error);
        assert!(text.contains("连接被拒绝"), "{text}");
    }
}
