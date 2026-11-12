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

use std::collections::VecDeque;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use std::path::{Path, PathBuf};

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
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
    /// Write targets (`host/owner/repo`, or `host`) a confined process may
    /// change under the user's identity without being asked.
    writes: Vec<String>,
}

/// Code hosts allowed by default, with every subdomain. They accept
/// authenticated writes, so the gateway inspects requests to them (see
/// [`inspected`]) instead of tunnelling blindly.
pub const BUILTIN_CODE_HOSTS: &[&str] = &["github.com", "gitlab.com", "bitbucket.org"];

/// Package registries and download hosts allowed by default: what a build
/// fetches its dependencies from. Tunnelled, not inspected.
pub const BUILTIN_REGISTRY_HOSTS: &[&str] = &[
    "**.githubusercontent.com",
    "registry.npmjs.org",
    "registry.yarnpkg.com",
    "nodejs.org",
    "pypi.org",
    "files.pythonhosted.org",
    "crates.io",
    "static.crates.io",
    "index.crates.io",
    "static.rust-lang.org",
    "proxy.golang.org",
    "sum.golang.org",
    "rubygems.org",
    "index.rubygems.org",
    "repo.maven.apache.org",
    "repo1.maven.org",
    "plugins.gradle.org",
    "services.gradle.org",
];

/// Whether `host` is a built-in code host or one of its subdomains: a host
/// whose requests the gateway reads, so that only the user's own identity
/// is used there.
pub fn inspected(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    BUILTIN_CODE_HOSTS.iter().any(|code_host| {
        host == *code_host
            || host
                .strip_suffix(code_host)
                .is_some_and(|prefix| prefix.ends_with('.'))
    })
}

/// Whether `host` is allowed only as a built-in registry: nothing Forge
/// looks a credential up for.
pub fn is_builtin_registry(host: &str) -> bool {
    BUILTIN_REGISTRY_HOSTS
        .iter()
        .any(|pattern| pattern.trim_start_matches("**.") == host)
}

pub const SANDBOX_DENIED_REASON: &str = "Forge Sandbox Denied";

/// Reason phrase when a permitted name resolves to a non-public address.
///
/// Distinct from [`SANDBOX_DENIED_REASON`] (which it extends, so output-based
/// detection still classifies it as a sandbox refusal) because a host grant
/// cannot fix it: the name was allowed, the address it resolved to was not.
pub const PRIVATE_ADDRESS_DENIED_REASON: &str = "Forge Sandbox Denied: private address";

/// How long the gateway waits for name resolution plus the upstream dial.
pub const EGRESS_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// A tunnel that carries no bytes in either direction for this long is closed
/// on both sides, so an abandoned connection cannot pin a socket forever.
pub const EGRESS_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Connection records kept in memory before the oldest are dropped.
pub const EGRESS_EVENT_CAPACITY: usize = 1024;

/// Gateway behaviour that is not part of the host policy.
#[derive(Debug, Clone, Copy)]
pub struct EgressOptions {
    pub connect_timeout: Duration,
    pub idle_timeout: Duration,
    /// Permit loopback/private/link-local destinations even when the allow
    /// pattern does not name the host exactly. Off in production; exists for
    /// hermetic tests whose upstream is a loopback listener behind a name.
    pub allow_private_destinations: bool,
    /// How long a connection to a host nobody has decided on is held while
    /// the user is asked. Past it the connection is refused as before.
    pub hold_timeout: Duration,
}

impl Default for EgressOptions {
    fn default() -> Self {
        Self {
            connect_timeout: EGRESS_CONNECT_TIMEOUT,
            idle_timeout: EGRESS_IDLE_TIMEOUT,
            allow_private_destinations: false,
            hold_timeout: EGRESS_HOLD_TIMEOUT,
        }
    }
}

/// How long a held connection waits for the user. Long enough to read a
/// card and click; clients that give up sooner (cargo after 30 seconds, pip
/// after 15) retry, and a host granted in the meantime is then let through.
pub const EGRESS_HOLD_TIMEOUT: Duration = Duration::from_secs(120);

