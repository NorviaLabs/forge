//! `web_fetch` built-in — fetches a URL and returns its content as text.
//!
//! Unlike `web_search` this needs no provider/API key: it is a direct HTTP
//! GET, so it is always registered. The SSRF guard below is deliberately
//! narrow in scope — it blocks the obvious cases (loopback, private, and
//! link-local targets, plus non-http(s) schemes) rather than attempting a
//! complete defense against DNS rebinding.

use async_trait::async_trait;
use forge_types::{SideEffectClass, ToolOutput};
use futures::StreamExt;
use reqwest::redirect::Policy;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::net::IpAddr;
use std::time::Duration;

use crate::builtins::schema_for;
use crate::registry::ToolContext;
use crate::{Tool, ToolError};

/// Response bodies are truncated to this many bytes before conversion.
const MAX_BODY_BYTES: usize = 3 * 1024 * 1024;
/// The text handed back to the model is truncated to this many characters.
const MAX_OUTPUT_CHARS: usize = 100_000;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_REDIRECTS: usize = 5;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct WebFetchArgs {
    /// URL to fetch. Must be `http://` or `https://`.
    pub url: String,
}

pub struct WebFetchTool {
    client: reqwest::Client,
}

impl Default for WebFetchTool {
    fn default() -> Self {
        Self::new()
    }
}

impl WebFetchTool {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .redirect(Policy::custom(|attempt| {
                if attempt.previous().len() >= MAX_REDIRECTS {
                    return attempt.error("too many redirects");
                }
                match validate_redirect_target(attempt.url()) {
                    Ok(()) => attempt.follow(),
                    Err(msg) => attempt.error(msg),
                }
            }))
            .user_agent(concat!("forge-web-fetch/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("reqwest client with static config must build");
        Self { client }
    }
}

/// Non-DNS checks applied to every redirect hop: scheme, and — when the host
/// is a literal IP — whether it is a blocked address. Hostnames are not
/// re-resolved on each hop (the closure reqwest calls is synchronous), so a
/// redirect to a private hostname is only caught if the server address it
/// eventually resolves to fails the TCP connect, not by this guard.
fn validate_redirect_target(url: &reqwest::Url) -> Result<(), &'static str> {
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err("redirect to a non-http(s) scheme is blocked");
    }
    if let Some(host) = url.host_str() {
        if let Ok(ip) = host.parse::<IpAddr>() {
            if is_blocked_ip(ip) {
                return Err("redirect to a non-public address is blocked");
            }
        }
    }
    Ok(())
}

fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
        }
        IpAddr::V6(v6) => {
            if v6.is_loopback() || v6.is_unspecified() || v6.is_multicast() {
                return true;
            }
            let seg0 = v6.segments()[0];
            // fc00::/7 (unique local) and fe80::/10 (link-local).
            (seg0 & 0xfe00) == 0xfc00 || (seg0 & 0xffc0) == 0xfe80
        }
    }
}

/// Refuse to dial anything but a public http(s) endpoint. Resolves the host
/// (a no-op for literal IPs) so a hostname that only resolves to a private
/// address is caught before we ever connect to it.
async fn guard_url(url: &reqwest::Url) -> Result<(), String> {
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(format!(
            "web_fetch: unsupported scheme `{}` (only http/https)",
            url.scheme()
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| "web_fetch: URL has no host".to_string())?;
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_blocked_ip(ip) {
            return Err(format!(
                "web_fetch: refusing to fetch `{host}` (non-public address)"
            ));
        }
        return Ok(());
    }
    let port = url.port_or_known_default().unwrap_or(80);
    let addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| format!("web_fetch: DNS lookup for `{host}` failed: {e}"))?
        .collect::<Vec<_>>();
    if addrs.is_empty() {
        return Err(format!(
            "web_fetch: DNS lookup for `{host}` returned no addresses"
        ));
    }
    if let Some(blocked) = addrs.iter().find(|a| is_blocked_ip(a.ip())) {
        return Err(format!(
            "web_fetch: refusing to fetch `{host}` (resolves to non-public address {})",
            blocked.ip()
        ));
    }
    Ok(())
}

/// Strips tags/scripts/styles and decodes a handful of common entities, so
/// the model sees prose instead of markup. Not a full HTML parser — it is
/// deliberately tolerant of malformed input rather than rejecting it.
fn html_to_text(html: &str) -> String {
    const BLOCK_TAGS: &[&str] = &[
        "p",
        "br",
        "div",
        "li",
        "tr",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "section",
        "article",
        "header",
        "footer",
        "ul",
        "ol",
        "table",
        "blockquote",
        "pre",
    ];
    let mut out = String::with_capacity(html.len());
    let mut skip_tag: Option<String> = None;
    let mut i = 0;
    let bytes = html.as_bytes();
    let len = bytes.len();
    while i < len {
        if bytes[i] == b'<' {
            let Some(rel_end) = html[i..].find('>') else {
                break; // unterminated tag; stop rather than emit a stray '<'.
            };
            let end = i + rel_end;
            let inner = html[i + 1..end].trim();
            let is_close = inner.starts_with('/');
            let name_part = inner.trim_start_matches('/');
            let name: String = name_part
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
                .to_ascii_lowercase();

            if let Some(skip) = skip_tag.clone() {
                if is_close && name == skip {
                    skip_tag = None;
                }
                i = end + 1;
                continue;
            }
            if !is_close && (name == "script" || name == "style") {
                skip_tag = Some(name);
                i = end + 1;
                continue;
            }
            if BLOCK_TAGS.contains(&name.as_str()) {
                out.push('\n');
            }
            i = end + 1;
            continue;
        }
        let ch = html[i..].chars().next().unwrap_or('\u{FFFD}');
        if skip_tag.is_none() {
            out.push(ch);
        }
        i += ch.len_utf8();
    }
    let out = decode_entities(&out);
    collapse_blank_lines(&out)
}

