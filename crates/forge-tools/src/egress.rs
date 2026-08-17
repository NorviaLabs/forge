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

impl EgressPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    /// An **opt-in** convenience covering the ecosystems a first run needs.
    ///
    /// Deliberately not the default. Claude Code pre-allows nothing and
    /// prompts on first use of each domain; Codex's production stance is
    /// `"*" = "deny"` plus what you add. A seeded allowlist is a policy
    /// decision made silently on the user's behalf — the same class of mistake
    /// as a sandbox that quietly grants the whole temp tree. Callers that want
    /// `cargo build` to run without a prompt should opt in explicitly.
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
    socket_path: Option<PathBuf>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
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
        let policy = Arc::new(policy);

        let task = tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                let policy = Arc::clone(&policy);
                tokio::spawn(async move {
                    let _ = serve(client, policy).await;
                });
            }
        });

        Ok(Self {
            addr,
            socket_path: None,
            tasks: vec![task],
        })
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
        policy: EgressPolicy,
    ) -> io::Result<()> {
        let path = path.as_ref().to_path_buf();
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        let policy = Arc::new(policy);

        self.tasks.push(tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                let policy = Arc::clone(&policy);
                tokio::spawn(async move {
                    let _ = serve_unix(client, policy).await;
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
async fn serve<S>(client: S, policy: Arc<EgressPolicy>) -> io::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + 'static,
{
    let (read_half, mut write_half) = tokio::io::split(client);
    let mut reader = BufReader::new(read_half);

    let mut request = String::new();
    reader.read_line(&mut request).await?;

    let Some(target) = parse_connect(&request) else {
        return respond(&mut write_half, 400, "Bad Request").await;
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
        return respond(&mut write_half, 400, "Bad Request").await;
    };

    if !policy.permits(&host) {
        return respond(&mut write_half, 403, "Forbidden").await;
    }

    let upstream = match TcpStream::connect((host.as_str(), port)).await {
        Ok(stream) => stream,
        Err(_) => return respond(&mut write_half, 502, "Bad Gateway").await,
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

async fn serve_unix(client: UnixStream, policy: Arc<EgressPolicy>) -> io::Result<()> {
    serve(client, policy).await
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

async fn respond<W>(client: &mut W, code: u16, reason: &str) -> io::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
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
}