/// A connection to a host the policy has no rule for, held while someone
/// decides. Whoever receives it answers on `reply`; dropping `reply`
/// refuses.
#[derive(Debug)]
pub struct HostAsk {
    pub host: String,
    pub port: u16,
    /// The tool call (or MCP server) the connection belongs to.
    pub label: Option<String>,
    /// Set when this is not about reaching a host but about changing
    /// something on it under the user's identity: what is being done.
    /// `host` is then the write target (`host/owner/repo`, or the host).
    pub write: Option<String>,
    pub reply: tokio::sync::oneshot::Sender<HostAnswer>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostAnswer {
    /// Let this command reach the host; decide nothing for the next one.
    AllowOnce,
    /// The host was added to the policy; let the connection through.
    Allow,
    Deny,
}

/// Where held connections are sent to be decided.
pub type HostAsker = tokio::sync::mpsc::UnboundedSender<HostAsk>;

/// What the gateway did with one CONNECT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EgressOutcome {
    /// Tunnel established; the event closes when the tunnel does.
    Allowed,
    /// Refused before any upstream socket was opened.
    Denied(EgressDenial),
    /// Permitted, but resolution or the dial failed or timed out.
    UpstreamFailed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressDenial {
    /// The host policy does not permit the name.
    Policy,
    /// The name is permitted but resolved to a non-public address.
    PrivateAddress,
}

/// One CONNECT decision and, when tunnelled, its traffic totals.
#[derive(Debug, Clone)]
pub struct EgressEvent {
    /// Attribution supplied by the invocation (the tool call id), if any.
    pub label: Option<String>,
    pub host: String,
    pub port: u16,
    pub outcome: EgressOutcome,
    /// The address actually dialled — the one that passed the address check.
    pub upstream: Option<SocketAddr>,
    /// Client to upstream.
    pub bytes_up: u64,
    /// Upstream to client.
    pub bytes_down: u64,
    pub duration: Duration,
}

impl EgressEvent {
    pub fn allowed(&self) -> bool {
        self.outcome == EgressOutcome::Allowed
    }
}

#[derive(Debug, Default)]
struct EgressEventLog {
    events: VecDeque<EgressEvent>,
    dropped: u64,
}

/// Explanation attached when the proxy refused a host, even if the client
/// printed something else (HTTP 403, "token invalid", "Forbidden", …).
pub const HOST_DENIED_EXPLANATION: &str =
    "blocked by the sandbox: the destination host is not allowed by \
     the personal host(...) network permissions.";

/// Decide whether a confined process encountered a sandbox denial.
///
/// Successful shell commands inspect only shell-generated stderr denials, so
/// ordinary output and non-fatal child-process warnings cannot create an
/// approval prompt. The invocation proxy is the source of truth for network.
/// A host is never inferred from a URL alone — that would turn a real 403 from
/// an already-allowed host into a grant prompt.
pub fn denial_for_confined_command(
    command: &str,
    content: &str,
    stderr: &str,
    command_succeeded: bool,
    shell: &str,
    workspace_root: &std::path::Path,
    denied_host: Option<String>,
) -> Option<crate::ToolError> {
    let explanation = if command_succeeded {
        // PTY sessions merge stderr into `content`; pipe sessions retain a
        // separate stderr buffer. Inspect both so a shell redirection cannot
        // hide a sandbox denial merely because the command exits zero.
        explain_shell_denial(content, shell, workspace_root)
            .or_else(|| explain_shell_denial(stderr, shell, workspace_root))
            .or_else(|| explain_requested_child_denial(command, content, workspace_root))
            .or_else(|| explain_requested_child_denial(command, stderr, workspace_root))
    } else {
        // Some runtimes (notably Python and Node) report permission errors on
        // stderr while stdout remains empty. The sandbox boundary is the
        // same regardless of which stream carried the diagnostic.
        crate::sandbox::explain_denial(content, workspace_root)
            .or_else(|| crate::sandbox::explain_denial(stderr, workspace_root))
    };
    let (reason, denied_host) = if let Some(explanation) = explanation {
        (explanation, denied_host)
    } else {
        let host = denied_host?;
        (HOST_DENIED_EXPLANATION.to_string(), Some(host))
    };
    let mut reported = content.to_string();
    if !reported.contains(&reason) {
        if !reported.is_empty() {
            reported.push('\n');
        }
        reported.push_str(&reason);
    }
    Some(crate::ToolError::SandboxDenied {
        content: reported,
        reason,
        denied_host,
    })
}

/// A nested runtime can report a denied operation without the outer shell's
/// prefix (for example Python's `PermissionError`). On a successful aggregate
/// shell status, correlate only an absolute resource that the requested
/// command names explicitly. This catches `... || true` without turning an
/// implicit compiler warning, such as macOS's xcrun cache warning, into HITL.
fn explain_requested_child_denial(
    command: &str,
    output: &str,
    workspace_root: &std::path::Path,
) -> Option<String> {
    output.lines().find_map(|line| {
        let resource = crate::sandbox::denied_resource(line)?;
        command
            .contains(&resource)
            .then(|| crate::sandbox::explain_denial(line, workspace_root))
            .flatten()
    })
}

/// A zero aggregate shell status is ambiguous: a pipeline can hide a failed
/// redirection, but successful tools also emit non-fatal sandbox warnings (for
/// example, macOS `git` may fail to write an `xcrun` cache and continue).
/// Restrict output-based detection to diagnostics emitted by the shell itself;
/// proxy refusals do not rely on this heuristic.
fn explain_shell_denial(
    stderr: &str,
    expected_shell: &str,
    workspace_root: &std::path::Path,
) -> Option<String> {
    let expected_shell = std::path::Path::new(expected_shell)
        .file_name()
        .and_then(|name| name.to_str())?;
    stderr.lines().find_map(|line| {
        let prefix = line.split_once(':')?.0;
        let shell = std::path::Path::new(prefix)
            .file_name()
            .and_then(|name| name.to_str())?;
        (shell == expected_shell)
            .then(|| crate::sandbox::explain_denial(line, workspace_root))
            .flatten()
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
        for target in file.allow.iter().filter_map(|raw| write_rule(raw)) {
            policy.allow_write(target);
        }
        policy
    }

    /// Add the built-in allow-list: the code hosts and registries a build
    /// needs, so ordinary work does not start with a question. A deny rule
    /// still wins over it.
    pub fn with_builtin_hosts(mut self) -> Self {
        for host in BUILTIN_CODE_HOSTS {
            self.allow(format!("**.{host}"));
        }
        for pattern in BUILTIN_REGISTRY_HOSTS {
            self.allow(*pattern);
        }
        self
    }

    /// Let writes to `target` through without asking.
    pub fn allow_write(&mut self, target: impl Into<String>) -> &mut Self {
        let target = target.into().trim().trim_matches('/').to_ascii_lowercase();
        if !target.is_empty() && !self.writes.contains(&target) {
            self.writes.push(target);
        }
        self
    }

    /// Whether a write to `target` needs no question: the target itself is
    /// allowed, or its whole host is.
    pub fn permits_write(&self, target: &str) -> bool {
        let target = target.to_ascii_lowercase();
        let host = target.split('/').next().unwrap_or(&target);
        self.writes
            .iter()
            .any(|allowed| *allowed == target || allowed == host)
    }

    pub fn is_empty(&self) -> bool {
        self.allow.is_empty() && self.deny.is_empty()
    }

    /// Patterns currently allowed, as stored (`**.example.com`, `host`, `*`).
    pub fn allow_patterns(&self) -> &[String] {
        &self.allow
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

    /// Whether `host` is a well-formed name no rule has decided: not
    /// permitted, and not denied either. Only such a host is worth asking
    /// the user about.
    pub fn undecided(&self, host: &str) -> bool {
        let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
        if host.is_empty() || host.starts_with('.') || host.contains("..") {
            return false;
        }
        !self.deny.iter().any(|p| matches_pattern(p, &host))
            && !self.allow.iter().any(|p| matches_pattern(p, &host))
    }
}

fn write_rule(raw: &str) -> Option<&str> {
    let raw = raw.trim();
    let inner = raw.strip_prefix("remote_write(")?.strip_suffix(')')?.trim();
    (!inner.is_empty()).then_some(inner)
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
    /// Hosts refused because they resolved to a non-public address. Kept
    /// apart from `denied` because a host grant cannot fix these, so they
    /// must never become a grant prompt.
    private_denied: Arc<Mutex<Vec<String>>>,
    /// Session-wide connection log, shared by every invocation.
    events: Arc<Mutex<EgressEventLog>>,
    options: EgressOptions,
    label: Option<Arc<str>>,
    /// Attribution that overrides `label` while it is set: a long-lived
    /// endpoint (an MCP server's) names the tool call in flight, so its
    /// connections correlate with that call rather than only the server.
    call_label: Arc<Mutex<Option<Arc<str>>>>,
    /// Who decides a host the policy has no rule for. Shared by every
    /// endpoint on this state; `None` (headless runs, tests) refuses at once.
    asker: Arc<RwLock<Option<HostAsker>>>,
    /// Hosts let through for this invocation only (`HostAnswer::AllowOnce`).
    once: Arc<Mutex<Vec<String>>>,
    /// Hosts the user refused when asked, for this invocation. Kept apart
    /// from `denied`: that queue is what raises a host approval, and a host
    /// the user has just said no to must not be asked about again.
    refused: Arc<Mutex<Vec<String>>>,
    /// The session's credentials and certificate authority, shared by every
    /// endpoint.
    intercept: Arc<crate::tls_intercept::Intercept>,
    /// Whether this endpoint's client was given the session authority's
    /// certificate. Only then is a tunnel to a credentialed host terminated:
    /// a client that does not trust the authority would just fail.
    intercepting: Arc<std::sync::atomic::AtomicBool>,
    /// Credentialed hosts whose client would not accept the session
    /// authority's certificate, for this invocation.
    untrusting: Arc<Mutex<Vec<String>>>,
    /// Write targets let through for this invocation only.
    once_writes: Arc<Mutex<Vec<String>>>,
}

/// What [`EgressShared::admit`] decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    Allowed,
    /// No rule permits the host and nobody allowed it in time.
    Refused,
    /// The user was asked and said no.
    RefusedByUser,
}

impl Admission {
    pub fn allowed(self) -> bool {
        self == Self::Allowed
    }
}

/// Appended to a command's output when a program in it would not accept the
/// sandbox certificate for a host Forge supplies credentials to.
pub fn untrusting_note(hosts: &[String]) -> String {
    format!(
        "A program in this command could not reach {} because it does not accept the sandbox's \
         certificate. Forge keeps your credentials for that host outside the sandbox and supplies \
         them at its proxy, which only programs that honour a custom certificate bundle can use \
         (git, curl, gh, Node, Python). Use one of those, or request to run the command \
         unconfined.",
        hosts.join(", ")
    )
}

/// Appended to a command's output when the user refused a host it tried to
/// reach, so the model reads a decision, not a network fault to retry.
pub fn user_refusal_note(hosts: &[String]) -> String {
    format!(
        "Network access to {} was refused by the user. Do not retry it; continue without it or ask.",
        hosts.join(", ")
    )
}

impl EgressShared {
    /// Fresh live state: `policy`, an empty refusal queue and an empty event
    /// log. A caller that needs several listeners (the session proxy and the
    /// MCP servers' endpoints) on one state builds it once and starts each
    /// listener from it, so a grant reaches all of them at once.
    pub fn new(policy: EgressPolicy, options: EgressOptions) -> Self {
        Self {
            policy: Arc::new(RwLock::new(policy)),
            denied: Arc::new(Mutex::new(Vec::new())),
            private_denied: Arc::new(Mutex::new(Vec::new())),
            events: Arc::new(Mutex::new(EgressEventLog::default())),
            options,
            label: None,
            call_label: Arc::new(Mutex::new(None)),
            asker: Arc::new(RwLock::new(None)),
            once: Arc::new(Mutex::new(Vec::new())),
            refused: Arc::new(Mutex::new(Vec::new())),
            intercept: Arc::default(),
            intercepting: Arc::default(),
            untrusting: Arc::default(),
            once_writes: Arc::default(),
        }
    }

    /// Have this endpoint supply `password` for `host` (and its subdomains)
    /// itself, and return the placeholder its client should hold instead.
    /// The caller must give that client the certificate from
    /// [`Self::session_ca_pem`]: from now on this endpoint terminates TLS to
    /// credentialed hosts. `None` when no authority could be created; the
    /// caller then has to hand over the real credential, as before.
    pub fn supply_credential(&self, host: &str, password: &str) -> Option<String> {
        self.intercept.ca_pem()?;
        let placeholder = self.intercept.register(host, password);
        self.intercepting.store(true, Ordering::Relaxed);
        Some(placeholder)
    }

    /// Take the credentialed hosts a client of this endpoint could not
    /// reach because it does not accept the session authority.
    pub fn take_untrusting_hosts(&self) -> Vec<String> {
        std::mem::take(
            &mut *self
                .untrusting
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    /// The session authority's certificate, PEM, for a client of this
    /// endpoint that will be told to trust it. From now on this endpoint
    /// terminates TLS to credentialed and inspected hosts.
    pub fn enable_interception(&self) -> Option<String> {
        let pem = self.intercept.ca_pem()?;
        self.intercepting.store(true, Ordering::Relaxed);
        Some(pem)
    }

    /// The session authority's certificate, PEM.
    pub fn session_ca_pem(&self) -> Option<String> {
        self.intercept.ca_pem()
    }

    /// Internal, for tests only: also trust this DER certificate when the
    /// gateway verifies a credentialed host.
    #[doc(hidden)]
    pub fn trust_upstream_root(&self, der: Vec<u8>) {
        self.intercept
            .trust_upstream_root(rustls::pki_types::CertificateDer::from(der));
    }

    /// Send hosts the policy has no rule for to `asker` instead of refusing
    /// them outright; `None` restores the immediate refusal. Applies to
    /// every endpoint sharing this state, including ones already running.
    pub fn set_asker(&self, asker: Option<HostAsker>) {
        *self
            .asker
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = asker;
    }

    /// Whether a connection to `host` would be held and put to the user
    /// (no rule decides it and someone is there to ask). A caller that
    /// checks the policy before connecting lets such a host through to the
    /// gateway, which is where the question is asked.
    pub fn asks_about(&self, host: &str) -> bool {
        self.asker
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some()
            && self
                .policy
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .undecided(host)
    }

    /// Whether `host` may be reached, asking when no rule decides it.
    ///
    /// A permitted host passes and a denied one is refused, both at once.
    /// Anything else is put to the asker, and the caller waits: the
    /// connection it is about to make is held rather than failed, so the
    /// command that opened it carries on when the user allows the host and
    /// nothing has to be run twice. With no asker, or no answer within
    /// `hold_timeout`, the host is refused as before.
    pub async fn admit(&self, host: &str, port: u16) -> bool {
        self.admission(host, port).await.allowed()
    }

    /// Hosts the user refused through this endpoint since the last take.
    pub fn take_user_refused_hosts(&self) -> Vec<String> {
        std::mem::take(
            &mut *self
                .refused
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    /// As [`Self::admit`], telling a refusal by the user from the rest.
    pub async fn admission(&self, host: &str, port: u16) -> Admission {
        if self.permits(host) {
            return Admission::Allowed;
        }
        let undecided = self
            .policy
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .undecided(host);
        let asker = self
            .asker
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let (true, Some(asker)) = (undecided, asker) else {
            return Admission::Refused;
        };
        let (reply, answer) = tokio::sync::oneshot::channel();
        let ask = HostAsk {
            host: host.trim().trim_end_matches('.').to_ascii_lowercase(),
            port,
            label: self.attribution(),
            write: None,
            reply,
        };
        if asker.send(ask).is_err() {
            return Admission::Refused;
        }
        let name = host.trim().trim_end_matches('.').to_ascii_lowercase();
        match tokio::time::timeout(self.options.hold_timeout, answer).await {
            Ok(Ok(HostAnswer::Allow)) if self.permits(host) => Admission::Allowed,
            Ok(Ok(HostAnswer::AllowOnce)) => {
                self.once
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(name);
                Admission::Allowed
            }
            Ok(Ok(HostAnswer::Deny)) => {
                self.refused
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(name);
                Admission::RefusedByUser
            }
            _ => Admission::Refused,
        }
    }

    /// Share live policy updates while keeping refusal attribution local to
    /// one command. Session-global refusal storage lets a swallowed denial
    /// poison whichever unrelated command happens to fail next. The event log
    /// stays shared: it is attributed by label, not by queue.
    fn for_invocation(&self, label: Option<&str>) -> Self {
        Self {
            policy: Arc::clone(&self.policy),
            denied: Arc::new(Mutex::new(Vec::new())),
            private_denied: Arc::new(Mutex::new(Vec::new())),
            events: Arc::clone(&self.events),
            options: self.options,
            label: label.map(Arc::from),
            call_label: Arc::new(Mutex::new(None)),
            asker: Arc::clone(&self.asker),
            once: Arc::new(Mutex::new(Vec::new())),
            refused: Arc::new(Mutex::new(Vec::new())),
            intercept: Arc::clone(&self.intercept),
            intercepting: Arc::default(),
            untrusting: Arc::default(),
            once_writes: Arc::default(),
        }
    }

    /// The attribution label connections through this endpoint carry when
    /// no call label is set.
    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    /// Attribute connections opened from now on to `label` (a tool call id)
    /// instead of [`Self::label`]; `None` restores the endpoint's own label.
    pub fn set_call_label(&self, label: Option<&str>) {
        *self
            .call_label
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = label.map(Arc::from);
    }

    /// The label a connection opened now is recorded under.
    fn attribution(&self) -> Option<String> {
        self.call_label
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_deref()
            .or(self.label.as_deref())
            .map(str::to_string)
    }

    /// Take every recorded connection event, oldest first.
    pub fn drain_events(&self) -> Vec<EgressEvent> {
        let mut log = self
            .events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        log.events.drain(..).collect()
    }

    fn record_event(&self, event: EgressEvent) {
        match &event.outcome {
            EgressOutcome::Allowed => tracing::info!(
                target: "forge::egress",
                label = event.label.as_deref().unwrap_or(""),
                host = %event.host,
                port = event.port,
                upstream = ?event.upstream,
                bytes_up = event.bytes_up,
                bytes_down = event.bytes_down,
                duration_ms = event.duration.as_millis() as u64,
                "egress tunnel closed"
            ),
            outcome => tracing::info!(
                target: "forge::egress",
                label = event.label.as_deref().unwrap_or(""),
                host = %event.host,
                port = event.port,
                outcome = ?outcome,
                duration_ms = event.duration.as_millis() as u64,
                "egress connect refused"
            ),
        }
        let mut log = self
            .events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if log.events.len() >= EGRESS_EVENT_CAPACITY {
            log.events.pop_front();
            log.dropped += 1;
        }
        log.events.push_back(event);
    }

    /// Whether a non-public destination is acceptable for `host`.
    ///
    /// Only when an allow pattern names the host exactly (an IP literal,
    /// `localhost`, or a hand-written exact name). A wildcard or `*` grant is
    /// about a family of public names; letting it reach loopback would turn
    /// any name that resolves to 127.0.0.1 or a metadata address into a way
    /// onto the host's own services.
    fn private_destination_permitted(&self, host: &str) -> bool {
        if self.options.allow_private_destinations {
            return true;
        }
        let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
        self.policy
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .allow_patterns()
            .iter()
            .any(|pattern| pattern == &host)
    }

    /// Replace the host policy in place. Every listener on this state (and
    /// every invocation endpoint derived from it) enforces the new policy on
    /// its next CONNECT; grants made with [`Self::grant_host`] are discarded
    /// with the old policy. The event log is kept.
    pub fn replace_policy(&self, policy: EgressPolicy) {
        *self
            .policy
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = policy;
    }

    /// Let writes to `target` through without asking, for the rest of this
    /// proxy's life.
    pub fn grant_write(&self, target: &str) {
        self.policy
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .allow_write(target);
    }

    /// Whether a write to `target` goes through without a question.
    pub fn permits_write(&self, target: &str) -> bool {
        self.policy
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .permits_write(target)
    }

    /// Decide a write under the user's identity: allowed when its target is
    /// recorded, otherwise held while the user is asked, like an undecided
    /// host. With nobody to ask it is refused.
    pub async fn admit_write(&self, write: &crate::write_gate::Write) -> Admission {
        let target = write.target.to_ascii_lowercase();
        let allowed_once = || {
            self.once_writes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .contains(&target)
        };
        if self.permits_write(&target) || allowed_once() {
            return Admission::Allowed;
        }
        let asker = self
            .asker
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let Some(asker) = asker else {
            return Admission::Refused;
        };
        let (reply, answer) = tokio::sync::oneshot::channel();
        let ask = HostAsk {
            host: target.clone(),
            port: 443,
            label: self.attribution(),
            write: Some(write.summary.clone()),
            reply,
        };
        if asker.send(ask).is_err() {
            return Admission::Refused;
        }
        match tokio::time::timeout(self.options.hold_timeout, answer).await {
            Ok(Ok(HostAnswer::Allow)) if self.permits_write(&target) => Admission::Allowed,
            Ok(Ok(HostAnswer::AllowOnce)) => {
                self.once_writes
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(target);
                Admission::Allowed
            }
            Ok(Ok(HostAnswer::Deny)) => {
                self.refused
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(format!("{target} (to change it as you)"));
                Admission::RefusedByUser
            }
            _ => Admission::Refused,
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

    /// Every host refused since the last take, oldest first, left queued.
    pub fn denied_hosts(&self) -> Vec<String> {
        self.denied
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn permits_host(&self, host: &str) -> bool {
        self.permits(host)
    }

    pub fn allow_patterns(&self) -> Vec<String> {
        self.policy
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .allow_patterns()
            .to_vec()
    }

    /// The first host refused for resolving to a non-public address since
    /// the last take, clearing the queue.
    pub fn take_private_address_denial(&self) -> Option<String> {
        let mut denied = self
            .private_denied
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let host = denied.first().cloned();
        denied.clear();
        host
    }

    fn record_private_denied(&self, host: String) {
        self.private_denied
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(host);
    }

    pub fn record_denied(&self, host: String) {
        self.denied
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(host);
    }

    fn permits(&self, host: &str) -> bool {
        let policy = self
            .policy
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if policy.permits(host) {
            return true;
        }
        // Allowed for this invocation only, unless a deny rule arrived since.
        let name = host.trim().trim_end_matches('.').to_ascii_lowercase();
        policy.undecided(host)
            && self
                .once
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .contains(&name)
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
        Self::start_with_options(policy, EgressOptions::default()).await
    }

    /// As [`Self::start`], with non-default timeouts or address rules.
    pub async fn start_with_options(
        policy: EgressPolicy,
        options: EgressOptions,
    ) -> io::Result<Self> {
        Self::start_with_shared(EgressShared::new(policy, options)).await
    }

    /// Serve `shared` — an existing live state — on a new loopback listener.
    /// Grants, refusals and events are those of `shared`, not a copy.
    pub async fn start_with_shared(shared: EgressShared) -> io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let addr = listener.local_addr()?;

        let accept = shared.clone();
        let task = tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                let shared = accept.clone();
                tokio::spawn(async move {
                    let _ = serve(client, shared, false).await;
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
        self.serve_on_unix_socket_inner(path).await
    }

    async fn serve_on_unix_socket_inner(&mut self, path: impl AsRef<Path>) -> io::Result<()> {
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

    /// The loopback TCP listener (`127.0.0.1`, ephemeral port).
    ///
    /// Same listener as [`Self::addr`], named for in-process and host-side
    /// callers that route through the gateway rather than the sandbox's
    /// proxy port.
    pub fn tcp_addr(&self) -> SocketAddr {
        self.addr
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

static NEXT_INVOCATION_SOCKET: AtomicU64 = AtomicU64::new(1);

/// A proxy endpoint and refusal queue owned by one confined shell invocation.
///
/// The session policy remains shared, so a session-only HITL grant applies to
/// the retry. The listener and denied-host queue do not: concurrent commands
/// cannot steal each other's refusal, and a command that masks its own exit
/// status cannot leave a host behind for the next failure.
///
/// Two modes share the same listener and queue. A confined shell gets a
/// sandbox mode endpoint ([`Self::start_labeled`]): loopback TCP plus a Unix
/// socket, described by a [`crate::sandbox::EgressGrant`] the sandbox policy
/// consumes. An in-process or host-side tool gets TCP mode
/// ([`Self::start_tcp_labeled`]): loopback TCP only, with no socket file and no
/// grant, because nothing confines the caller. Both stop listening on drop.
pub struct EgressInvocation {
    proxy: EgressProxy,
    /// Present in sandbox mode only.
    grant: Option<crate::sandbox::EgressGrant>,
    /// Other per-invocation services (the SSH broker) that must live exactly
    /// as long as this endpoint does.
    held: Vec<Box<dyn std::any::Any + Send + Sync>>,
}

impl EgressInvocation {
    /// Create an invocation-local endpoint from a session grant. Grants built
    /// without live proxy control are test doubles and retain their original
    /// endpoint.
    pub async fn start(template: Option<&crate::sandbox::EgressGrant>) -> io::Result<Option<Self>> {
        Self::start_labeled(template, None).await
    }

    /// As [`Self::start`], attributing every connection to `label` (the tool
    /// call id) in the session's event log.
    pub async fn start_labeled(
        template: Option<&crate::sandbox::EgressGrant>,
        label: Option<&str>,
    ) -> io::Result<Option<Self>> {
        let Some(shared) = template.and_then(|grant| grant.control.as_ref()) else {
            return Ok(None);
        };
        Self::start_for_shared(shared, label).await.map(Some)
    }

    /// A sandbox mode endpoint (loopback TCP plus a Unix socket) on the live
    /// state `shared`: its policy, grants and event log, with a refusal queue
    /// of its own and every connection attributed to `label`. For a confined
    /// process that is not a shell call, such as a long-lived MCP server.
    pub async fn start_for_shared(shared: &EgressShared, label: Option<&str>) -> io::Result<Self> {
        let mut proxy = EgressProxy::start_with_shared(shared.for_invocation(label)).await?;
        let sequence = NEXT_INVOCATION_SOCKET.fetch_add(1, Ordering::Relaxed);
        let socket_path =
            std::path::Path::new("/tmp").join(format!("f{}-{sequence}.sock", std::process::id()));
        // The Linux sandbox routes everything through the socket. On macOS
        // it is for clients that send plain requests to it instead of doing
        // TLS themselves; failing to bind it only loses that.
        if cfg!(target_os = "linux") {
            proxy.serve_on_unix_socket_inner(&socket_path).await?;
        } else {
            let _ = proxy.serve_on_unix_socket_inner(&socket_path).await;
        }
        let grant = crate::sandbox::EgressGrant {
            proxy_port: proxy.addr().port(),
            socket_path,
            control: Some(proxy.shared().clone()),
        };
        Ok(Self {
            proxy,
            grant: Some(grant),
            held: Vec::new(),
        })
    }

    /// A TCP-only endpoint (`127.0.0.1`, ephemeral port) for a caller that
    /// is not confined: an in-process tool or a host process it spawns. Same
    /// session policy, its own denied-host queue, connections attributed to
    /// `label`. `None` when `template` has no live proxy behind it.
    pub async fn start_tcp_labeled(
        template: Option<&crate::sandbox::EgressGrant>,
        label: Option<&str>,
    ) -> io::Result<Option<Self>> {
        let Some(shared) = template.and_then(|grant| grant.control.as_ref()) else {
            return Ok(None);
        };
        let proxy = EgressProxy::start_with_shared(shared.for_invocation(label)).await?;
        Ok(Some(Self {
            proxy,
            grant: None,
            held: Vec::new(),
        }))
    }

    /// This endpoint's live state.
    pub fn shared(&self) -> &EgressShared {
        self.proxy.shared()
    }

    /// Keep `service` alive until this endpoint is dropped.
    pub fn hold(&mut self, service: impl std::any::Any + Send + Sync) {
        self.held.push(Box::new(service));
    }

    /// The grant a sandbox policy consumes. `None` in TCP mode, which has no
    /// socket for a sandbox to mount.
    pub fn grant(&self) -> Option<&crate::sandbox::EgressGrant> {
        self.grant.as_ref()
    }

    /// The loopback TCP listener, in either mode.
    pub fn tcp_addr(&self) -> SocketAddr {
        self.proxy.tcp_addr()
    }

    /// The first host this endpoint refused since the last take, clearing
    /// the queue.
    pub fn take_denied_host(&self) -> Option<String> {
        self.proxy.shared().take_denied_host()
    }

    /// Every host this endpoint refused since the last take, oldest first,
    /// without clearing the queue.
    pub fn denied_hosts(&self) -> Vec<String> {
        self.proxy.shared().denied_hosts()
    }

    /// Hosts the user refused, when asked, during this invocation.
    pub fn take_user_refused_hosts(&self) -> Vec<String> {
        self.proxy.shared().take_user_refused_hosts()
    }

    pub fn label(&self) -> Option<&str> {
        self.proxy.shared().label()
    }

    /// Attribute this endpoint's connections to `label` until it is cleared
    /// with `None`. See [`EgressShared::set_call_label`].
    pub fn set_call_label(&self, label: Option<&str>) {
        self.proxy.shared().set_call_label(label);
    }
}

/// The gateway endpoint for one in-process or host-side tool call.
///
/// Tools that open their own connections (`web_fetch`, host `git`) do not
/// run inside the sandbox, so nothing forces their traffic anywhere. Routing
/// them through this endpoint is what puts them under the same policy,
/// address check, DNS pinning, timeouts and event log as a confined shell:
/// point the client's proxy at [`Self::proxy_url`] (plain HTTP is forwarded,
/// HTTPS is tunnelled with CONNECT), or for SSH use
/// `ssh -o ProxyCommand="<forge exe> __egress-connect 127.0.0.1:PORT %h %p"`.
///
/// A gateway refusal is a 403 to the client. The refused host is queued here,
/// so the tool can turn the failure into a host-grant prompt with
/// [`Self::sandbox_denied`] rather than guessing a host from client output.
///
/// Works the same on macOS and Linux: it is a loopback listener in this
/// process, with no sandbox involved. Dropping it stops listening.
pub struct CallGateway {
    invocation: EgressInvocation,
}

impl std::fmt::Debug for CallGateway {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallGateway")
            .field("addr", &self.addr())
            .field("label", &self.label())
            .finish()
    }
}

impl CallGateway {
    /// Start an endpoint sharing `grant`'s session policy, attributed to
    /// `label` (the tool call id). `None` when there is no grant, or the
    /// grant has no live proxy behind it: the caller keeps its direct path.
    pub async fn start(
        grant: Option<&crate::sandbox::EgressGrant>,
        label: Option<&str>,
    ) -> io::Result<Option<Self>> {
        Ok(EgressInvocation::start_tcp_labeled(grant, label)
            .await?
            .map(|invocation| Self { invocation }))
    }

    /// The loopback listener (`127.0.0.1`, ephemeral port).
    pub fn addr(&self) -> SocketAddr {
        self.invocation.tcp_addr()
    }

    /// `http://127.0.0.1:PORT`, for `HTTP_PROXY`/`HTTPS_PROXY` or a client's
    /// proxy setting.
    pub fn proxy_url(&self) -> String {
        format!("http://{}", self.addr())
    }

    pub fn label(&self) -> Option<&str> {
        self.invocation.label()
    }

    /// Whether the live session policy currently permits `host`.
    pub fn permits_host(&self, host: &str) -> bool {
        self.invocation.proxy.shared().permits_host(host)
    }

    /// Hosts this call's endpoint refused, oldest first, without clearing.
    pub fn denied_hosts(&self) -> Vec<String> {
        self.invocation.denied_hosts()
    }

    /// The first refused host, clearing the queue.
    pub fn take_denied_host(&self) -> Option<String> {
        self.invocation.take_denied_host()
    }

    /// The first host this endpoint refused for resolving to a non-public
    /// address, clearing that queue. Recorded by the gateway itself, so a
    /// caller can tell this refusal from an upstream's own 403 without
    /// trusting response content. Never a grant prompt: no grant fixes it.
    pub fn take_private_address_denial(&self) -> Option<String> {
        self.invocation.proxy.shared().take_private_address_denial()
    }

    /// When this endpoint refused a host, the structured denial that makes
    /// forge-core prompt for a host grant and retry the call. `content` is
    /// what the tool would otherwise have reported; the explanation is
    /// appended. Consumes the queued host. `None` when nothing was refused,
    /// so an ordinary upstream 403 stays an ordinary failure.
    pub fn sandbox_denied(&self, content: &str) -> Option<crate::ToolError> {
        let host = self.take_denied_host()?;
        let reason = HOST_DENIED_EXPLANATION.to_string();
        let mut reported = content.to_string();
        if !reported.contains(&reason) {
            if !reported.is_empty() {
                reported.push('\n');
            }
            reported.push_str(&reason);
        }
        Some(crate::ToolError::SandboxDenied {
            content: reported,
            reason,
            denied_host: Some(host),
        })
    }
}

/// Upper bound on a request or response head the gateway parses. Anything
/// larger is refused rather than buffered without limit.
const MAX_HEAD_BYTES: usize = 64 * 1024;
/// Largest request body the gateway reads to tell a read from a write.
const MAX_INSPECTED_BODY_BYTES: usize = 1024 * 1024;

/// What a client asked the gateway to do, once its head parsed.
#[derive(Debug, PartialEq, Eq)]
enum ProxyRequest {
    /// `CONNECT host:port` — an opaque tunnel.
    Connect { host: String, port: u16 },
    /// An absolute-form plain-HTTP request (`GET http://host/path HTTP/1.1`),
    /// rewritten to origin-form and forwarded.
    Forward(ForwardRequest),
    /// An origin-form request (`GET /path HTTP/1.1`, `Host: host`) for an
    /// HTTPS host. Accepted on the Unix socket only.
    Origin { host: String, port: u16 },
}

#[derive(Debug, PartialEq, Eq)]
struct ForwardRequest {
    method: String,
    host: String,
    port: u16,
    /// Origin-form target (`/path?query`), never empty.
    path: String,
    version: String,
}

/// Handle one client, whatever transport it arrived on.
///
/// Two request shapes are served, and both meet the same gateway: the host
/// policy, the address check with pinned resolution, the timeouts,
/// attribution and the event log.
///
/// * `CONNECT host:port` opens a byte tunnel. This is how HTTPS (and SSH via
///   `forge __egress-connect`) leaves; the gateway never sees the plaintext.
/// * An absolute-form plain-HTTP request (`GET http://host[:port]/path`, any
///   method) is forwarded: rewritten to origin-form with hop-by-hop and
///   `Proxy-*` headers stripped, sent once to the pinned upstream with
///   `Connection: close`, and the response streamed back. Plain HTTP is
///   plaintext on the wire anyway, so forwarding it needs no interception.
///   One request per client connection keeps the parser out of the response
///   body. Confined shells already get `HTTP_PROXY` pointing here, so
///   `curl http://…` from a sandbox works through this path too.
///
/// Anything else is a 400.
///
/// The stream is split *before* buffering so the read half can be buffered
/// while the write half stays usable. Wrapping the whole stream and later
/// unwrapping would discard anything the client pipelined behind its headers.
async fn serve<S>(client: S, shared: EgressShared, origin_form: bool) -> io::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + 'static,
{
    let started = Instant::now();
    let (read_half, mut write_half) = tokio::io::split(client);
    let mut reader = BufReader::new(read_half);

    let Some(head) = read_head(&mut reader).await? else {
        return respond(&mut write_half, 400, "Bad Request", "").await;
    };
    let Some((request, headers)) = parse_request(&head) else {
        return respond(&mut write_half, 400, "Bad Request", "").await;
    };
    let (host, port) = match &request {
        ProxyRequest::Connect { host, port } | ProxyRequest::Origin { host, port } => {
            (host.clone(), *port)
        }
        ProxyRequest::Forward(forward) => (forward.host.clone(), forward.port),
    };
    if matches!(request, ProxyRequest::Origin { .. }) && !origin_form {
        return respond(&mut write_half, 400, "Bad Request", "").await;
    }
    let attempt = Attempt {
        shared: &shared,
        label: shared.attribution(),
        host: &host,
        port,
        started,
    };

    let Some((upstream, upstream_addr)) = attempt.open_upstream(&mut write_half).await? else {
        return Ok(());
    };
    // A plain request for an HTTPS host, from a client that was told to
    // speak to this socket instead of doing TLS itself (`gh`): the gateway
    // does the TLS, and supplies the credential.
    if matches!(request, ProxyRequest::Origin { .. }) {
        let Some(upstream) =
            tls_to_upstream(upstream, upstream_addr, &mut write_half, &attempt).await?
        else {
            return Ok(());
        };
        let secret = shared.intercept.secret_for(&host);
        let identity = match &secret {
            Some(secret) => Identity::Supply(secret),
            None if inspected(&host) => Identity::Anonymous,
            None => Identity::Unchecked,
        };
        return forward_with_credentials(
            reader,
            write_half,
            upstream,
            upstream_addr,
            &head,
            identity,
            &attempt,
        )
        .await;
    }
    // A host Forge holds a credential for, reached by a client that trusts
    // the session authority: terminate, so the credential is supplied here
    // and never inside the sandbox.
    if matches!(request, ProxyRequest::Connect { .. })
        && port == 443
        && shared.intercepting.load(Ordering::Relaxed)
    {
        let secret = shared.intercept.secret_for(&host);
        if secret.is_some() || inspected(&host) {
            let client = tokio::io::join(reader, write_half);
            return serve_terminated(client, upstream, upstream_addr, secret.as_ref(), &attempt)
                .await;
        }
    }
    let (upstream_read, mut upstream_write) = tokio::io::split(upstream);

    match request {
        ProxyRequest::Origin { .. } => unreachable!("handled above"),
        ProxyRequest::Connect { .. } => {
            if let Err(error) = write_half
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
            {
                attempt.record(EgressOutcome::Allowed, Some(upstream_addr), 0, 0);
                return Err(error);
            }

            // From here the proxy copies bytes and never sees the plaintext.
            let (up, down) = relay(
                reader,
                write_half,
                upstream_read,
                upstream_write,
                shared.options.idle_timeout,
            )
            .await;
            attempt.record(EgressOutcome::Allowed, Some(upstream_addr), up, down);
        }
        ProxyRequest::Forward(forward) => {
            let request_head = forward.head(headers);
            if let Err(error) = upstream_write.write_all(request_head.as_bytes()).await {
                attempt.record(
                    EgressOutcome::UpstreamFailed(format!("write {upstream_addr}: {error}")),
                    None,
                    0,
                    0,
                );
                return respond(&mut write_half, 502, "Bad Gateway", "").await;
            }
            // Whatever the client sends after its head (the body, framed by
            // its own Content-Length or chunking) is copied verbatim, exactly
            // as a tunnel would — only to the host the policy already passed.
            let (up, down) = relay_forward(
                reader,
                write_half,
                upstream_read,
                upstream_write,
                shared.options.idle_timeout,
            )
            .await;
            attempt.record(
                EgressOutcome::Allowed,
                Some(upstream_addr),
                request_head.len() as u64 + up,
                down,
            );
        }
    }
    Ok(())
}

/// Terminate the client's TLS as `attempt.host`, forward one request to the
/// real host over TLS with the user's credential supplied, and relay the
/// response.
async fn serve_terminated<C>(
    mut client: C,
    upstream: TcpStream,
    upstream_addr: SocketAddr,
    secret: Option<&crate::tls_intercept::Secret>,
    attempt: &Attempt<'_>,
) -> io::Result<()>
where
    C: AsyncRead + AsyncWrite + Unpin,
{
    let (shared, host) = (attempt.shared, attempt.host);
    let failed = |why: String| attempt.record(EgressOutcome::UpstreamFailed(why), None, 0, 0);
    let Some(server_config) = shared.intercept.server_config(host) else {
        failed("cannot prepare TLS termination".into());
        return respond(&mut client, 502, "Bad Gateway", "").await;
    };
    client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await?;
    let accept = tokio_rustls::TlsAcceptor::from(server_config).accept(client);
    let client = match tokio::time::timeout(shared.options.connect_timeout, accept).await {
        Ok(Ok(client)) => client,
        // Most often a client that does not trust the session authority.
        Ok(Err(error)) => {
            failed(format!("client TLS (the sandbox certificate): {error}"));
            let mut untrusting = shared
                .untrusting
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !untrusting.iter().any(|known| known == host) {
                untrusting.push(host.to_string());
            }
            return Ok(());
        }
        Err(_) => {
            failed("client TLS timed out".into());
            return Ok(());
        }
    };
    let (client_read, mut client_write) = tokio::io::split(client);
    let mut client_read = BufReader::new(client_read);
    let Some(upstream) =
        tls_to_upstream(upstream, upstream_addr, &mut client_write, attempt).await?
    else {
        return Ok(());
    };

    let Some(head) = read_head(&mut client_read).await? else {
        failed("malformed request inside TLS".into());
        return respond(&mut client_write, 400, "Bad Request", "").await;
    };
    forward_with_credentials(
        client_read,
        client_write,
        upstream,
        upstream_addr,
        &head,
        match secret {
            Some(secret) => Identity::Supply(secret),
            None => Identity::Anonymous,
        },
        attempt,
    )
    .await
}

/// Whose credentials a forwarded request may carry.
#[derive(Clone, Copy)]
enum Identity<'a> {
    /// The user's: the placeholder becomes this credential; any other
    /// credential is refused.
    Supply(&'a crate::tls_intercept::Secret),
    /// Nobody's: Forge has no credential for this inspected host, so a
    /// request that carries one is refused.
    Anonymous,
    /// Not Forge's concern: a host that is neither credentialed nor
    /// inspected.
    Unchecked,
}

/// Forward one origin-form request (`head`) to `attempt.host` over the
/// established TLS connection `upstream`, supplying the user's credential in
/// place of its placeholder, and relay the response.
async fn forward_with_credentials<CR, CW, U>(
    client_read: CR,
    mut client_write: CW,
    upstream: U,
    upstream_addr: SocketAddr,
    head: &[String],
    identity: Identity<'_>,
    attempt: &Attempt<'_>,
) -> io::Result<()>
where
    CR: AsyncRead + Unpin,
    CW: AsyncWrite + Unpin,
    U: AsyncRead + AsyncWrite + Unpin,
{
    use tokio::io::AsyncReadExt as _;
    let (shared, host) = (attempt.shared, attempt.host);
    let failed = |why: String| attempt.record(EgressOutcome::UpstreamFailed(why), None, 0, 0);
    let (upstream_read, mut upstream_write) = tokio::io::split(upstream);
    let request = parse_origin_form(head);
    let Some((mut request_head, headers)) = request else {
        failed("malformed request".into());
        return respond(&mut client_write, 400, "Bad Request", "").await;
    };
    let secret = match identity {
        Identity::Supply(secret) => Some(secret),
        _ => None,
    };
    let headers = match identity {
        Identity::Supply(secret) => crate::tls_intercept::supply_credentials(secret, headers),
        Identity::Anonymous
            if headers
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("authorization")) =>
        {
            Err(crate::tls_intercept::ForeignCredentials)
        }
        Identity::Anonymous | Identity::Unchecked => Ok((
            headers
                .into_iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect(),
            false,
        )),
    };
    let Ok((headers, as_the_user)) = headers else {
        attempt.record(EgressOutcome::Denied(EgressDenial::Policy), None, 0, 0);
        let body = format!(
            "{SANDBOX_DENIED_REASON}: this request to {host} carries credentials Forge did not \
             supply. Inside the sandbox, {host} is reached with the user's own identity only.\n"
        );
        return respond(&mut client_write, 403, SANDBOX_DENIED_REASON, &body).await;
    };
    for (name, value) in &headers {
        if !name.eq_ignore_ascii_case("host") {
            request_head.push_str(&format!("{name}: {value}\r\n"));
        }
    }
    request_head.push_str(&format!("Host: {host}\r\nConnection: close\r\n\r\n"));

    // A request made with the user's credential that changes something is
    // held until its target is known to be allowed. Reads go through.
    let mut client_read = client_read;
    let mut body = Vec::new();
    if let Some(secret) = secret.filter(|_| as_the_user) {
        let mut line = head[0].split(' ');
        let (method, target) = (line.next().unwrap_or(""), line.next().unwrap_or(""));
        let mut classified = crate::write_gate::classify(&secret.host, host, method, target);
        if classified == crate::write_gate::Classified::GraphQl {
            // Query or mutation is only in the body.
            let length = headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, value)| value.parse::<usize>().ok())
                .filter(|length| *length <= MAX_INSPECTED_BODY_BYTES);
            classified = match length {
                Some(length) => {
                    body = vec![0; length];
                    client_read.read_exact(&mut body).await?;
                    crate::write_gate::classify_graphql(&secret.host, &body)
                }
                None => crate::write_gate::Classified::Write(crate::write_gate::Write {
                    target: secret.host.clone(),
                    summary: "API change".into(),
                }),
            };
        }
        if let crate::write_gate::Classified::Write(write) = classified {
            match shared.admit_write(&write).await {
                Admission::Allowed => {}
                Admission::RefusedByUser => {
                    attempt.record(EgressOutcome::Denied(EgressDenial::Policy), None, 0, 0);
                    let body = format!("Refused by the user: {}\n", write.summary);
                    return respond(&mut client_write, 403, "Refused By User", &body).await;
                }
                Admission::Refused => {
                    attempt.record(EgressOutcome::Denied(EgressDenial::Policy), None, 0, 0);
                    let body = format!(
                        "{SANDBOX_DENIED_REASON}: {} on {} was not approved. A command in the \
                         sandbox may change things under the user's identity only where the \
                         user has allowed it.\n",
                        write.summary, write.target
                    );
                    return respond(&mut client_write, 403, SANDBOX_DENIED_REASON, &body).await;
                }
            }
        }
    }
    let sent = async {
        upstream_write.write_all(request_head.as_bytes()).await?;
        upstream_write.write_all(&body).await
    };
    if let Err(error) = sent.await {
        failed(format!("write {upstream_addr}: {error}"));
        return respond(&mut client_write, 502, "Bad Gateway", "").await;
    }
    let (up, down) = relay_forward(
        client_read,
        client_write,
        upstream_read,
        upstream_write,
        shared.options.idle_timeout,
    )
    .await;
    attempt.record(
        EgressOutcome::Allowed,
        Some(upstream_addr),
        request_head.len() as u64 + up,
        down,
    );
    Ok(())
}

/// `METHOD /target HTTP/1.x` and its headers, hop-by-hop ones dropped: the
/// request line to send, and the headers. `None` for anything else.
fn parse_origin_form(head: &[String]) -> Option<(String, Vec<(&str, &str)>)> {
    let (line, header_lines) = head.split_first()?;
    let mut parts = line.split(' ');
    let (method, target, version) = (parts.next()?, parts.next()?, parts.next()?);
    let valid = parts.next().is_none()
        && !method.is_empty()
        && method.bytes().all(is_token_byte)
        && target.starts_with('/')
        && matches!(version, "HTTP/1.1" | "HTTP/1.0");
    valid.then_some(())?;
    Some((
        format!("{method} {target} {version}\r\n"),
        forward_headers(header_lines)?,
    ))
}

/// TLS from the gateway to the real `attempt.host`, verified against the
/// public roots. On failure the client has been answered.
async fn tls_to_upstream<W>(
    upstream: TcpStream,
    upstream_addr: SocketAddr,
    client: &mut W,
    attempt: &Attempt<'_>,
) -> io::Result<Option<tokio_rustls::client::TlsStream<TcpStream>>>
where
    W: AsyncWrite + Unpin,
{
    let (shared, host) = (attempt.shared, attempt.host);
    let failed = |why: String| attempt.record(EgressOutcome::UpstreamFailed(why), None, 0, 0);
    let config = shared
        .intercept
        .client_config()
        .zip(rustls::pki_types::ServerName::try_from(host.to_string()).ok());
    let Some((config, server_name)) = config else {
        failed("cannot prepare TLS".into());
        respond(client, 502, "Bad Gateway", "").await?;
        return Ok(None);
    };
    let connect = tokio_rustls::TlsConnector::from(config).connect(server_name, upstream);
    match tokio::time::timeout(shared.options.connect_timeout, connect).await {
        Ok(Ok(upstream)) => Ok(Some(upstream)),
        Ok(Err(error)) => {
            failed(format!("TLS to {upstream_addr}: {error}"));
            respond(client, 502, "Bad Gateway", "").await?;
            Ok(None)
        }
        Err(_) => {
            failed(format!("TLS to {upstream_addr}: timed out"));
            respond(client, 504, "Gateway Timeout", "").await?;
            Ok(None)
        }
    }
}

/// One destination decision, attributed and timed from the client's arrival.
struct Attempt<'a> {
    shared: &'a EgressShared,
    /// Captured when the client arrives, so a tunnel that outlives a call
    /// label is still recorded under the call that opened it.
    label: Option<String>,
    host: &'a str,
    port: u16,
    started: Instant,
}

impl Attempt<'_> {
    fn record(&self, outcome: EgressOutcome, upstream: Option<SocketAddr>, up: u64, down: u64) {
        self.shared.record_event(EgressEvent {
            label: self.label.clone(),
            host: self.host.to_string(),
            port: self.port,
            outcome,
            upstream,
            bytes_up: up,
            bytes_down: down,
            duration: self.started.elapsed(),
        });
    }

    /// Apply the policy and the address check, then dial a checked address.
    ///
    /// On any refusal or failure the client has already been answered and the
    /// event recorded, and `None` is returned. Both request shapes go through
    /// here, so plain HTTP cannot be a way around a rule CONNECT obeys.
    async fn open_upstream<W>(&self, client: &mut W) -> io::Result<Option<(TcpStream, SocketAddr)>>
    where
        W: AsyncWrite + Unpin,
    {
        let (shared, host, port) = (self.shared, self.host, self.port);
        match shared.admission(host, port).await {
            Admission::Allowed => {}
            Admission::Refused => {
                shared.record_denied(host.to_string());
                self.record(EgressOutcome::Denied(EgressDenial::Policy), None, 0, 0);
                let body = format!("{SANDBOX_DENIED_REASON}: host {host} is not allowed\n");
                respond(client, 403, SANDBOX_DENIED_REASON, &body).await?;
                return Ok(None);
            }
            // Not queued as a denial: that queue raises a host approval, and
            // the user has just answered this one.
            Admission::RefusedByUser => {
                self.record(EgressOutcome::Denied(EgressDenial::Policy), None, 0, 0);
                let body = format!("Refused by the user: host {host}\n");
                respond(client, 403, "Refused By User", &body).await?;
                return Ok(None);
            }
        }

        // Resolve here, check every address, and dial only what was checked.
        // A second resolution by the dialler is a rebinding window: the name
        // could answer with a public address for the check and loopback for
        // the dial.
        let deadline = tokio::time::Instant::now() + shared.options.connect_timeout;
        let addrs = match tokio::time::timeout_at(deadline, resolve(host, port)).await {
            Ok(Ok(addrs)) => addrs,
            Ok(Err(error)) => {
                self.record(
                    EgressOutcome::UpstreamFailed(format!("resolve: {error}")),
                    None,
                    0,
                    0,
                );
                respond(client, 502, "Bad Gateway", "").await?;
                return Ok(None);
            }
            Err(_) => {
                self.record(
                    EgressOutcome::UpstreamFailed("resolve timed out".into()),
                    None,
                    0,
                    0,
                );
                respond(client, 504, "Gateway Timeout", "").await?;
                return Ok(None);
            }
        };

        if addrs.iter().any(|addr| !is_public_address(addr.ip()))
            && !shared.private_destination_permitted(host)
        {
            // Not queued as a denied host: a host grant cannot fix this, so
            // it must not turn into a grant prompt. It is queued separately
            // so an in-process caller can report it as what it is.
            shared.record_private_denied(host.to_string());
            self.record(
                EgressOutcome::Denied(EgressDenial::PrivateAddress),
                None,
                0,
                0,
            );
            let body = format!(
                "{PRIVATE_ADDRESS_DENIED_REASON}: host {host} resolves to a non-public address\n"
            );
            respond(client, 403, PRIVATE_ADDRESS_DENIED_REASON, &body).await?;
            return Ok(None);
        }

        let mut failure = String::from("no address");
        for addr in &addrs {
            match tokio::time::timeout_at(deadline, TcpStream::connect(addr)).await {
                Ok(Ok(stream)) => return Ok(Some((stream, *addr))),
                Ok(Err(error)) => failure = format!("connect {addr}: {error}"),
                Err(_) => {
                    failure = format!("connect {addr}: timed out");
                    break;
                }
            }
        }
        self.record(EgressOutcome::UpstreamFailed(failure), None, 0, 0);
        respond(client, 502, "Bad Gateway", "").await?;
        Ok(None)
    }
}

/// Read a head up to and including its blank line, as lines without their
/// line endings. `None` when it is larger than [`MAX_HEAD_BYTES`] or is not
/// UTF-8. End of stream ends the head early, which leaves an incomplete
/// request for the parser to reject.
async fn read_head<R>(reader: &mut R) -> io::Result<Option<Vec<String>>>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    let mut lines = Vec::new();
    let mut total = 0usize;
    loop {
        let mut line = Vec::new();
        let limit = (MAX_HEAD_BYTES - total) as u64;
        let read = (&mut *reader)
            .take(limit)
            .read_until(b'\n', &mut line)
            .await?;
        if read == 0 {
            break;
        }
        total += read;
        let Ok(line) = String::from_utf8(line) else {
            return Ok(None);
        };
        let line = line.strip_suffix('\n').unwrap_or(&line);
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            break;
        }
        if total >= MAX_HEAD_BYTES {
            return Ok(None);
        }
        lines.push(line.to_string());
    }
    Ok(Some(lines))
}

/// Parse a request head into what to do and the header lines that follow
/// the request line. `None` is a 400.
fn parse_request(head: &[String]) -> Option<(ProxyRequest, &[String])> {
    let (line, headers) = head.split_first()?;
    if let Some(target) = parse_connect(line) {
        // Parse once, then dial the parsed parts. Deriving a host for the
        // policy check while dialling the original string lets the two
        // disagree, and a target the policy never saw is a bypass however it
        // is spelled — an earlier version approved `[crates.io]:443` as
        // `crates.io` and then dialled the bracketed form.
        let (host, port) = split_host_port(&target)?;
        return Some((ProxyRequest::Connect { host, port }, headers));
    }
    if let Some((_, origin_headers)) = parse_origin_form(head) {
        let target = origin_headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("host"))?
            .1;
        let (host, port) = if target.contains(':') && !target.ends_with(']') {
            split_host_port(target)?
        } else {
            split_host_port(&format!("{target}:443"))?
        };
        return Some((ProxyRequest::Origin { host, port }, headers));
    }
    let forward = parse_absolute_form(line)?;
    // Validate the headers before the policy runs, so a malformed request is
    // a 400 whatever its destination.
    forward_headers(headers)?;
    Some((ProxyRequest::Forward(forward), headers))
}

/// `METHOD http://host[:port][/path][?query] HTTP/1.x`.
///
/// Only the `http` scheme: an HTTPS client asks for a tunnel with CONNECT,
/// and a proxy that fetched `https://` URLs itself would be terminating TLS
/// on the client's behalf. Userinfo is refused rather than stripped, so the
/// policy never has to decide which side of an `@` is the real host.
fn parse_absolute_form(line: &str) -> Option<ForwardRequest> {
    let mut parts = line.split(' ');
    let (method, target, version) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    if method.is_empty()
        || !method.bytes().all(is_token_byte)
        || method.eq_ignore_ascii_case("CONNECT")
        || !matches!(version, "HTTP/1.1" | "HTTP/1.0")
    {
        return None;
    }
    let scheme_len = "http://".len();
    if target.len() < scheme_len || !target[..scheme_len].eq_ignore_ascii_case("http://") {
        return None;
    }
    let rest = &target[scheme_len..];
    let (authority, path) = match rest.find(['/', '?', '#']) {
        Some(index) => rest.split_at(index),
        None => (rest, ""),
    };
    if authority.is_empty()
        || !authority.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b':' | b'[' | b']')
        })
    {
        return None;
    }
    let has_port = if authority.starts_with('[') {
        authority.contains("]:")
    } else {
        authority.contains(':')
    };
    let (host, port) = if has_port {
        split_host_port(authority)?
    } else {
        split_host_port(&format!("{authority}:80"))?
    };
    // The fragment is never sent to a server.
    let path = path.split('#').next().unwrap_or_default();
    if path.bytes().any(|b| b <= 0x20 || b == 0x7f) {
        return None;
    }
    let path = match path {
        "" => "/".to_string(),
        query if query.starts_with('?') => format!("/{query}"),
        path => path.to_string(),
    };
    Some(ForwardRequest {
        method: method.to_string(),
        host,
        port,
        path,
        version: version.to_string(),
    })
}

/// RFC 9110 `tchar`, the characters of a method or header name.
fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

/// Headers that describe one connection rather than the message, and so
/// must not cross the gateway.
fn is_hop_by_hop(name: &str) -> bool {
    matches!(
        name,
        "connection" | "keep-alive" | "te" | "trailer" | "upgrade"
    ) || name.starts_with("proxy-")
}

/// Body framing: forwarded verbatim alongside the body bytes, so never
/// removed even when a `Connection` header names it.
fn is_framing(name: &str) -> bool {
    matches!(name, "content-length" | "transfer-encoding")
}

/// Parse header lines and drop the hop-by-hop ones: the fixed set, every
/// `Proxy-*`, and any header a `Connection` header names. `None` for a
/// malformed line, including obsolete line folding.
fn forward_headers(lines: &[String]) -> Option<Vec<(&str, &str)>> {
    let mut headers = Vec::with_capacity(lines.len());
    for line in lines {
        if line.starts_with([' ', '\t']) {
            return None;
        }
        let (name, value) = line.split_once(':')?;
        if name.is_empty() || !name.bytes().all(is_token_byte) {
            return None;
        }
        headers.push((name, value.trim()));
    }
    let named: Vec<String> = headers
        .iter()
        .filter(|(name, _)| {
            name.eq_ignore_ascii_case("connection") || name.eq_ignore_ascii_case("proxy-connection")
        })
        .flat_map(|(_, value)| value.split(','))
        .map(|token| token.trim().to_ascii_lowercase())
        .filter(|token| !token.is_empty() && !is_framing(token))
        .collect();
    Some(
        headers
            .into_iter()
            .filter(|(name, _)| {
                let name = name.to_ascii_lowercase();
                !is_hop_by_hop(&name) && !named.contains(&name)
            })
            .collect(),
    )
}

impl ForwardRequest {
    /// The origin-form head sent upstream. The `Host` header is rebuilt
    /// from the URI the policy checked, so a client cannot name one host in
    /// the request line and another to the server.
    fn head(&self, header_lines: &[String]) -> String {
        let mut head = format!("{} {} {}\r\n", self.method, self.path, self.version);
        for (name, value) in forward_headers(header_lines).unwrap_or_default() {
            if name.eq_ignore_ascii_case("host") {
                continue;
            }
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        if self.port == 80 {
            head.push_str(&format!("Host: {host}\r\n"));
        } else {
            head.push_str(&format!("Host: {host}:{}\r\n", self.port));
        }
        head.push_str("Connection: close\r\n\r\n");
        head
    }
}

/// Rewrite an upstream response head for the client: hop-by-hop headers
/// dropped and `Connection: close` added, since the gateway closes after one
/// exchange. An interim (1xx) head passes through unchanged. `None` when the
/// upstream did not answer with HTTP.
fn rewrite_response_head(lines: &[String]) -> Option<String> {
    let (status, header_lines) = lines.split_first()?;
    let mut parts = status.splitn(3, ' ');
    let version = parts.next()?;
    let code = parts.next()?;
    if !version.starts_with("HTTP/1.")
        || code.len() != 3
        || !code.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    if code.starts_with('1') {
        let mut head = lines.join("\r\n");
        head.push_str("\r\n\r\n");
        return Some(head);
    }
    let mut head = format!("{status}\r\n");
    for (name, value) in forward_headers(header_lines)? {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("Connection: close\r\n\r\n");
    Some(head)
}

/// Every address `host` names, resolved once. An IP literal is its own
/// answer and never touches the resolver.
async fn resolve(host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(vec![SocketAddr::new(ip, port)]);
    }
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port)).await?.collect();
    if addrs.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "name resolved to no addresses",
        ));
    }
    Ok(addrs)
}