fn decode_entities(s: &str) -> String {
    s.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
}

fn collapse_blank_lines(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut blank_run = 0;
    for line in s.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            blank_run += 1;
            if blank_run > 1 {
                continue;
            }
        } else {
            blank_run = 0;
        }
        out.push_str(trimmed);
        out.push('\n');
    }
    out.trim().to_string()
}

fn truncate_chars(s: &str, max_chars: usize) -> (String, bool) {
    if s.chars().count() <= max_chars {
        return (s.to_string(), false);
    }
    (s.chars().take(max_chars).collect(), true)
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "Fetch a URL over http(s) and return its content as plain text (HTML is stripped of tags/scripts/styles). \
         Refuses non-http(s) schemes and private/loopback network targets."
    }

    fn input_schema(&self) -> Value {
        schema_for::<WebFetchArgs>()
    }

    fn side_effect_class(&self) -> SideEffectClass {
        SideEffectClass::Network
    }

    fn idempotent(&self) -> bool {
        true
    }

    async fn call(&self, _ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let a: WebFetchArgs = serde_json::from_value(args).map_err(|e| {
            ToolError::Execution(format!("internal deserialize after validation: {e}"))
        })?;

        let url = match reqwest::Url::parse(a.url.trim()) {
            Ok(u) => u,
            Err(e) => {
                return Ok(ToolOutput::failed_exit(
                    format!("web_fetch: invalid URL: {e}"),
                    None,
                ))
            }
        };

        if let Err(msg) = guard_url(&url).await {
            return Ok(ToolOutput::failed_exit(msg, None));
        }

        Ok(fetch_and_render(&self.client, url).await)
    }
}

/// Performs the GET and renders the response into a [`ToolOutput`]. Split out
/// from [`Tool::call`] so tests can exercise it against a local server
/// without tripping the SSRF guard, which is tested separately against
/// [`guard_url`] and [`is_blocked_ip`].
async fn fetch_and_render(client: &reqwest::Client, url: reqwest::Url) -> ToolOutput {
    let response = match client.get(url.clone()).send().await {
        Ok(r) => r,
        Err(e) => return ToolOutput::failed_exit(format!("web_fetch: request failed: {e}"), None),
    };

    let status = response.status();
    let final_url = response.url().clone();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();

    let mut body = Vec::new();
    let mut truncated_body = false;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                return ToolOutput::failed_exit(
                    format!("web_fetch: error reading response body: {e}"),
                    None,
                )
            }
        };
        if body.len() >= MAX_BODY_BYTES {
            truncated_body = true;
            continue;
        }
        let remaining = MAX_BODY_BYTES - body.len();
        if chunk.len() > remaining {
            body.extend_from_slice(&chunk[..remaining]);
            truncated_body = true;
        } else {
            body.extend_from_slice(&chunk);
        }
    }

    let text = String::from_utf8_lossy(&body).into_owned();
    let is_html = content_type.contains("html");
    let is_texty = is_html
        || content_type.contains("text/")
        || content_type.contains("json")
        || content_type.contains("xml")
        || content_type.is_empty();
    if !is_texty {
        return ToolOutput::failed_exit(
            format!(
                "web_fetch: `{final_url}` returned non-text content-type `{content_type}`; fetch not supported for binary content"
            ),
            None,
        );
    }

    let rendered = if is_html { html_to_text(&text) } else { text };
    let (rendered, truncated_output) = truncate_chars(&rendered, MAX_OUTPUT_CHARS);

    let mut header = format!("URL: {final_url}\nStatus: {status}\n");
    if final_url != url {
        header.push_str(&format!("(redirected from {url})\n"));
    }
    header.push('\n');
    let mut content = format!("{header}{rendered}");
    if truncated_body || truncated_output {
        content.push_str("\n\n[web_fetch: content truncated]");
    }

    ToolOutput {
        outcome: Default::default(),
        content,
        is_error: !status.is_success(),
        exit_code: None,
        attachments: Vec::new(),
    }
}

