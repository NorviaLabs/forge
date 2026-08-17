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
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

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

impl EgressPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    /// The ecosystems a first run actually needs.
    ///
    /// Without these, `cargo build` on uncached dependencies fails inside the
    /// sandbox — the single most common first-run action in a Rust project,
    /// failing in a way that looks like a broken build rather than a policy
    /// decision.
    pub fn with_default_ecosystems() -> Self {
        let mut policy = Self::new();
        for host in [
            "crates.io",
            "static.crates.io",
            "index.crates.io",
            "**.crates.io",
            "github.com",
            "**.githubusercontent.com",
            "registry.npmjs.org",
            "pypi.org",
            "files.pythonhosted.org",
        ] {
            policy.allow(host);
        }
        policy
    }

    pub fn allow(&mut self, pattern: impl Into<String>) -> &mut Self {
        self.allow.push(pattern.into().to_ascii_lowercase());
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
        if host.is_empty() {
            return false;
        }
        if self.deny.iter().any(|p| matches_pattern(p, &host)) {
            return false;
        }
        self.allow.iter().any(|p| matches_pattern(p, &host))
    }
}

fn matches_pattern(pattern: &str, host: &str) -> bool {
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

/// A running CONNECT proxy.
///
/// Dropping it stops accepting new connections.
pub struct EgressProxy {
    addr: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for EgressProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl EgressProxy {
    /// Bind to an ephemeral port on loopback and start serving.
    pub async fn start(policy: EgressPolicy) -> io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let addr = listener.local_addr()?;
        let policy = Arc::new(policy);

        let task = tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                let policy = Arc::clone(&policy);
                tokio::spawn(async move {
                    let _ = serve(client, policy).await;
                });
            }
        });

        Ok(Self { addr, task })
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

async fn serve(client: TcpStream, policy: Arc<EgressPolicy>) -> io::Result<()> {
    let mut reader = BufReader::new(client);
    let mut request = String::new();
    reader.read_line(&mut request).await?;

    let Some(target) = parse_connect(&request) else {
        return respond(reader.into_inner(), 400, "Bad Request").await;
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

    let host = target.rsplit_once(':').map(|(h, _)| h).unwrap_or(&target);
    let host = host.trim_start_matches('[').trim_end_matches(']');

    if !policy.permits(host) {
        return respond(reader.into_inner(), 403, "Forbidden").await;
    }

    let upstream = match TcpStream::connect(&target).await {
        Ok(stream) => stream,
        Err(_) => return respond(reader.into_inner(), 502, "Bad Gateway").await,
    };

    let mut client = reader.into_inner();
    client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await?;

    // From here the proxy copies bytes and never sees the plaintext.
    let (mut cr, mut cw) = client.into_split();
    let (mut ur, mut uw) = upstream.into_split();
    let c2u = tokio::spawn(async move { tokio::io::copy(&mut cr, &mut uw).await });
    let u2c = tokio::spawn(async move { tokio::io::copy(&mut ur, &mut cw).await });
    let _ = tokio::join!(c2u, u2c);
    Ok(())
}

fn parse_connect(line: &str) -> Option<String> {
    let mut parts = line.split_whitespace();
    if !parts.next()?.eq_ignore_ascii_case("CONNECT") {
        return None;
    }
    let target = parts.next()?;
    (!target.is_empty()).then(|| target.to_string())
}

async fn respond(mut client: TcpStream, code: u16, reason: &str) -> io::Result<()> {
    let body =
        format!("HTTP/1.1 {code} {reason}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    client.write_all(body.as_bytes()).await?;
    client.shutdown().await
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn the_default_seed_covers_a_first_cargo_build() {
        let p = EgressPolicy::with_default_ecosystems();
        for host in ["crates.io", "static.crates.io", "index.crates.io"] {
            assert!(p.permits(host), "{host} is needed by a first cargo build");
        }
        assert!(!p.permits("evil.example.com"));
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
}
