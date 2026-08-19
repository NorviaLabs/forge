//! Domain-filtered network egress for confined processes.
//!
//! Neither sandbox mechanism can express a domain rule. Seatbelt's
//! `network-outbound` filters by IP, and bubblewrap's `--unshare-net` is
//! all-or-nothing — so "allow crates.io, deny everything else" cannot be a
//! sandbox flag. It needs something that speaks the protocol. This is why
//! Codex ships `network_proxy` as a separate feature rather than a sandbox
//! setting.
//!
//! # Why CONNECT filtering rather than MITM
//!
//! An HTTPS proxy can enforce domains two ways. It can terminate TLS, inspect,
//! and re-encrypt — which needs a generated CA installed into the sandbox's
//! trust store, and puts a CA private key on disk. Compromise that key and you
//! have not merely bypassed the sandbox, you can forge any certificate the
//! machine will trust.
//!
//! Or it can filter `CONNECT`. The client announces its destination in
//! plaintext (`CONNECT github.com:443 HTTP/1.1`) *before* the TLS handshake,
//! so the hostname is checkable without decrypting anything. Once allowed, the
//! proxy copies bytes and never sees the plaintext.
//!
//! The second is strictly better here: it gives the same domain-level decision
//! with no key material, no trust-store surgery, and no ability to read the
//! traffic. The cost is that the decision is per-host, not per-URL — which is
//! the right granularity for a dependency allowlist anyway.
//!
//! # What this module does not do
//!
//! Running the proxy is not the same as *forcing* traffic through it. That is
//! per-platform routing and is deliberately not decided here — see
//! `EgressProxy::addr`.

use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, RwLock};

use std::path::{Path, PathBuf};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream, UnixListener, UnixStream};

/// Hosts a confined process may reach.
///
/// Matching follows Codex's proxy rules, which are worth copying because the
/// distinction between them is easy to get wrong:
///
/// * `example.com` — that exact host, nothing else
/// * `*.example.com` — subdomains only; `example.com` itself is **not** matched
/// * `**.example.com` — the apex and every subdomain
///
/// A deny always wins over an allow, so a broad allow cannot be used to
/// re-permit something explicitly denied.
#[derive(Debug, Clone, Default)]
pub struct EgressPolicy {
    allow: Vec<String>,
    deny: Vec<String>,
}

pub const SANDBOX_DENIED_REASON: &str = "Forge Sandbox Denied";

/// Explanation attached when the proxy refused a host, even if the client
/// printed something else (HTTP 403, "token invalid", "Forbidden", …).
pub const HOST_DENIED_EXPLANATION: &str =
    "blocked by the sandbox: the destination host is not allowed by \
     the personal host(...) network permissions.";

/// After a confined process exits non-zero, decide whether the sandbox
/// (filesystem or egress) is why.
///
/// The proxy log is the source of truth for network. Client output is only
/// used when `explain_denial` recognises a boundary string. A host taken
/// from the grant is never inferred from a URL in the output alone — that
/// would turn a real 403 from an already-allowed host into a grant prompt.
pub fn denial_for_failed_confined_command(
    output: &str,
    workspace_root: &std::path::Path,
    grant: Option<&crate::sandbox::EgressGrant>,
) -> Option<crate::ToolError> {
    let from_proxy = grant.and_then(crate::sandbox::EgressGrant::take_denied_host);
    let (reason, denied_host) =
        if let Some(explanation) = crate::sandbox::explain_denial(output, workspace_root) {
            (
                explanation.to_string(),
                from_proxy.or_else(|| extract_denied_host(output)),
            )
        } else {
            let host = from_proxy?;
            (HOST_DENIED_EXPLANATION.to_string(), Some(host))
        };
    let mut content = output.to_string();
    if !content.contains(&reason) {
        if !content.is_empty() {
            content.push('\n');
        }
        content.push_str(&reason);
    }
    Some(crate::ToolError::SandboxDenied {
        content,
        reason,
        denied_host,
    })
}

