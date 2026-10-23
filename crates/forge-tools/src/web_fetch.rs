//! `web_fetch` built-in — fetches a URL and returns its content as text.
//!
//! Unlike `web_search` this needs no provider/API key: it is a direct HTTP
//! GET, so it is always registered.
//!
//! Every hop — the first request and each redirect — is first checked
//! against the session's egress allowlist (the same `host(...)` policy shell
//! commands are confined by). A refusal is a [`ToolError::SandboxDenied`]
//! carrying the host, so the session raises the usual host-grant prompt and
//! retries after a grant. Non-http(s) schemes are refused outright.
//!
//! How the hop then leaves depends on the session:
//!
//! * **Through the per-call gateway** ([`ToolContext::gateway_for_call`])
//!   when the session runs an egress proxy. Every hop is sent via the
//!   gateway, which applies the same policy, refuses non-public
//!   destinations, pins the address it checked, and logs the connection
//!   under this call's id — the one enforcement point shell traffic meets.
//!   System proxy settings (`HTTP(S)_PROXY`, `ALL_PROXY`, `NO_PROXY`) are
//!   ignored so they cannot route around it. A gateway policy refusal becomes
//!   the host-grant prompt; a non-public-address refusal is a plain failure,
//!   since no grant can fix it. If the session has a gateway but the call's
//!   endpoint cannot be started, the fetch fails rather than going direct.
//! * **Directly** when there is no egress proxy (standalone/test callers).
//!   The host must resolve only to public addresses (loopback, private and
//!   link-local targets are refused), and the connection is pinned to
//!   exactly the addresses that were checked, so a second DNS answer cannot
//!   rebind the request elsewhere.
//!
//! Redirects are followed by hand rather than by reqwest, because its
//! redirect hook is synchronous and cannot re-check (or pin) the next host.

use async_trait::async_trait;
use forge_types::{SideEffectClass, ToolOutput};
use futures::StreamExt;
use reqwest::redirect::Policy;
use reqwest::StatusCode;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;
use tokio::time::Instant;

use crate::builtins::schema_for;
use crate::egress::{CallGateway, EgressShared};
use crate::registry::ToolContext;
use crate::{Tool, ToolError};

/// Response bodies are truncated to this many bytes before conversion.
const MAX_BODY_BYTES: usize = 3 * 1024 * 1024;
/// The text handed back to the model is truncated to this many characters.
const MAX_OUTPUT_CHARS: usize = 100_000;
/// Budget for the whole fetch, redirects included.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_REDIRECTS: usize = 5;

/// Decides whether a resolved address may be dialed. Production always uses
/// [`is_blocked_ip`]; tests substitute a narrower one so they can exercise
/// the full fetch path against a loopback server.
type AddrGuard = fn(IpAddr) -> bool;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct WebFetchArgs {
    /// URL to fetch. Must be `http://` or `https://`.
    pub url: String,
}

#[derive(Debug, Default)]
pub struct WebFetchTool;

impl WebFetchTool {
    pub fn new() -> Self {
        Self
    }
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
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_blocked_ip(IpAddr::V4(v4));
            }
            let seg0 = v6.segments()[0];
            // fc00::/7 (unique local) and fe80::/10 (link-local).
            (seg0 & 0xfe00) == 0xfc00 || (seg0 & 0xffc0) == 0xfe80
        }
    }
}

/// The session's host allowlist, if one is reachable from `ctx`.
///
/// `None` — no egress grant, or a grant with no live proxy behind it — means
/// this session has no host policy at all (the sandbox or its proxy is
/// unavailable, or a standalone/test caller). web_fetch keeps its historical
/// behaviour there: allowed, behind the SSRF guard only. Failing closed would
/// leave such a session with no way to fetch anything, since there is no live
/// policy a host grant could be written to, so the prompt could never unblock
/// the retry. A session that does run a proxy always has a policy, and that
/// policy fails closed (an empty allowlist permits nothing).
fn host_policy(ctx: &ToolContext) -> Option<&EgressShared> {
    ctx.egress.as_ref().and_then(|grant| grant.control.as_ref())
}