/// Whether `ip` is a globally routable unicast address.
///
/// Refused: unspecified, loopback, RFC 1918 private, CGNAT shared space,
/// link-local (which includes cloud metadata at 169.254.169.254), multicast,
/// broadcast and reserved IPv4; and for IPv6 the unspecified/loopback/
/// IPv4-compatible block, unique-local, link-local, site-local and multicast,
/// with IPv4-mapped addresses judged as the IPv4 address they carry.
fn is_public_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_v4(v4);
            }
            let segments = v6.segments();
            let first = segments[0];
            let compat_block = segments[..6].iter().all(|segment| *segment == 0);
            !(compat_block
                || v6.is_multicast()
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80
                || (first & 0xffc0) == 0xfec0)
        }
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    !(a == 0
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_broadcast()
        || (a == 100 && (b & 0xc0) == 64)
        || a >= 240)
}

/// Last time either direction of a tunnel moved a byte, as nanoseconds since
/// the tunnel opened.
struct Activity {
    opened: Instant,
    last: AtomicU64,
}

impl Activity {
    fn touch(&self) {
        let now = self.opened.elapsed().as_nanos() as u64;
        self.last.fetch_max(now, Ordering::Relaxed);
    }

    fn idle_for(&self) -> Duration {
        let last = Duration::from_nanos(self.last.load(Ordering::Relaxed));
        self.opened.elapsed().saturating_sub(last)
    }
}