pub fn web_fetch_tool() -> std::sync::Arc<dyn Tool> {
    std::sync::Arc::new(WebFetchTool::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validation::validate_args;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn schema_rejects_missing_url() {
        let tool = WebFetchTool::new();
        let err = validate_args("web_fetch", &tool.input_schema(), &json!({})).unwrap_err();
        assert_eq!(err.tool, "web_fetch");
    }

    #[test]
    fn describes_itself() {
        let tool = WebFetchTool::new();
        assert_eq!(tool.name(), "web_fetch");
        assert_eq!(tool.side_effect_class(), SideEffectClass::Network);
        assert!(tool.idempotent());
    }

    #[test]
    fn blocks_loopback_and_private_ips() {
        assert!(is_blocked_ip("127.0.0.1".parse().unwrap()));
        assert!(is_blocked_ip("10.0.0.5".parse().unwrap()));
        assert!(is_blocked_ip("192.168.1.1".parse().unwrap()));
        assert!(is_blocked_ip("169.254.1.1".parse().unwrap()));
        assert!(is_blocked_ip("::1".parse().unwrap()));
        assert!(is_blocked_ip("fe80::1".parse().unwrap()));
        assert!(is_blocked_ip("fc00::1".parse().unwrap()));
        assert!(!is_blocked_ip("93.184.216.34".parse().unwrap()));
        assert!(!is_blocked_ip(
            "2606:2800:220:1:248:1893:25c8:1946".parse().unwrap()
        ));
    }

    #[tokio::test]
    async fn guard_url_rejects_non_http_scheme() {
        let url = reqwest::Url::parse("file:///etc/passwd").unwrap();
        let err = guard_url(&url).await.unwrap_err();
        assert!(err.contains("unsupported scheme"), "{err}");
    }

    #[tokio::test]
    async fn guard_url_rejects_literal_loopback() {
        let url = reqwest::Url::parse("http://127.0.0.1:9/").unwrap();
        let err = guard_url(&url).await.unwrap_err();
        assert!(err.contains("non-public"), "{err}");
    }

    #[test]
    fn html_to_text_strips_tags_scripts_and_styles() {
        let html = "<html><head><style>body{color:red}</style></head><body>\
                     <h1>Title</h1><p>Hello <b>world</b>&amp;friends</p>\
                     <script>alert(1)</script></body></html>";
        let text = html_to_text(html);
        assert!(text.contains("Title"), "{text}");
        assert!(text.contains("Hello"), "{text}");
        assert!(text.contains("world"), "{text}");
        assert!(text.contains("&friends"), "{text}");
        assert!(!text.contains("color:red"), "{text}");
        assert!(!text.contains("alert(1)"), "{text}");
        assert!(!text.contains('<'), "{text}");
    }

    #[test]
    fn truncate_chars_marks_truncation() {
        let (s, truncated) = truncate_chars("abcdef", 3);
        assert_eq!(s, "abc");
        assert!(truncated);
        let (s, truncated) = truncate_chars("abc", 3);
        assert_eq!(s, "abc");
        assert!(!truncated);
    }

    /// Minimal single-request HTTP/1.1 server for integration-testing the
    /// tool without a real network dependency. Reads one request line +
    /// headers (discarded) then writes the given raw response bytes.
    fn spawn_http_server(response: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0_u8; 4096];
                let mut total = Vec::new();
                loop {
                    let n = stream.read(&mut buf).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    total.extend_from_slice(&buf[..n]);
                    if total.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        format!("http://{addr}/")
    }

    #[tokio::test]
    async fn fetches_and_converts_html() {
        let url = spawn_http_server(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n\
             <html><body><h1>Hi</h1><p>there</p></body></html>",
        );
        let tool = WebFetchTool::new();
        let out = fetch_and_render(&tool.client, reqwest::Url::parse(&url).unwrap()).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("Hi"), "{}", out.content);
        assert!(out.content.contains("there"), "{}", out.content);
    }

    #[tokio::test]
    async fn non_success_status_is_reported_as_error() {
        let url = spawn_http_server(
            "HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\nnope",
        );
        let tool = WebFetchTool::new();
        let out = fetch_and_render(&tool.client, reqwest::Url::parse(&url).unwrap()).await;
        assert!(out.is_error);
        assert!(out.content.contains("404"), "{}", out.content);
    }

    #[tokio::test]
    async fn binary_content_type_is_refused() {
        let url = spawn_http_server(
            "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n\x00\x01\x02",
        );
        let tool = WebFetchTool::new();
        let out = fetch_and_render(&tool.client, reqwest::Url::parse(&url).unwrap()).await;
        assert!(out.is_error);
        assert!(out.content.contains("non-text"), "{}", out.content);
    }

    #[tokio::test]
    async fn rejects_loopback_url_before_connecting() {
        let tool = WebFetchTool::new();
        let ctx = ToolContext::new(std::env::current_dir().unwrap());
        let out = tool
            .call(&ctx, json!({"url": "http://127.0.0.1:1/"}))
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("non-public"), "{}", out.content);
    }

    #[tokio::test]
    async fn call_reports_internal_deserialize_failure() {
        let tool = WebFetchTool::new();
        let ctx = ToolContext::new(std::env::current_dir().unwrap());
        let err = tool.call(&ctx, json!({"url": 12345})).await.unwrap_err();
        assert!(err.to_string().contains("internal deserialize"), "{err}");
    }
}