/// What a URL's host names: an IP literal (IPv6 brackets stripped) or a
/// domain to resolve.
enum Target<'a> {
    Ip(IpAddr),
    Domain(&'a str),
}

fn target(url: &reqwest::Url) -> Option<Target<'_>> {
    let host = url.host_str()?;
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    Some(match bare.parse::<IpAddr>() {
        Ok(ip) => Target::Ip(ip),
        Err(_) => Target::Domain(host),
    })
}

/// The host as the allowlist sees it: a domain as written, an IP literal
/// without IPv6 brackets.
fn policy_host(url: &reqwest::Url) -> Option<String> {
    Some(match target(url)? {
        Target::Ip(ip) => ip.to_string(),
        Target::Domain(domain) => domain.to_ascii_lowercase(),
    })
}

/// Refuse a hop whose host the session's allowlist does not permit.
///
/// The error carries the host so forge-core offers the same "allow this host"
/// prompt it shows for a confined shell command, then re-runs the fetch.
fn check_host_permitted(
    policy: Option<&EgressShared>,
    url: &reqwest::Url,
    redirected_from: Option<&reqwest::Url>,
) -> Result<(), ToolError> {
    let Some(policy) = policy else {
        return Ok(());
    };
    let host = policy_host(url).unwrap_or_default();
    if policy.permits_host(&host) {
        return Ok(());
    }
    let content = match redirected_from {
        Some(from) => format!(
            "web_fetch: `{from}` redirected to `{url}`, but `{host}` is not in this session's allowed hosts"
        ),
        None => format!("web_fetch: `{host}` is not in this session's allowed hosts"),
    };
    Err(ToolError::SandboxDenied {
        content,
        reason: crate::egress::HOST_DENIED_EXPLANATION.into(),
        denied_host: Some(host),
    })
}

/// Refuse a URL web_fetch cannot fetch on either path: a scheme other than
/// http(s), or no host.
fn check_fetchable(url: &reqwest::Url) -> Result<Target<'_>, String> {
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(format!(
            "web_fetch: unsupported scheme `{}` (only http/https)",
            url.scheme()
        ));
    }
    target(url).ok_or_else(|| "web_fetch: URL has no host".to_string())
}

/// Resolve `url`'s host and refuse it unless every address is public. The
/// returned addresses are the only ones the request may connect to.
///
/// Literal IPs skip DNS. All addresses are checked rather than the first, so
/// a name with one public and one private record cannot be steered to the
/// private one by the connector's fallback order.
async fn resolve_public(url: &reqwest::Url, blocked: AddrGuard) -> Result<Vec<SocketAddr>, String> {
    let port = url.port_or_known_default().unwrap_or(80);
    let domain = match check_fetchable(url)? {
        Target::Ip(ip) => return literal_addr(ip, port, blocked),
        Target::Domain(domain) => domain,
    };
    let addrs = tokio::net::lookup_host((domain, port))
        .await
        .map_err(|e| format!("web_fetch: DNS lookup for `{domain}` failed: {e}"))?
        .collect::<Vec<_>>();
    if addrs.is_empty() {
        return Err(format!(
            "web_fetch: DNS lookup for `{domain}` returned no addresses"
        ));
    }
    if let Some(bad) = addrs.iter().find(|a| blocked(a.ip())) {
        return Err(format!(
            "web_fetch: refusing to fetch `{domain}` (resolves to non-public address {})",
            bad.ip()
        ));
    }
    Ok(addrs)
}

fn literal_addr(ip: IpAddr, port: u16, blocked: AddrGuard) -> Result<Vec<SocketAddr>, String> {
    if blocked(ip) {
        return Err(format!(
            "web_fetch: refusing to fetch `{ip}` (non-public address)"
        ));
    }
    Ok(vec![SocketAddr::new(ip, port)])
}

fn client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .redirect(Policy::none())
        .user_agent(concat!("forge-web-fetch/", env!("CARGO_PKG_VERSION")))
}