/// Copy both directions until both finish or the tunnel goes idle, returning
/// `(bytes_up, bytes_down)`. Returning drops every half, which closes both
/// sockets — that is what makes the idle timeout close the tunnel.
async fn relay<CR, CW, UR, UW>(
    client_read: CR,
    client_write: CW,
    upstream_read: UR,
    upstream_write: UW,
    idle_timeout: Duration,
) -> (u64, u64)
where
    CR: AsyncRead + Unpin,
    CW: AsyncWrite + Unpin,
    UR: AsyncRead + Unpin,
    UW: AsyncWrite + Unpin,
{
    let traffic = Traffic::new();
    tokio::select! {
        _ = async {
            tokio::join!(
                pump(client_read, upstream_write, &traffic.up, &traffic.activity),
                pump(upstream_read, client_write, &traffic.down, &traffic.activity),
            )
        } => {}
        () = traffic.idle(idle_timeout) => {}
    }
    traffic.totals()
}

/// Byte counters and activity clock shared by both directions of a relay.
struct Traffic {
    activity: Activity,
    up: AtomicU64,
    down: AtomicU64,
}

impl Traffic {
    fn new() -> Self {
        Self {
            activity: Activity {
                opened: Instant::now(),
                last: AtomicU64::new(0),
            },
            up: AtomicU64::new(0),
            down: AtomicU64::new(0),
        }
    }