impl EgressPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a policy from the merged permission file.
    ///
    /// Only `host(...)` rules affect egress. Other patterns (`bash(...)`,
    /// `fetch(...)`) stay on the HITL path. An empty file — the default —
    /// permits nothing. `host(*)` is unrestricted network through the proxy;
    /// a deny still wins over it.
    pub fn from_permissions(file: &forge_config::PermissionsFile) -> Self {
        let mut policy = Self::new();
        for pattern in file.allow.iter().filter_map(|raw| host_rule(raw)) {
            policy.allow(pattern);
        }
        for pattern in file.deny.iter().filter_map(|raw| host_rule(raw)) {
            policy.deny(pattern);
        }
        policy
    }

    pub fn is_empty(&self) -> bool {
        self.allow.is_empty() && self.deny.is_empty()
    }

    pub fn allow(&mut self, pattern: impl Into<String>) -> &mut Self {
        let pattern = pattern.into().to_ascii_lowercase();
        if !self.allow.iter().any(|existing| existing == &pattern) {
            self.allow.push(pattern);
        }
        self
    }

    pub fn deny(&mut self, pattern: impl Into<String>) -> &mut Self {
        self.deny.push(pattern.into().to_ascii_lowercase());
        self
    }

    /// Whether `host` may be reached. Fails closed: an empty policy permits
    /// nothing.
    pub fn permits(&self, host: &str) -> bool {
        let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
        // An empty label makes suffix matching lie: `.github.com` ends with
        // `.github.com`, so a wildcard for that domain accepts it. Rejecting
        // empty labels here fixes every pattern form at once, rather than
        // teaching each one the same trick.
        if host.is_empty() || host.starts_with('.') || host.contains("..") {
            return false;
        }
        if self.deny.iter().any(|p| matches_pattern(p, &host)) {
            return false;
        }
        self.allow.iter().any(|p| matches_pattern(p, &host))
    }
}

fn host_rule(raw: &str) -> Option<&str> {
    let raw = raw.trim();
    let inner = raw.strip_prefix("host(")?.strip_suffix(')')?.trim();
    (!inner.is_empty()).then_some(inner)
}

fn matches_pattern(pattern: &str, host: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(suffix) = pattern.strip_prefix("**.") {
        return host == suffix || host.ends_with(&format!(".{suffix}"));
    }
    if let Some(suffix) = pattern.strip_prefix("*.") {
        // Subdomains only — the apex is deliberately excluded, so `*.` cannot
        // be used to quietly permit the bare domain too.
        return host != suffix && host.ends_with(&format!(".{suffix}"));
    }
    pattern == host
}

/// Live policy and the hosts this proxy has refused, shared by every listener.
///
/// The session mutates the policy when the user grants a host so the next
/// CONNECT (and a confined retry of the same command) sees the new allow
/// without tearing down the socket.
#[derive(Clone, Debug)]
pub struct EgressShared {
    policy: Arc<RwLock<EgressPolicy>>,
    denied: Arc<Mutex<Vec<String>>>,
}

impl EgressShared {
    fn new(policy: EgressPolicy) -> Self {
        Self {
            policy: Arc::new(RwLock::new(policy)),
            denied: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Permit `pattern` for the rest of this proxy's life.
    pub fn grant_host(&self, pattern: &str) {
        self.policy
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .allow(pattern);
    }

    pub fn take_denied_host(&self) -> Option<String> {
        let mut denied = self
            .denied
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let host = denied.first().cloned();
        denied.clear();
        host
    }

    pub fn record_denied(&self, host: String) {
        self.denied
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(host);
    }

    fn permits(&self, host: &str) -> bool {
        self.policy
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .permits(host)
    }
}

/// A running CONNECT proxy.
///
/// Dropping it stops accepting new connections.
pub struct EgressProxy {
    addr: SocketAddr,
    socket_path: Option<PathBuf>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    shared: EgressShared,
}

impl Drop for EgressProxy {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
        // The socket file outlives the listener otherwise, and a stale path
        // would let a later bind fail with EADDRINUSE.
        if let Some(path) = &self.socket_path {
            let _ = std::fs::remove_file(path);
        }
    }
}

impl EgressProxy {
    /// Bind to an ephemeral port on loopback and start serving.
    pub async fn start(policy: EgressPolicy) -> io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let addr = listener.local_addr()?;
        let shared = EgressShared::new(policy);