/// A client for one direct hop that can only connect to `addrs`.
///
/// `resolve_to_addrs` replaces DNS for this host with the addresses already
/// checked, which is what closes the rebinding window between the check and
/// the connect. A literal-IP URL never consults DNS, so it needs no pin.
/// Redirects are disabled: [`fetch`] follows them itself, re-checking each.
fn pinned_client(url: &reqwest::Url, addrs: &[SocketAddr]) -> Result<reqwest::Client, String> {
    let mut builder = client_builder();
    if let Some(Target::Domain(domain)) = target(url) {
        builder = builder.resolve_to_addrs(domain, addrs);
    }
    builder
        .build()
        .map_err(|e| format!("web_fetch: could not build HTTP client: {e}"))
}

/// A client that sends every request — http forwarded, https tunnelled with
/// CONNECT — through `gateway` and nowhere else.
///
/// `no_proxy()` drops reqwest's system proxy detection, so `HTTP(S)_PROXY`
/// or `ALL_PROXY` in the environment cannot divert a request around the
/// gateway, and the gateway proxy carries no `NO_PROXY` exceptions, so no
/// host is sent direct. Name resolution and the address check happen in the
/// gateway, which dials only the address it checked; resolving here as well
/// would only add a second, unused answer. Connections are not pooled: the
/// gateway serves one forwarded request per connection.
fn gateway_client(gateway: &CallGateway) -> Result<reqwest::Client, String> {
    let proxy = reqwest::Proxy::all(gateway.proxy_url())
        .map_err(|e| format!("web_fetch: invalid egress gateway address: {e}"))?
        .no_proxy(None);
    client_builder()
        .no_proxy()
        .proxy(proxy)
        .pool_max_idle_per_host(0)
        .build()
        .map_err(|e| format!("web_fetch: could not build HTTP client: {e}"))
}

/// How hops leave the process.
#[derive(Clone, Copy)]
enum Route<'a> {
    /// No egress proxy in this session: resolve, check with the guard, pin.
    Direct(AddrGuard),
    /// Every hop through this call's gateway.
    Gateway(&'a CallGateway),
}

/// After a hop through `gateway`, the refusal it recorded, if any.
///
/// Both come from the gateway's own per-call queues, never from response
/// content, so an upstream that answers 403 (or claims to be the gateway)
/// stays an ordinary response. A policy refusal is the host-grant prompt; a
/// non-public-address refusal is a plain failure, because no grant fixes it.
fn gateway_refusal(
    gateway: &CallGateway,
    url: &reqwest::Url,
    redirected_from: Option<&reqwest::Url>,
) -> Option<Result<ToolOutput, ToolError>> {
    let host = policy_host(url).unwrap_or_default();
    let via = redirected_from
        .map(|from| format!(" (redirected from `{from}`)"))
        .unwrap_or_default();
    if let Some(denied) = gateway.sandbox_denied(&format!(
        "web_fetch: the egress gateway refused `{host}`: not in this session's allowed hosts{via}"
    )) {
        return Some(Err(denied));
    }
    gateway.take_private_address_denial().map(|refused| {
        Ok(ToolOutput::failed_exit(
            format!(
                "web_fetch: refusing to fetch `{refused}` (resolves to a non-public address){via}"
            ),
            None,
        ))
    })
}

fn is_followed_redirect(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::MOVED_PERMANENTLY
            | StatusCode::FOUND
            | StatusCode::SEE_OTHER
            | StatusCode::TEMPORARY_REDIRECT
            | StatusCode::PERMANENT_REDIRECT
    )
}