    /// Completes once neither direction has moved a byte for `idle_timeout`.
    async fn idle(&self, idle_timeout: Duration) {
        loop {
            let idle = self.activity.idle_for();
            if idle >= idle_timeout {
                return;
            }
            tokio::time::sleep(idle_timeout - idle).await;
        }
    }

    fn totals(&self) -> (u64, u64) {
        (
            self.up.load(Ordering::Relaxed),
            self.down.load(Ordering::Relaxed),
        )
    }
}

/// Relay one forwarded plain-HTTP exchange, returning `(bytes_up,
/// bytes_down)`; the request head written before this is not counted.
///
/// Unlike a tunnel, the response decides when it is over: the upstream was
/// sent `Connection: close`, so its end of stream is the end of the exchange,
/// even while the client (waiting for that answer) still has its side open.
async fn relay_forward<CR, CW, UR, UW>(
    client_read: CR,
    mut client_write: CW,
    upstream_read: UR,
    upstream_write: UW,
    idle_timeout: Duration,
) -> (u64, u64)
where
    CR: AsyncRead + Unpin,
    CW: AsyncWrite + Unpin,
    UR: AsyncRead + Unpin,
    UW: AsyncWrite + Unpin,
{
    let traffic = Traffic::new();
    let t = &traffic;
    let up = async move {
        pump(client_read, upstream_write, &t.up, &t.activity).await;
        std::future::pending::<()>().await
    };
    let down = async move {
        let mut upstream = BufReader::new(upstream_read);
        let head = match read_head(&mut upstream).await {
            Ok(Some(lines)) => rewrite_response_head(&lines),
            _ => None,
        };
        t.activity.touch();
        let Some(head) = head else {
            let _ = respond(&mut client_write, 502, "Bad Gateway", "").await;
            return;
        };
        if client_write.write_all(head.as_bytes()).await.is_err() {
            return;
        }
        t.down.fetch_add(head.len() as u64, Ordering::Relaxed);
        t.activity.touch();
        pump(upstream, client_write, &t.down, &t.activity).await;
    };
    tokio::select! {
        () = up => {}
        () = down => {}
        () = traffic.idle(idle_timeout) => {}
    }
    traffic.totals()
}