        let accept = shared.clone();
        let task = tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                let shared = accept.clone();
                tokio::spawn(async move {
                    let _ = serve(client, shared).await;
                });
            }
        });

        Ok(Self {
            addr,
            socket_path: None,
            tasks: vec![task],
            shared,
        })
    }

    pub fn shared(&self) -> &EgressShared {
        &self.shared
    }

    /// Additionally serve on a Unix socket at `path`.
    ///
    /// This is what makes the proxy reachable from inside a sandbox that has
    /// no network at all. `--unshare-net` removes the network namespace, so
    /// loopback TCP is gone — but **a Unix socket is a filesystem object, not
    /// a network one**, so a bind-mounted socket still crosses the boundary.
    /// That is how a confined process can reach exactly one destination
    /// without a userspace network stack, root, or slirp4netns.
    ///
    /// Both Claude Code and Codex do this: Claude Code shells out to `socat`
    /// as the relay, Codex bridges TCP→UDS→TCP in-process. Bridging in-process
    /// is better here — forge is already a Rust binary, and it avoids making
    /// `socat` a dependency users must install.
    ///
    /// **The socket alone is not the boundary.** A process that can create its
    /// own `AF_UNIX` sockets can reach anything else bind-mounted in, and forge
    /// does not stop it opening one. Codex blocks that with seccomp once its
    /// bridge is live; forge cannot copy that directly, because the relay runs
    /// *inside* the sandbox and needs the same syscall a filter would remove.
    /// The sandbox's filesystem rules are therefore what limit which sockets
    /// are reachable — asserted by
    /// `a_confined_command_reaches_workspace_sockets_but_not_host_sockets`.
    /// See `SandboxPolicy::with_egress_socket` for the full reasoning.
    pub async fn serve_on_unix_socket(
        &mut self,
        path: impl AsRef<Path>,
        _policy: EgressPolicy,
    ) -> io::Result<()> {
        let path = path.as_ref().to_path_buf();
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        let shared = self.shared.clone();

        self.tasks.push(tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                let shared = shared.clone();
                tokio::spawn(async move {
                    let _ = serve_unix(client, shared).await;
                });
            }
        }));
        self.socket_path = Some(path);
        Ok(())
    }

    /// The Unix socket being served, if one was requested.
    pub fn socket_path(&self) -> Option<&Path> {
        self.socket_path.as_deref()
    }

    /// Where the proxy is listening.
    ///
    /// Pointing a process at this address is *not* enforcement on its own — a
    /// process free to open sockets can ignore `HTTPS_PROXY` and connect
    /// directly. The sandbox has to be the thing that makes this the only
    /// reachable destination, and how varies by platform:
    ///
    /// * **macOS** — Seatbelt can deny all outbound except this port, so the
    ///   routing is enforced.
    /// * **Linux** — `--unshare-net` denies loopback too, so the proxy is
    ///   unreachable; permitting only it needs a userspace network
    ///   (slirp4netns/pasta) rather than a bwrap flag. Until that exists,
    ///   pointing at the proxy on Linux is a convention, not a boundary, and
    ///   must not be described as one.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
}

/// Handle one client, whatever transport it arrived on.
///
/// The stream is split *before* buffering so the read half can be buffered
/// while the write half stays usable. Wrapping the whole stream and later
/// unwrapping would discard anything the client pipelined behind its headers.
async fn serve<S>(client: S, shared: EgressShared) -> io::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + 'static,
{
    let (read_half, mut write_half) = tokio::io::split(client);
    let mut reader = BufReader::new(read_half);

    let mut request = String::new();
    reader.read_line(&mut request).await?;

    let Some(target) = parse_connect(&request) else {
        return respond(&mut write_half, 400, "Bad Request", None).await;
    };

    // Drain the remaining request headers before replying.
    let mut header = String::new();
    loop {
        header.clear();
        let read = reader.read_line(&mut header).await?;
        if read == 0 || header.trim().is_empty() {
            break;
        }
    }

    // Parse once, then dial the parsed parts. Deriving a host for the policy
    // check while dialling the original string lets the two disagree, and a
    // target the policy never saw is a bypass however it is spelled — the
    // earlier version approved `[crates.io]:443` as `crates.io` and then dialled
    // the bracketed form.
    let Some((host, port)) = split_host_port(&target) else {
        return respond(&mut write_half, 400, "Bad Request", None).await;
    };

    if !shared.permits(&host) {
        shared.record_denied(host.clone());
        return respond(&mut write_half, 403, SANDBOX_DENIED_REASON, Some(&host)).await;
    }

    let upstream = match TcpStream::connect((host.as_str(), port)).await {
        Ok(stream) => stream,
        Err(_) => return respond(&mut write_half, 502, "Bad Gateway", None).await,
    };

    write_half
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await?;

    // From here the proxy copies bytes and never sees the plaintext.
    let (mut upstream_read, mut upstream_write) = tokio::io::split(upstream);
    let client_to_upstream =
        tokio::spawn(async move { tokio::io::copy(&mut reader, &mut upstream_write).await });
    let upstream_to_client =
        tokio::spawn(async move { tokio::io::copy(&mut upstream_read, &mut write_half).await });
    let _ = tokio::join!(client_to_upstream, upstream_to_client);
    Ok(())
}