/// GET `url`, following up to [`MAX_REDIRECTS`] redirects, with the
/// allowlist applied to every hop and then either the gateway or the direct
/// SSRF guard and DNS pin (see the module docs).
///
/// Ordinary failures (bad scheme, private address, network error) are a
/// failed [`ToolOutput`] the model can read; only an allowlist refusal is an
/// error, because that one has an approval path.
async fn fetch(
    policy: Option<&EgressShared>,
    url: reqwest::Url,
    route: Route<'_>,
) -> Result<ToolOutput, ToolError> {
    let deadline = Instant::now() + REQUEST_TIMEOUT;
    let original = url.clone();
    let mut current = url;
    let mut previous: Option<reqwest::Url> = None;
    let mut redirects = 0;
    // One client serves every gateway hop: nothing in it is per host. A
    // direct hop needs its own, pinned to the addresses it checked.
    enum Dial {
        Direct(AddrGuard),
        Gateway(reqwest::Client),
    }
    let dial = match route {
        Route::Direct(blocked) => Dial::Direct(blocked),
        Route::Gateway(gateway) => match gateway_client(gateway) {
            Ok(client) => Dial::Gateway(client),
            Err(msg) => return Ok(ToolOutput::failed_exit(msg, None)),
        },
    };
    loop {
        // Allowlist first: a denied host is not even resolved, or sent to
        // the gateway, and the prompt is the same either way.
        check_host_permitted(policy, &current, previous.as_ref())?;
        let prepared = match &dial {
            Dial::Gateway(client) => check_fetchable(&current).map(|_| client.clone()),
            Dial::Direct(blocked) => match resolve_public(&current, *blocked).await {
                Ok(addrs) => pinned_client(&current, &addrs),
                Err(msg) => Err(msg),
            },
        };
        let client = match prepared {
            Ok(client) => client,
            Err(msg) => {
                let msg = match &previous {
                    Some(from) => format!("{msg} (redirected from `{from}`)"),
                    None => msg,
                };
                return Ok(ToolOutput::failed_exit(msg, None));
            }
        };
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(ToolOutput::failed_exit(
                "web_fetch: request timed out".to_string(),
                None,
            ));
        }
        let sent = client.get(current.clone()).timeout(remaining).send().await;
        // A refused CONNECT is a send error, a refused forward a 403
        // response; either way the gateway recorded why.
        if let Route::Gateway(gateway) = route {
            if let Some(refusal) = gateway_refusal(gateway, &current, previous.as_ref()) {
                return refusal;
            }
        }
        let response = match sent {
            Ok(r) => r,
            Err(e) => {
                return Ok(ToolOutput::failed_exit(
                    format!("web_fetch: request failed: {e}"),
                    None,
                ))
            }
        };
        if !is_followed_redirect(response.status()) {
            return Ok(render_response(response, &original).await);
        }
        let Some(location) = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
        else {
            return Ok(render_response(response, &original).await);
        };
        if redirects >= MAX_REDIRECTS {
            return Ok(ToolOutput::failed_exit(
                format!("web_fetch: too many redirects (more than {MAX_REDIRECTS})"),
                None,
            ));
        }
        let next = match current.join(location) {
            Ok(next) => next,
            Err(e) => {
                return Ok(ToolOutput::failed_exit(
                    format!("web_fetch: invalid redirect location `{location}`: {e}"),
                    None,
                ))
            }
        };
        redirects += 1;
        previous = Some(std::mem::replace(&mut current, next));
    }
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
         Refuses non-http(s) schemes and private/loopback network targets, and only reaches hosts this \
         session's network permissions allow (the user is asked to allow any other host)."
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

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
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

        match ctx.gateway_for_call().await {
            Ok(Some(gateway)) => fetch(host_policy(ctx), url, Route::Gateway(&gateway)).await,
            Ok(None) => fetch(host_policy(ctx), url, Route::Direct(is_blocked_ip)).await,
            // The session has an egress gateway but this call's endpoint
            // could not start. Going direct would bypass the policy.
            Err(e) => Ok(ToolOutput::failed_exit(
                format!("web_fetch: could not start the egress gateway for this call: {e}"),
                None,
            )),
        }
    }
}