async fn pump<R, W>(mut from: R, mut to: W, counter: &AtomicU64, activity: &Activity)
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let read = match from.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        activity.touch();
        if to.write_all(&buf[..read]).await.is_err() {
            break;
        }
        counter.fetch_add(read as u64, Ordering::Relaxed);
        activity.touch();
    }
    // Propagate the half-close so the far side sees end of stream.
    let _ = to.shutdown().await;
}

async fn serve_unix(client: UnixStream, shared: EgressShared) -> io::Result<()> {
    serve(client, shared, true).await
}

/// The grant for a denied host: exactly that host, as the approval card
/// names it. A grant for one host never opens its siblings.
pub fn host_grant_pattern(host: &str) -> String {
    host.trim().trim_end_matches('.').to_ascii_lowercase()
}

pub fn host_allow_rule(host: &str) -> String {
    format!("host({})", host_grant_pattern(host))
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

async fn respond<W>(client: &mut W, code: u16, reason: &str, body: &str) -> io::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
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

    /// Starts the loopback egress proxy, or skips the calling test on hosts
    /// that deny binding a listener (CI sandboxes, agent harnesses).
    async fn started_proxy(policy: EgressPolicy) -> Option<EgressProxy> {
        match EgressProxy::start(policy).await {
            Ok(proxy) => Some(proxy),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("skipping: this host denies binding a listener");
                None
            }
            Err(e) => panic!("egress proxy failed to start: {e}"),
        }
    }

    #[test]
    fn only_globally_routable_addresses_are_public() {
        for public in ["1.1.1.1", "8.8.8.8", "151.101.1.1", "2606:4700::1111"] {
            assert!(
                is_public_address(public.parse().unwrap()),
                "{public} is public"
            );
        }
        for private in [
            "0.0.0.0",
            "127.0.0.1",
            "127.255.0.9",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "224.0.0.1",
            "255.255.255.255",
            "240.0.0.1",
            "::",
            "::1",
            "::127.0.0.1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "fec0::1",
            "ff02::1",
        ] {
            assert!(
                !is_public_address(private.parse().unwrap()),
                "{private} must not be public"
            );
        }
    }

    #[test]
    fn an_empty_policy_permits_nothing() {
        assert!(!EgressPolicy::new().permits("crates.io"));
    }

    #[tokio::test]
    async fn denied_connect_identifies_the_sandbox() {
        let Some(proxy) = started_proxy(EgressPolicy::new()).await else {
            return;
        };
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
        let Some(proxy) = started_proxy(policy).await else {
            return;
        };

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
    fn absolute_form_requests_parse_to_origin_form() {
        let parsed = parse_absolute_form("GET http://Example.com/a/b?c=d#frag HTTP/1.1").unwrap();
        assert_eq!(
            parsed,
            ForwardRequest {
                method: "GET".into(),
                host: "Example.com".into(),
                port: 80,
                path: "/a/b?c=d".into(),
                version: "HTTP/1.1".into(),
            }
        );
        let parsed = parse_absolute_form("POST HTTP://127.0.0.1:8080 HTTP/1.0").unwrap();
        assert_eq!((parsed.port, parsed.path.as_str()), (8080, "/"));
        let parsed = parse_absolute_form("GET http://[::1]:81?q HTTP/1.1").unwrap();
        assert_eq!(
            (parsed.host.as_str(), parsed.port, parsed.path.as_str()),
            ("::1", 81, "/?q")
        );
        assert_eq!(
            parse_absolute_form("GET http://[::1]/ HTTP/1.1")
                .unwrap()
                .port,
            80
        );
    }

    #[test]
    fn malformed_or_unsupported_requests_do_not_parse() {
        for line in [
            "GET /origin-form HTTP/1.1",
            "GET https://crates.io/ HTTP/1.1",
            "GET http://user@crates.io/ HTTP/1.1",
            "GET http://crates.io@evil.com/ HTTP/1.1",
            "GET http:///path HTTP/1.1",
            "GET http://crates.io:/ HTTP/1.1",
            "GET http://crates.io:99999/ HTTP/1.1",
            "GET http://[crates.io]/ HTTP/1.1",
            "GET http://crates.io%2eevil.com/ HTTP/1.1",
            "GET http://crates.io/ HTTP/2",
            "GET http://crates.io/ HTTP/1.1 extra",
            "GET  http://crates.io/ HTTP/1.1",
            "G(T http://crates.io/ HTTP/1.1",
            "CONNECT http://crates.io/ HTTP/1.1",
            "",
        ] {
            assert_eq!(parse_absolute_form(line), None, "{line:?} must be a 400");
        }
    }

    #[test]
    fn forwarded_heads_drop_hop_by_hop_and_rebuild_host() {
        let request = parse_absolute_form("GET http://crates.io:8080/x HTTP/1.1").unwrap();
        let headers: Vec<String> = [
            "Host: evil.com",
            "Proxy-Authorization: Basic abc",
            "Proxy-Connection: keep-alive",
            "Connection: keep-alive, X-Secret",
            "Keep-Alive: timeout=5",
            "X-Secret: 1",
            "Upgrade: websocket",
            "TE: trailers",
            "Content-Length: 4",
            "X-Kept: yes",
        ]
        .map(String::from)
        .to_vec();
        let head = request.head(&headers);
        assert_eq!(
            head,
            "GET /x HTTP/1.1\r\nContent-Length: 4\r\nX-Kept: yes\r\n\
             Host: crates.io:8080\r\nConnection: close\r\n\r\n"
        );
    }

    #[test]
    fn a_connection_header_cannot_strip_body_framing() {
        let headers = [
            "Connection: content-length, transfer-encoding",
            "Content-Length: 3",
        ]
        .map(String::from);
        let kept = forward_headers(&headers).unwrap();
        assert_eq!(kept, vec![("Content-Length", "3")]);
    }

    #[test]
    fn folded_or_nameless_header_lines_are_malformed() {
        for line in [
            " continued",
            "\tcontinued",
            "no colon",
            ": value",
            "bad name: v",
        ] {
            assert!(
                forward_headers(&[line.to_string()]).is_none(),
                "{line:?} must be rejected"
            );
        }
    }

    #[test]
    fn response_heads_are_rewritten_to_close() {
        let lines = [
            "HTTP/1.1 200 OK",
            "Connection: keep-alive",
            "Keep-Alive: timeout=5",
            "Content-Length: 5",
        ]
        .map(String::from);
        assert_eq!(
            rewrite_response_head(&lines).unwrap(),
            "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\n"
        );
        let interim = ["HTTP/1.1 100 Continue".to_string()];
        assert_eq!(
            rewrite_response_head(&interim).unwrap(),
            "HTTP/1.1 100 Continue\r\n\r\n"
        );
        assert!(rewrite_response_head(&["SSH-2.0-OpenSSH".to_string()]).is_none());
        assert!(rewrite_response_head(&["HTTP/1.1 2000 OK".to_string()]).is_none());
    }

    #[tokio::test]
    async fn an_oversized_head_is_refused() {
        let mut head = b"GET http://crates.io/ HTTP/1.1\r\n".to_vec();
        head.extend(std::iter::repeat_n(b'a', MAX_HEAD_BYTES));
        head.extend_from_slice(b"\r\n\r\n");
        let mut reader = BufReader::new(head.as_slice());
        assert_eq!(read_head(&mut reader).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_call_gateway_needs_a_live_grant() {
        assert!(CallGateway::start(None, Some("call"))
            .await
            .unwrap()
            .is_none());
        let double = crate::sandbox::EgressGrant {
            proxy_port: 1,
            socket_path: PathBuf::from("/nonexistent.sock"),
            control: None,
        };
        assert!(CallGateway::start(Some(&double), Some("call"))
            .await
            .unwrap()
            .is_none());
        let ctx = crate::ToolContext::new(PathBuf::from("/tmp"));
        assert!(ctx.gateway_for_call().await.unwrap().is_none());
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
    fn a_host_grant_is_exactly_the_denied_host() {
        assert_eq!(host_grant_pattern("API.github.com."), "api.github.com");
        assert_eq!(host_grant_pattern("github.com"), "github.com");
        assert_eq!(
            host_grant_pattern("uploads.github.com"),
            "uploads.github.com"
        );
        assert_eq!(host_allow_rule("api.github.com"), "host(api.github.com)");
        assert_eq!(host_grant_pattern("127.0.0.1"), "127.0.0.1");
        assert_eq!(host_grant_pattern("localhost"), "localhost");
    }

    #[test]
    fn a_proxy_refusal_is_a_sandbox_denial_even_when_the_client_prints_something_else() {
        let dir = tempfile::tempdir().unwrap();
        let output = "HTTP 403: unexpected status from the remote API";
        let error = denial_for_confined_command(
            "curl https://example.com",
            output,
            output,
            false,
            "sh",
            dir.path(),
            Some("example.com".into()),
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
    fn diagnostic_urls_do_not_authorize_host_grant_routing() {
        let dir = tempfile::tempdir().unwrap();
        for output in [
            "_LSOpenURLsWithCompletionHandler() failed with error -54 for the URL https://google.com.",
            "Operation not permitted: https://google.com",
        ] {
            let error = denial_for_confined_command(
                "open https://google.com",
                output, output, false, "bash", dir.path(), None,
            ).expect("expected sandbox failure");
            let crate::ToolError::SandboxDenied { denied_host, .. } = error else {
                panic!("expected SandboxDenied");
            };
            assert_eq!(denied_host, None, "diagnostic URL is not proxy evidence");
        }
    }

    #[test]
    fn a_failed_command_is_not_a_sandbox_denial_without_a_proxy_refusal() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            denial_for_confined_command(
                "curl https://example.com",
                "HTTP 403: unexpected status from the remote API\nhttps://example.com/v1",
                "HTTP 403: unexpected status from the remote API\nhttps://example.com/v1",
                false,
                "sh",
                dir.path(),
                None,
            )
            .is_none(),
            "a URL in a real HTTP 403 must not invent a host grant"
        );
    }

    #[test]
    fn a_shell_denial_in_merged_pty_output_is_reported_after_zero_exit() {
        let dir = tempfile::tempdir().unwrap();
        let error = denial_for_confined_command(
            "printf test > /tmp/escape.txt",
            "sh: 1: cannot create /tmp/escape.txt: Operation not permitted",
            "",
            true,
            "sh",
            dir.path(),
            None,
        )
        .expect("a merged PTY shell denial must remain observable");
        assert!(matches!(error, crate::ToolError::SandboxDenied { .. }));
    }

    #[test]
    fn a_runtime_denial_on_stderr_is_reported_when_stdout_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let error = denial_for_confined_command(
            "python3 -c 'open(\"/tmp/escape.txt\").read()' || true",
            "",
            "PermissionError: [Errno 1] Operation not permitted: '/tmp/escape.txt'",
            false,
            "sh",
            dir.path(),
            None,
        )
        .expect("runtime filesystem denials must be classified from stderr");
        assert!(matches!(error, crate::ToolError::SandboxDenied { .. }));
    }

    #[test]
    fn an_explicit_child_denial_is_reported_after_zero_exit() {
        let dir = tempfile::tempdir().unwrap();
        let error = denial_for_confined_command(
            "python3 -c 'open(\"/tmp/escape.txt\").read()' || true",
            "Traceback (most recent call last):\nPermissionError: [Errno 1] Operation not permitted: '/tmp/escape.txt'",
            "",
            true,
            "sh",
            dir.path(),
            None,
        )
        .expect("an explicitly requested child denial must remain observable");
        assert!(matches!(error, crate::ToolError::SandboxDenied { .. }));
    }

    #[test]
    fn an_implicit_child_cache_warning_does_not_trigger_hitl() {
        let dir = tempfile::tempdir().unwrap();
        assert!(denial_for_confined_command(
            "cargo test",
            "warning: xcrun: error: couldn't create cache file '/var/folders/x/xcrun_db-abc' (errno=Operation not permitted)",
            "",
            true,
            "sh",
            dir.path(),
            None,
        )
        .is_none());
    }

    #[tokio::test]
    async fn granting_a_host_unblocks_the_next_connect() {
        let Some(proxy) = started_proxy(EgressPolicy::new()).await else {
            return;
        };
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

    /// Send one CONNECT and return the status line.
    async fn connect_line(port: u16, target: &str) -> String {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        stream
            .write_all(format!("CONNECT {target} HTTP/1.1\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).await.unwrap();
        line
    }

    /// A host no rule decides is held and put to the asker. Allowing it lets
    /// the same connection through: the client sees no refusal, and nothing
    /// is recorded as denied, so the command is not treated as blocked.
    #[tokio::test]
    async fn an_undecided_host_is_held_until_the_asker_allows_it() {
        let Some(proxy) = started_proxy(EgressPolicy::new()).await else {
            return;
        };
        let (asker, mut asks) = tokio::sync::mpsc::unbounded_channel();
        proxy.shared().set_asker(Some(asker));
        let shared = proxy.shared().clone();
        let decide = tokio::spawn(async move {
            let ask: HostAsk = asks.recv().await.unwrap();
            assert_eq!((ask.host.as_str(), ask.port), ("held.invalid", 443));
            shared.grant_host(&ask.host);
            ask.reply.send(HostAnswer::Allow).unwrap();
        });

        let status = connect_line(proxy.addr().port(), "Held.invalid:443").await;
        decide.await.unwrap();
        assert!(!status.contains("403"), "{status}");
        assert_eq!(proxy.shared().take_denied_host(), None);
        // Granted now: the next connection is not asked about.
        let status = connect_line(proxy.addr().port(), "held.invalid:443").await;
        assert!(!status.contains("403"), "{status}");
    }

    /// "Once" covers the invocation that asked and nothing after it.
    #[tokio::test]
    async fn allowing_once_does_not_change_the_policy() {
        let Some(proxy) = started_proxy(EgressPolicy::new()).await else {
            return;
        };
        let (asker, mut asks) = tokio::sync::mpsc::unbounded_channel::<HostAsk>();
        proxy.shared().set_asker(Some(asker));
        let answers = tokio::spawn(async move {
            let mut seen = 0;
            while let Some(ask) = asks.recv().await {
                seen += 1;
                let _ = ask.reply.send(HostAnswer::AllowOnce);
            }
            seen
        });

        let first = proxy.shared().for_invocation(Some("call-1"));
        assert!(first.admit("once.invalid", 443).await);
        assert!(
            first.admit("once.invalid", 443).await,
            "same invocation, no second ask"
        );
        let second = proxy.shared().for_invocation(Some("call-2"));
        assert!(second.admit("once.invalid", 443).await);
        assert!(!proxy.shared().permits_host("once.invalid"));

        proxy.shared().set_asker(None);
        assert_eq!(answers.await.unwrap(), 2, "one ask per invocation");
    }

    /// A missing asker and an unanswered ask end as before: 403, with the
    /// host recorded for the approval that follows. A refusal by the user is
    /// a 403 too, and final.
    #[tokio::test]
    async fn a_host_that_is_not_allowed_in_time_is_refused_and_recorded() {
        let options = EgressOptions {
            hold_timeout: Duration::from_millis(50),
            ..Default::default()
        };
        let shared = EgressShared::new(EgressPolicy::new(), options);
        let Ok(proxy) = EgressProxy::start_with_shared(shared).await else {
            return;
        };
        let port = proxy.addr().port();

        // Nobody to ask.
        assert!(connect_line(port, "a.invalid:443").await.contains("403"));
        assert_eq!(
            proxy.shared().take_denied_host().as_deref(),
            Some("a.invalid")
        );

        // Asked, refused.
        let (asker, mut asks) = tokio::sync::mpsc::unbounded_channel::<HostAsk>();
        proxy.shared().set_asker(Some(asker));
        let refuse = tokio::spawn(async move {
            let ask = asks.recv().await.unwrap();
            ask.reply.send(HostAnswer::Deny).unwrap();
            // The next ask is left unanswered until it times out.
            let ask = asks.recv().await.unwrap();
            tokio::time::sleep(Duration::from_millis(200)).await;
            drop(ask);
        });
        // A refusal by the user is not queued for an approval: they have
        // answered. An ask nobody answered is, as before.
        assert!(connect_line(port, "b.invalid:443").await.contains("403"));
        assert!(proxy.shared().denied_hosts().is_empty());
        assert_eq!(proxy.shared().take_user_refused_hosts(), ["b.invalid"]);
        assert!(connect_line(port, "c.invalid:443").await.contains("403"));
        assert_eq!(proxy.shared().denied_hosts(), ["c.invalid"]);
        refuse.await.unwrap();
    }

    /// A deny rule is a decision already made; it is not put to the user.
    /// A write under the user's identity goes through when its target (or
    /// its whole host) is recorded; otherwise it is held and asked about,
    /// and with nobody to ask it is refused.
    #[tokio::test]
    async fn a_write_is_allowed_by_its_record_or_by_the_user() {
        let write = |target: &str| crate::write_gate::Write {
            target: target.into(),
            summary: "git push".into(),
        };
        let shared = EgressShared::new(EgressPolicy::new(), EgressOptions::default());
        assert_eq!(
            shared.admit_write(&write("github.com/o/r")).await,
            Admission::Refused
        );

        shared.grant_write("GitHub.com/O/R");
        assert!(shared.admit_write(&write("github.com/o/r")).await.allowed());
        assert!(!shared
            .admit_write(&write("github.com/o/other"))
            .await
            .allowed());
        shared.grant_write("gitlab.example");
        assert!(shared
            .admit_write(&write("gitlab.example/g/p"))
            .await
            .allowed());

        let (asker, mut asks) = tokio::sync::mpsc::unbounded_channel::<HostAsk>();
        shared.set_asker(Some(asker));
        let invocation = shared.for_invocation(Some("call-1"));
        let answering = tokio::spawn(async move {
            let ask = asks.recv().await.unwrap();
            assert_eq!(ask.host, "github.com/o/other");
            assert_eq!(ask.write.as_deref(), Some("git push"));
            let _ = ask.reply.send(HostAnswer::AllowOnce);
            let ask = asks.recv().await.unwrap();
            let _ = ask.reply.send(HostAnswer::Deny);
        });
        assert!(invocation
            .admit_write(&write("github.com/o/other"))
            .await
            .allowed());
        // Once: this invocation again without asking, but not the policy.
        assert!(invocation
            .admit_write(&write("github.com/o/other"))
            .await
            .allowed());
        assert!(!shared.permits_write("github.com/o/other"));
        assert_eq!(
            invocation.admit_write(&write("github.com/o/third")).await,
            Admission::RefusedByUser
        );
        answering.await.unwrap();
    }

    #[test]
    fn the_builtin_list_allows_code_hosts_and_registries_and_a_deny_still_wins() {
        let policy = EgressPolicy::new().with_builtin_hosts();
        for host in [
            "github.com",
            "api.github.com",
            "objects.githubusercontent.com",
            "registry.npmjs.org",
            "crates.io",
            "pypi.org",
        ] {
            assert!(policy.permits(host), "{host}");
        }
        for host in [
            "example.com",
            "github.com.evil.test",
            "npmjs.org",
            "pastebin.com",
        ] {
            assert!(!policy.permits(host), "{host}");
        }
        let file = forge_config::PermissionsFile {
            deny: vec!["host(**.github.com)".into()],
            ..Default::default()
        };
        let policy = EgressPolicy::from_permissions(&file).with_builtin_hosts();
        assert!(!policy.permits("github.com") && policy.permits("crates.io"));

        // Code hosts are inspected; registries and look-alikes are not.
        assert!(inspected("github.com") && inspected("API.github.com"));
        assert!(!inspected("githubusercontent.com") && !inspected("notgithub.com"));
        assert!(is_builtin_registry("crates.io") && is_builtin_registry("githubusercontent.com"));
        assert!(!is_builtin_registry("github.com"));
    }

    #[test]
    fn remote_write_rules_come_from_the_permission_file() {
        let file = forge_config::PermissionsFile {
            allow: vec![
                "host(github.com)".into(),
                "remote_write(github.com/o/r)".into(),
            ],
            ..Default::default()
        };
        let policy = EgressPolicy::from_permissions(&file);
        assert!(policy.permits("github.com"));
        assert!(policy.permits_write("github.com/o/r"));
        assert!(!policy.permits_write("github.com/o/x"));
        assert!(!policy.permits_write("github.com"));
    }

    #[tokio::test]
    async fn a_denied_host_is_refused_without_asking() {
        let mut policy = EgressPolicy::new();
        policy.deny("blocked.invalid");
        let shared = EgressShared::new(policy, EgressOptions::default());
        let (asker, mut asks) = tokio::sync::mpsc::unbounded_channel::<HostAsk>();
        shared.set_asker(Some(asker));
        assert!(!shared.admit("blocked.invalid", 443).await);
        assert!(
            !shared.admit("..", 443).await,
            "a malformed name is not asked about"
        );
        assert!(asks.try_recv().is_err());
    }

    #[tokio::test]
    async fn denied_hosts_are_isolated_between_invocations() {
        let Some(proxy) = started_proxy(EgressPolicy::new()).await else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let template = crate::sandbox::EgressGrant {
            proxy_port: proxy.addr().port(),
            socket_path: dir.path().join("session.sock"),
            control: Some(proxy.shared().clone()),
        };
        let first = EgressInvocation::start(Some(&template))
            .await
            .unwrap()
            .unwrap();
        let second = EgressInvocation::start(Some(&template))
            .await
            .unwrap()
            .unwrap();
        let first_socket = first.grant().unwrap().socket_path.clone();

        let status = connect_status(first.grant().unwrap().proxy_port, "blocked.example:443").await;
        assert!(
            status.contains("403"),
            "the host should be refused: {status}"
        );
        assert_eq!(first.take_denied_host().as_deref(), Some("blocked.example"));
        assert_eq!(second.take_denied_host(), None);
        assert_eq!(template.take_denied_host(), None);

        template.control.as_ref().unwrap().grant_host("**.example");
        assert!(first.grant().unwrap().permits_host("blocked.example"));
        assert!(second.grant().unwrap().permits_host("blocked.example"));

        drop(first);
        assert!(
            !first_socket.exists(),
            "an invocation must remove its socket when the command ends"
        );
    }
}