async fn serve_unix(client: UnixStream, shared: EgressShared) -> io::Result<()> {
    serve(client, shared).await
}

/// Family pattern for a denied host, used as `host(...)` in permissions.toml.
///
/// `api.github.com` and `github.com` both become `**.github.com` so one grant
/// covers `gh`, git-over-https, and the rest of that apex.
pub fn suggest_host_pattern(host: &str) -> String {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if host.parse::<std::net::IpAddr>().is_ok() || !host.contains('.') {
        return host;
    }
    let labels: Vec<&str> = host.split('.').filter(|label| !label.is_empty()).collect();
    if labels.len() >= 2 {
        format!(
            "**.{}.{}",
            labels[labels.len() - 2],
            labels[labels.len() - 1]
        )
    } else {
        host
    }
}

pub fn host_allow_rule(host: &str) -> String {
    format!("host({})", suggest_host_pattern(host))
}

/// Best-effort host from a failed command's output, when the proxy log is not
/// available (tests, or a client that never printed the 403 body).
pub fn extract_denied_host(output: &str) -> Option<String> {
    if let Some(host) = output
        .split("host ")
        .nth(1)
        .and_then(|rest| rest.split(" is not allowed").next())
    {
        if let Some(host) = sanitize_host(host) {
            return Some(host);
        }
    }
    for marker in [
        "Could not resolve host: ",
        "Could not resolve host ",
        "Failed to connect to ",
    ] {
        if let Some(rest) = output.split(marker).nth(1) {
            let token = rest
                .split(|c: char| c.is_whitespace() || ['\'', '"', ')', ','].contains(&c))
                .next()
                .unwrap_or("");
            if let Some(host) = sanitize_host(token) {
                return Some(host);
            }
        }
    }
    for prefix in ["https://", "http://"] {
        if let Some(idx) = output.find(prefix) {
            let rest = &output[idx + prefix.len()..];
            let hostport = rest
                .split(['/', '"', '\'', ' ', '\n', '?', '#'])
                .next()
                .unwrap_or("");
            let host = hostport
                .rsplit_once('@')
                .map(|(_, h)| h)
                .unwrap_or(hostport);
            let host = host.rsplit_once(']').map(|(_, h)| h).unwrap_or(host);
            let host = host.split(':').next().unwrap_or(host);
            if let Some(host) = sanitize_host(host) {
                return Some(host);
            }
        }
    }
    None
}

fn sanitize_host(raw: &str) -> Option<String> {
    let host = raw
        .trim()
        .trim_matches(|c| matches!(c, '.' | '"' | '\'' | '`'))
        .to_ascii_lowercase();
    if host.is_empty() || host.starts_with('.') || host.contains("..") || host.contains('/') {
        return None;
    }
    if host
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
    {
        Some(host)
    } else {
        None
    }
}