/// Reads the final response and renders it into a [`ToolOutput`].
async fn render_response(response: reqwest::Response, url: &reqwest::Url) -> ToolOutput {
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
    if final_url != *url {
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
    use crate::egress::EgressPolicy;
    use crate::validation::validate_args;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// Lets tests reach the loopback server while still exercising the rest
    /// of the fetch path; one documentation address stays blocked so a
    /// redirect into it can be shown to be refused.
    fn test_guard(ip: IpAddr) -> bool {
        ip == IpAddr::from([192, 0, 2, 1])
    }

    fn policy(allow: &[&str]) -> EgressShared {
        let mut policy = EgressPolicy::new();
        for pattern in allow {
            policy.allow(*pattern);
        }
        EgressShared::new(policy, crate::egress::EgressOptions::default())
    }

    fn ctx_with_policy(allow: &[&str]) -> ToolContext {
        let mut ctx = ToolContext::new(std::env::current_dir().unwrap());
        ctx.egress = Some(std::sync::Arc::new(crate::sandbox::EgressGrant {
            proxy_port: 1,
            socket_path: std::env::temp_dir().join("forge-web-fetch-test.sock"),
            control: Some(policy(allow)),
        }));
        ctx
    }

    fn url(raw: &str) -> reqwest::Url {
        reqwest::Url::parse(raw).unwrap()
    }

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
        assert!(is_blocked_ip("::ffff:127.0.0.1".parse().unwrap()));
        assert!(!is_blocked_ip("93.184.216.34".parse().unwrap()));
        assert!(!is_blocked_ip(
            "2606:2800:220:1:248:1893:25c8:1946".parse().unwrap()
        ));
    }

    #[tokio::test]
    async fn resolve_rejects_non_http_scheme() {
        let err = resolve_public(&url("file:///etc/passwd"), is_blocked_ip)
            .await
            .unwrap_err();
        assert!(err.contains("unsupported scheme"), "{err}");
    }

    #[tokio::test]
    async fn resolve_rejects_literal_loopback() {
        let err = resolve_public(&url("http://127.0.0.1:9/"), is_blocked_ip)
            .await
            .unwrap_err();
        assert!(err.contains("non-public"), "{err}");
        let err = resolve_public(&url("http://[::1]:9/"), is_blocked_ip)
            .await
            .unwrap_err();
        assert!(err.contains("non-public"), "{err}");
    }

    /// `localhost` is a name, not a literal, so this only passes if the
    /// guard resolves names and checks what they resolve to.
    #[tokio::test]
    async fn resolve_rejects_a_name_that_resolves_to_loopback() {
        let err = resolve_public(&url("http://localhost:9/"), is_blocked_ip)
            .await
            .unwrap_err();
        assert!(err.contains("non-public"), "{err}");
    }

    #[test]
    fn allowlist_decision_follows_the_session_policy() {
        let shared = policy(&["**.example.com"]);
        assert!(
            check_host_permitted(Some(&shared), &url("https://docs.example.com/x"), None).is_ok()
        );
        let err =
            check_host_permitted(Some(&shared), &url("https://evil.test/x"), None).unwrap_err();
        let ToolError::SandboxDenied { denied_host, .. } = err else {
            panic!("expected SandboxDenied, got {err:?}");
        };
        assert_eq!(denied_host.as_deref(), Some("evil.test"));
        // No policy in context: today's behaviour, allowed.
        assert!(check_host_permitted(None, &url("https://evil.test/x"), None).is_ok());
    }

    #[test]
    fn granting_a_host_permits_the_retry() {
        let shared = policy(&[]);
        let target = url("https://api.example.com/");
        assert!(check_host_permitted(Some(&shared), &target, None).is_err());
        shared.grant_host("api.example.com");
        assert!(check_host_permitted(Some(&shared), &target, None).is_ok());
    }

    #[test]
    fn ip_literal_hosts_are_checked_without_brackets() {
        let shared = policy(&["2001:db8::1"]);
        assert!(check_host_permitted(Some(&shared), &url("http://[2001:db8::1]/"), None).is_ok());
    }

    #[tokio::test]
    async fn a_host_outside_the_allowlist_is_a_sandbox_denial() {
        let tool = WebFetchTool::new();
        let ctx = ctx_with_policy(&["docs.rs"]);
        let err = tool
            .call(&ctx, json!({"url": "https://blocked.invalid/page"}))
            .await
            .unwrap_err();
        let ToolError::SandboxDenied {
            denied_host,
            reason,
            content,
        } = err
        else {
            panic!("expected SandboxDenied, got {err:?}");
        };
        assert_eq!(denied_host.as_deref(), Some("blocked.invalid"));
        assert_eq!(reason, crate::egress::HOST_DENIED_EXPLANATION);
        assert!(content.contains("blocked.invalid"), "{content}");
    }

    /// The allowlist does not replace the SSRF guard: a host permitted by a
    /// wildcard that resolves privately is still refused — by the gateway,
    /// since this session has one — and the refusal is a plain failure, not
    /// a host-grant prompt, because no grant could fix it.
    #[tokio::test]
    async fn a_permitted_host_still_passes_the_ssrf_guard() {
        let tool = WebFetchTool::new();
        let ctx = ctx_with_policy(&["**.localhost"]);
        let out = tool
            .call(&ctx, json!({"url": "http://localhost:9/"}))
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("non-public"), "{}", out.content);
        assert!(out.content.contains("`localhost`"), "{}", out.content);
    }

    /// A grant with no live proxy behind it is no gateway: the direct path,
    /// with its own SSRF guard, is what runs.
    #[tokio::test]
    async fn a_grant_without_a_live_proxy_keeps_the_direct_path() {
        let tool = WebFetchTool::new();
        let mut ctx = ToolContext::new(std::env::current_dir().unwrap());
        ctx.egress = Some(std::sync::Arc::new(crate::sandbox::EgressGrant {
            proxy_port: 1,
            socket_path: std::env::temp_dir().join("forge-web-fetch-test.sock"),
            control: None,
        }));
        assert!(ctx.gateway_for_call().await.unwrap().is_none());
        let out = tool
            .call(&ctx, json!({"url": "http://127.0.0.1:1/"}))
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("non-public"), "{}", out.content);
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
    /// headers (discarded) then writes the given raw response bytes. Returns
    /// the bound port.
    fn spawn_http_server(response: String) -> Option<u16> {
        let listener = match TcpListener::bind("127.0.0.1:0") {
            Ok(listener) => listener,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("skipping: this host denies binding a loopback listener");
                return None;
            }
            Err(e) => panic!("loopback bind failed: {e}"),
        };
        let port = listener.local_addr().unwrap().port();
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
        Some(port)
    }

    fn redirect_to(location: &str) -> String {
        format!(
            "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
    }

    #[tokio::test]
    async fn fetches_and_converts_html() {
        let Some(port) = spawn_http_server(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n\
             <html><body><h1>Hi</h1><p>there</p></body></html>"
                .into(),
        ) else {
            return;
        };
        let out = fetch(
            None,
            url(&format!("http://127.0.0.1:{port}/")),
            Route::Direct(test_guard),
        )
        .await
        .unwrap();
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("Hi"), "{}", out.content);
        assert!(out.content.contains("there"), "{}", out.content);
    }

    /// A permitted name goes through allowlist, resolution and the pinned
    /// connect, and reaches the server.
    #[tokio::test]
    async fn a_permitted_host_is_fetched() {
        let Some(port) = spawn_http_server(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\npermitted"
                .into(),
        ) else {
            return;
        };
        let shared = policy(&["localhost"]);
        let out = fetch(
            Some(&shared),
            url(&format!("http://localhost:{port}/")),
            Route::Direct(test_guard),
        )
        .await
        .unwrap();
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("permitted"), "{}", out.content);
    }

    #[tokio::test]
    async fn a_redirect_to_a_host_outside_the_allowlist_is_denied() {
        let Some(port) = spawn_http_server(redirect_to("https://elsewhere.invalid/next")) else {
            return;
        };
        let shared = policy(&["localhost"]);
        let err = fetch(
            Some(&shared),
            url(&format!("http://localhost:{port}/")),
            Route::Direct(test_guard),
        )
        .await
        .unwrap_err();
        let ToolError::SandboxDenied {
            denied_host,
            content,
            ..
        } = err
        else {
            panic!("expected SandboxDenied, got {err:?}");
        };
        assert_eq!(denied_host.as_deref(), Some("elsewhere.invalid"));
        assert!(content.contains("redirected"), "{content}");
    }

    /// Without a policy the allowlist is skipped, but every hop is still
    /// checked against the SSRF guard — including a literal address that the
    /// old redirect hook only caught for literals and this one catches before
    /// connecting.
    #[tokio::test]
    async fn a_redirect_to_a_blocked_address_is_refused() {
        let Some(port) = spawn_http_server(redirect_to("http://192.0.2.1/secret")) else {
            return;
        };
        let out = fetch(
            None,
            url(&format!("http://127.0.0.1:{port}/")),
            Route::Direct(test_guard),
        )
        .await
        .unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("non-public"), "{}", out.content);
        assert!(out.content.contains("redirected from"), "{}", out.content);
    }

    #[tokio::test]
    async fn non_success_status_is_reported_as_error() {
        let Some(port) = spawn_http_server(
            "HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\nnope"
                .into(),
        ) else {
            return;
        };
        let out = fetch(
            None,
            url(&format!("http://127.0.0.1:{port}/")),
            Route::Direct(test_guard),
        )
        .await
        .unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("404"), "{}", out.content);
    }

    #[tokio::test]
    async fn binary_content_type_is_refused() {
        let Some(port) = spawn_http_server(
            "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n\x00\x01\x02"
                .into(),
        ) else {
            return;
        };
        let out = fetch(
            None,
            url(&format!("http://127.0.0.1:{port}/")),
            Route::Direct(test_guard),
        )
        .await
        .unwrap();
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

    // -- Through the per-call gateway --------------------------------------

    /// A session policy whose gateway may reach the loopback test server
    /// behind any permitted name.
    fn loopback_policy(allow: &[&str]) -> EgressShared {
        let mut policy = EgressPolicy::new();
        for pattern in allow {
            policy.allow(*pattern);
        }
        EgressShared::new(
            policy,
            crate::egress::EgressOptions {
                allow_private_destinations: true,
                ..Default::default()
            },
        )
    }

    fn ctx_for(shared: &EgressShared, call_id: &str) -> ToolContext {
        let mut ctx = ToolContext::new(std::env::current_dir().unwrap());
        ctx.egress = Some(std::sync::Arc::new(crate::sandbox::EgressGrant {
            proxy_port: 1,
            socket_path: std::env::temp_dir().join("forge-web-fetch-test.sock"),
            control: Some(shared.clone()),
        }));
        ctx.with_tool_call_id(call_id)
    }

    /// The call's gateway, or `None` (skip) where binding is not allowed.
    async fn gateway(shared: &EgressShared, call_id: &str) -> Option<CallGateway> {
        match ctx_for(shared, call_id).gateway_for_call().await {
            Ok(gateway) => Some(gateway.expect("a live grant yields a gateway")),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("skipping: this host denies binding a listener");
                None
            }
            Err(e) => panic!("gateway failed to start: {e}"),
        }
    }

    /// Events are recorded when a connection closes, which can trail the
    /// response the client already read.
    async fn events_for(shared: &EgressShared, want: usize) -> Vec<crate::egress::EgressEvent> {
        let mut events = Vec::new();
        for _ in 0..100 {
            events.extend(shared.drain_events());
            if events.len() >= want {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        events
    }

    #[tokio::test]
    async fn a_fetch_through_the_gateway_is_attributed_to_the_call() {
        let Some(port) = spawn_http_server(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\nvia gateway"
                .into(),
        ) else {
            return;
        };
        let shared = loopback_policy(&["localhost"]);
        if gateway(&shared, "probe").await.is_none() {
            return;
        }
        let out = WebFetchTool::new()
            .call(
                &ctx_for(&shared, "call-web"),
                json!({"url": format!("http://localhost:{port}/")}),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("via gateway"), "{}", out.content);

        let events = events_for(&shared, 1).await;
        let event = events
            .iter()
            .find(|event| event.allowed())
            .unwrap_or_else(|| panic!("no allowed connection logged: {events:?}"));
        assert_eq!(event.label.as_deref(), Some("call-web"));
        assert_eq!(event.host, "localhost");
        assert_eq!(event.port, port);
    }

    /// With no allowlist pre-check in front, the gateway itself refuses the
    /// host — over CONNECT (https) and over forwarded plain http — and the
    /// refusal is the host-grant prompt carrying the host.
    #[tokio::test]
    async fn a_host_denied_at_the_gateway_is_a_sandbox_denial() {
        let shared = loopback_policy(&["localhost"]);
        let Some(gateway) = gateway(&shared, "call-deny").await else {
            return;
        };
        for target in ["http://denied.invalid/page", "https://denied.invalid/page"] {
            let err = fetch(None, url(target), Route::Gateway(&gateway))
                .await
                .unwrap_err();
            let ToolError::SandboxDenied {
                denied_host,
                content,
                reason,
            } = err
            else {
                panic!("{target}: expected SandboxDenied, got {err:?}");
            };
            assert_eq!(denied_host.as_deref(), Some("denied.invalid"), "{target}");
            assert_eq!(reason, crate::egress::HOST_DENIED_EXPLANATION);
            assert!(content.contains("denied.invalid"), "{content}");
            assert!(gateway.denied_hosts().is_empty(), "the denial is consumed");
        }
    }

    #[tokio::test]
    async fn a_redirect_through_the_gateway_to_a_denied_host_is_denied() {
        let Some(port) = spawn_http_server(redirect_to("http://elsewhere.invalid/next")) else {
            return;
        };
        let shared = loopback_policy(&["localhost"]);
        let Some(gateway) = gateway(&shared, "call-redirect").await else {
            return;
        };
        let err = fetch(
            None,
            url(&format!("http://localhost:{port}/")),
            Route::Gateway(&gateway),
        )
        .await
        .unwrap_err();
        let ToolError::SandboxDenied {
            denied_host,
            content,
            ..
        } = err
        else {
            panic!("expected SandboxDenied, got {err:?}");
        };
        assert_eq!(denied_host.as_deref(), Some("elsewhere.invalid"));
        assert!(content.contains("redirected from"), "{content}");
    }

    /// The same redirect with the session policy in front is caught by the
    /// pre-check, before anything reaches the gateway.
    #[tokio::test]
    async fn the_allowlist_pre_check_still_runs_on_the_gateway_path() {
        let Some(port) = spawn_http_server(redirect_to("https://elsewhere.invalid/next")) else {
            return;
        };
        let shared = loopback_policy(&["localhost"]);
        let err = WebFetchTool::new()
            .call(
                &ctx_for(&shared, "call-pre"),
                json!({"url": format!("http://localhost:{port}/")}),
            )
            .await
            .unwrap_err();
        let ToolError::SandboxDenied { denied_host, .. } = err else {
            panic!("expected SandboxDenied, got {err:?}");
        };
        assert_eq!(denied_host.as_deref(), Some("elsewhere.invalid"));
        let events = events_for(&shared, 1).await;
        assert!(
            events.iter().all(|event| event.host != "elsewhere.invalid"),
            "the denied hop must not reach the gateway: {events:?}"
        );
    }

    /// An upstream's own 403 is an ordinary response, not a host prompt:
    /// only a refusal the gateway recorded becomes one.
    #[tokio::test]
    async fn an_upstream_403_through_the_gateway_is_not_a_denial() {
        let Some(port) = spawn_http_server(
            "HTTP/1.1 403 Forge Sandbox Denied\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\nForge Sandbox Denied: host x is not allowed"
                .into(),
        ) else {
            return;
        };
        let shared = loopback_policy(&["localhost"]);
        let Some(gateway) = gateway(&shared, "call-403").await else {
            return;
        };
        let out = fetch(
            None,
            url(&format!("http://localhost:{port}/")),
            Route::Gateway(&gateway),
        )
        .await
        .unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("403"), "{}", out.content);
    }
}