/// Split a CONNECT target into the host to authorize and the port to dial.
///
/// Strict on purpose. Brackets are the IPv6 literal form, so bracketed content
/// that is not an IPv6 address is malformed rather than a hostname with
/// decoration — accepting it is what let `[crates.io]:443` be checked as one
/// thing and dialled as another.
fn split_host_port(target: &str) -> Option<(String, u16)> {
    if let Some(rest) = target.strip_prefix('[') {
        let (host, tail) = rest.split_once(']')?;
        host.parse::<std::net::Ipv6Addr>().ok()?;
        let port = tail.strip_prefix(':')?.parse().ok()?;
        return Some((host.to_string(), port));
    }

    let (host, port) = target.rsplit_once(':')?;
    // A bare IPv6 address without brackets is ambiguous with host:port, and
    // an empty host is never valid.
    if host.is_empty() || host.contains(':') {
        return None;
    }
    Some((host.to_string(), port.parse().ok()?))
}

fn parse_connect(line: &str) -> Option<String> {
    let mut parts = line.split_whitespace();
    if !parts.next()?.eq_ignore_ascii_case("CONNECT") {
        return None;
    }
    let target = parts.next()?;
    (!target.is_empty()).then(|| target.to_string())
}

async fn respond<W>(
    client: &mut W,
    code: u16,
    reason: &str,
    denied_host: Option<&str>,
) -> io::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let body = match denied_host {
        Some(host) => format!("{reason}: host {host} is not allowed\n"),
        None => String::new(),
    };
    let header = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    client.write_all(header.as_bytes()).await?;
    if !body.is_empty() {
        client.write_all(body.as_bytes()).await?;
    }
    client.shutdown().await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn connect_status(port: u16, target: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        stream
            .write_all(format!("CONNECT {target} HTTP/1.1\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut reader = BufReader::new(stream);
        let mut status = String::new();
        reader.read_line(&mut status).await.unwrap();
        status
    }

    fn policy(patterns: &[&str]) -> EgressPolicy {
        let mut p = EgressPolicy::new();
        for pattern in patterns {
            p.allow(*pattern);
        }
        p
    }

    #[test]
    fn an_empty_policy_permits_nothing() {
        assert!(!EgressPolicy::new().permits("crates.io"));
    }

    #[tokio::test]
    async fn denied_connect_identifies_the_sandbox() {
        let proxy = EgressProxy::start(EgressPolicy::new()).await.unwrap();
        let status = connect_status(proxy.addr().port(), "github.com:443").await;
        assert_eq!(status, "HTTP/1.1 403 Forge Sandbox Denied\r\n");
    }

    #[test]
    fn exact_hosts_match_only_themselves() {
        let p = policy(&["crates.io"]);
        assert!(p.permits("crates.io"));
        assert!(!p.permits("evil-crates.io"));
        assert!(!p.permits("static.crates.io"));
        assert!(!p.permits("crates.io.evil.com"));
    }

    /// The distinction worth pinning: `*.` is subdomains only, so it cannot be
    /// used to quietly permit the apex as well.
    #[test]
    fn single_star_excludes_the_apex_and_double_star_includes_it() {
        let single = policy(&["*.example.com"]);
        assert!(single.permits("api.example.com"));
        assert!(!single.permits("example.com"));

        let double = policy(&["**.example.com"]);
        assert!(double.permits("api.example.com"));
        assert!(double.permits("example.com"));
    }

    /// A suffix match must not be satisfied by a host that merely ends with
    /// the same letters — `notexample.com` is not a subdomain of
    /// `example.com`.
    #[test]
    fn suffix_matching_respects_label_boundaries() {
        let p = policy(&["**.example.com"]);
        assert!(!p.permits("notexample.com"));
        assert!(!p.permits("example.com.attacker.net"));
    }

    #[test]
    fn deny_beats_allow() {
        let mut p = policy(&["**.example.com"]);
        p.deny("secrets.example.com");
        assert!(p.permits("api.example.com"));
        assert!(!p.permits("secrets.example.com"));
    }

    #[test]
    fn matching_ignores_case_and_a_trailing_root_dot() {
        let p = policy(&["crates.io"]);
        assert!(p.permits("CRATES.IO"));
        assert!(p.permits("crates.io."));
    }

    #[test]
    fn from_permissions_reads_only_host_rules() {
        let file = forge_config::PermissionsFile {
            allow: vec![
                "host(**.example.com)".into(),
                "bash(cargo test *)".into(),
                "fetch(docs.example.com)".into(),
                "host( )".into(),
            ],
            deny: vec!["host(evil.example.com)".into(), "bash(curl*)".into()],
        };
        let p = EgressPolicy::from_permissions(&file);
        assert!(p.permits("api.example.com"));
        assert!(p.permits("example.com"));
        assert!(!p.permits("evil.example.com"));
        assert!(!p.permits("crates.io"));
        assert!(!p.permits("registry.npmjs.org"));
    }

    #[test]
    fn a_star_allow_is_unrestricted_except_for_denies() {
        let file = forge_config::PermissionsFile {
            allow: vec!["host(*)".into()],
            deny: vec!["host(evil.example.com)".into()],
        };
        let p = EgressPolicy::from_permissions(&file);
        assert!(p.permits("crates.io"));
        assert!(p.permits("registry.npmjs.org"));
        assert!(p.permits("pypi.org"));
        assert!(p.permits("github.com"));
        assert!(!p.permits("evil.example.com"));
    }

    #[test]
    fn an_empty_permissions_file_permits_nothing() {
        let p = EgressPolicy::from_permissions(&forge_config::PermissionsFile::default());
        for host in [
            "crates.io",
            "static.crates.io",
            "registry.npmjs.org",
            "pypi.org",
            "github.com",
            "api.github.com",
        ] {
            assert!(!p.permits(host), "{host} must not be pre-allowed");
        }
    }

    #[test]
    fn connect_lines_are_parsed_and_anything_else_rejected() {
        assert_eq!(
            parse_connect("CONNECT github.com:443 HTTP/1.1\r\n").as_deref(),
            Some("github.com:443")
        );
        assert_eq!(
            parse_connect("connect github.com:443 HTTP/1.1\r\n").as_deref(),
            Some("github.com:443")
        );
        assert!(parse_connect("GET /secrets HTTP/1.1\r\n").is_none());
        assert!(parse_connect("").is_none());
    }

    /// Confusable and malformed hosts must fail closed.
    ///
    /// The allow-list is the only thing deciding where a confined command may
    /// send bytes, so the interesting direction is a host that is *not* the
    /// allowed one but matches anyway. `permits` only ASCII-lowercases, which
    /// is correct — Unicode case folding would map distinct hosts onto each
    /// other — but it means every non-ASCII variant has to miss, and that is
    /// worth pinning rather than assuming.
    #[test]
    fn confusable_and_malformed_hosts_are_denied() {
        let p = policy(["crates.io", "**.github.com"].as_slice());

        // Sanity: the things that must keep working, so a policy that denies
        // everything cannot pass this test.
        for permitted in [
            "crates.io",
            "CRATES.IO",
            "crates.io.",
            " crates.io ",
            "github.com",
            "api.github.com",
            "deep.nested.github.com",
        ] {
            assert!(p.permits(permitted), "{permitted:?} must stay reachable");
        }

        let denied = [
            // Cyrillic 'с' (U+0441) for ASCII 'c' — the classic homograph.
            ("\u{441}rates.io", "cyrillic homograph"),
            // Fullwidth 'c' (U+FF43).
            ("\u{ff43}rates.io", "fullwidth homograph"),
            // Turkish dotless i — ASCII lowercasing leaves it alone.
            ("crates.\u{131}o", "dotless i"),
            // Punycode of the Cyrillic spelling: a different host entirely.
            ("xn--rates-4td.io", "punycode of a homograph"),
            // Suffix matching must respect label boundaries.
            ("evilcrates.io", "no label boundary"),
            ("crates.io.evil.com", "allowed name as a prefix"),
            ("notgithub.com", "no label boundary under a wildcard"),
            ("github.com.evil.com", "wildcard apex as a prefix"),
            // Embedded separators must not smuggle a second host past the
            // comparison.
            ("crates.io@evil.com", "userinfo style"),
            ("evil.com@crates.io", "reversed userinfo"),
            ("crates.io\u{0}evil.com", "null byte"),
            ("crates.io/evil.com", "path style"),
            ("crates.io:evil.com", "colon style"),
            ("crates.io#evil.com", "fragment style"),
            ("evil.com?crates.io", "query style"),
            // Nothing at all.
            ("", "empty"),
            (".", "bare dot"),
            ("..", "bare dots"),
        ];

        for (host, why) in denied {
            assert!(
                !p.permits(host),
                "{host:?} ({why}) was permitted; the allow-list is what decides \
                 where a confined command may send bytes"
            );
        }
    }

    /// A leading dot is not a label boundary trick.
    ///
    /// `.github.com` ends with `.github.com`, so a naive suffix check accepts
    /// it. It is not a resolvable host, so this is a hygiene case rather than a
    /// live bypass — but suffix matching is exactly where allow-lists fail, and
    /// the check should not depend on a resolver refusing to cooperate.
    #[test]
    fn a_leading_dot_does_not_satisfy_a_wildcard() {
        let p = policy(["**.github.com", "*.gitlab.com"].as_slice());
        assert!(!p.permits(".github.com"));
        assert!(!p.permits(".gitlab.com"));
    }

    /// Deny wins over allow no matter which pattern form each side uses.
    #[test]
    fn deny_beats_allow_across_pattern_forms() {
        let mut p = EgressPolicy::new();
        p.allow("**.internal.example");
        p.deny("secrets.internal.example");
        assert!(p.permits("ok.internal.example"));
        assert!(!p.permits("secrets.internal.example"));
        assert!(
            !p.permits("SECRETS.INTERNAL.EXAMPLE"),
            "deny must be case-insensitive too"
        );
        assert!(
            !p.permits("secrets.internal.example."),
            "a trailing dot must not evade a deny"
        );
    }

    /// The proxy must refuse the target it will actually dial.
    ///
    /// `serve` validates a `host` it derives from the CONNECT target, then
    /// dials the *target*. Any input where those two disagree is a bypass, so
    /// this drives real CONNECT lines through a real proxy rather than calling
    /// `permits` directly.
    ///
    /// 403 and 502 are the whole point of the assertion. 403 means the policy
    /// refused. 502 means the policy **allowed** it and the dial merely failed
    /// — a bypass that happens to be unroutable today, which is not a boundary.
    #[tokio::test]
    async fn adversarial_connect_targets_are_refused_by_policy_not_by_dns() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let mut policy = EgressPolicy::new();
        policy.allow("crates.io");
        let proxy = EgressProxy::start(policy).await.unwrap();

        let status = |line: String| {
            let addr = proxy.addr();
            async move {
                let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
                stream
                    .write_all(format!("{line}\r\n\r\n").as_bytes())
                    .await
                    .unwrap();
                let mut reader = BufReader::new(stream);
                let mut response = String::new();
                reader.read_line(&mut response).await.unwrap();
                response
            }
        };

        for (target, why) in [
            (
                "[crates.io]:443",
                "brackets are stripped from the checked host but kept in the dialled target",
            ),
            ("crates.io.evil.com:443", "allowed name as a prefix"),
            ("evil.com:443", "plainly not allowed"),
            ("crates.io@evil.com:443", "userinfo style"),
            ("\u{441}rates.io:443", "cyrillic homograph"),
            (".crates.io:443", "empty leading label"),
        ] {
            let response = status(format!("CONNECT {target} HTTP/1.1")).await;
            // 403 (policy refused) and 400 (malformed, never reached policy)
            // are both refusals. 200 and 502 are not: both mean the request
            // got as far as dialling, so the policy had already said yes.
            assert!(
                response.contains("403") || response.contains("400"),
                "CONNECT {target:?} ({why}) got {response:?}.\n\
                 A 502 here means the policy said yes and only the dial failed, \
                 which is not the same as being refused."
            );
        }

        // Control: the allowed host must reach the policy's yes-path, or the
        // loop above would pass against a proxy that refuses everything.
        let response = status("CONNECT crates.io:443 HTTP/1.1".to_string()).await;
        assert!(
            !response.contains("403"),
            "the allowed host was refused, so the 403s above prove nothing: {response:?}"
        );
    }

    #[test]
    fn host_and_port_are_split_strictly() {
        assert_eq!(
            split_host_port("crates.io:443"),
            Some(("crates.io".to_string(), 443))
        );
        assert_eq!(
            split_host_port("[::1]:443"),
            Some(("::1".to_string(), 443)),
            "a real IPv6 literal must still work"
        );

        for malformed in [
            "[crates.io]:443", // brackets are the IPv6 form, not decoration
            "crates.io",       // no port
            ":443",            // no host
            "::1:443",         // bare IPv6, ambiguous with host:port
            "crates.io:https", // non-numeric port
            "crates.io:99999", // out of range
            "[::1]443",        // missing the port separator
        ] {
            assert_eq!(
                split_host_port(malformed),
                None,
                "{malformed:?} must not parse; anything that does is dialled"
            );
        }
    }

    #[test]
    fn a_github_api_host_suggests_the_apex_family() {
        assert_eq!(suggest_host_pattern("api.github.com"), "**.github.com");
        assert_eq!(suggest_host_pattern("github.com"), "**.github.com");
        assert_eq!(suggest_host_pattern("uploads.github.com"), "**.github.com");
        assert_eq!(host_allow_rule("api.github.com"), "host(**.github.com)");
        assert_eq!(suggest_host_pattern("127.0.0.1"), "127.0.0.1");
        assert_eq!(suggest_host_pattern("localhost"), "localhost");
    }

    #[test]
    fn denied_host_is_pulled_from_gh_and_git_errors() {
        assert_eq!(
            extract_denied_host(
                "failed to authenticate: Post \"https://api.github.com/graphql\": Forge Sandbox Denied"
            )
            .as_deref(),
            Some("api.github.com")
        );
        assert_eq!(
            extract_denied_host(
                "fatal: unable to access 'https://github.com/x/y': Could not resolve host: github.com"
            )
            .as_deref(),
            Some("github.com")
        );
        assert_eq!(
            extract_denied_host("Forge Sandbox Denied: host crates.io is not allowed\n").as_deref(),
            Some("crates.io")
        );
    }

    #[test]
    fn a_proxy_refusal_is_a_sandbox_denial_even_when_the_client_prints_something_else() {
        let dir = tempfile::tempdir().unwrap();
        let shared = EgressShared::new(EgressPolicy::new());
        shared.record_denied("example.com".into());
        let grant = crate::sandbox::EgressGrant {
            proxy_port: 1,
            socket_path: dir.path().join("e.sock"),
            control: Some(shared),
        };
        let error = denial_for_failed_confined_command(
            "HTTP 403: unexpected status from the remote API",
            dir.path(),
            Some(&grant),
        )
        .expect("a proxy refusal must be a sandbox denial");
        let crate::ToolError::SandboxDenied {
            denied_host,
            reason,
            ..
        } = error
        else {
            panic!("expected SandboxDenied");
        };
        assert_eq!(denied_host.as_deref(), Some("example.com"));
        assert!(reason.contains("host(...)"), "{reason}");
    }

    #[test]
    fn a_failed_command_is_not_a_sandbox_denial_without_a_proxy_refusal() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            denial_for_failed_confined_command(
                "HTTP 403: unexpected status from the remote API\nhttps://example.com/v1",
                dir.path(),
                None,
            )
            .is_none(),
            "a URL in a real HTTP 403 must not invent a host grant"
        );
    }

    #[tokio::test]
    async fn granting_a_host_unblocks_the_next_connect() {
        let proxy = EgressProxy::start(EgressPolicy::new()).await.unwrap();
        let status = {
            use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
            let mut stream = TcpStream::connect(("127.0.0.1", proxy.addr().port()))
                .await
                .unwrap();
            stream
                .write_all(b"CONNECT api.github.com:443 HTTP/1.1\r\n\r\n")
                .await
                .unwrap();
            let mut line = String::new();
            BufReader::new(stream).read_line(&mut line).await.unwrap();
            line
        };
        assert!(status.contains("403"), "{status}");
        assert_eq!(
            proxy.shared().take_denied_host().as_deref(),
            Some("api.github.com")
        );

        proxy.shared().grant_host("**.github.com");
        let status = {
            use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
            let mut stream = TcpStream::connect(("127.0.0.1", proxy.addr().port()))
                .await
                .unwrap();
            stream
                .write_all(b"CONNECT api.github.com:443 HTTP/1.1\r\n\r\n")
                .await
                .unwrap();
            let mut line = String::new();
            BufReader::new(stream).read_line(&mut line).await.unwrap();
            line
        };
        assert!(
            !status.contains("403"),
            "a granted family must not 403, got {status:?}"
        );
    }
}
