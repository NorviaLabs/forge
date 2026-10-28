//! The `git` tool's network operations under the session host allowlist.
//!
//! The `git` tool runs host `git` directly, not in the sandbox, because it
//! relies on the user's own credential helpers, SSH keys and config. Two
//! layers hold it to the same session policy a confined shell is held to:
//!
//! 1. A fast pre-check ([`check_git_network`]): before spawning a subcommand
//!    that contacts a remote, the remote's URL is resolved locally (config
//!    reads only, never the network), its host extracted, and checked against
//!    the policy. A host the policy does not permit is reported as
//!    [`ToolError::SandboxDenied`] with `denied_host`, which is what raises the
//!    existing "allow this host" prompt; the approved retry re-runs the same
//!    call against the updated policy. Resolution fails closed: a remote whose
//!    URL cannot be determined, or whose transport this module does not
//!    understand, is refused rather than guessed.
//! 2. Enforcement ([`route_through_gateway`]): the network subcommand itself
//!    runs with its HTTP(S), SSH and `git://` traffic pointed at the call's
//!    egress gateway, so a host the pre-check could not see (a redirect, a
//!    submodule, a hook, an ssh alias) is still refused by the gateway. The
//!    gateway only tunnels: TLS and SSH authentication stay end to end with
//!    the user's own credentials.

use std::path::Path;
use std::process::{Output, Stdio};
use std::time::Duration;

use tokio::process::Command;

use crate::egress::CallGateway;
use crate::registry::ToolContext;
use crate::ToolError;

/// Deadline for subcommands that contact a remote.
pub(crate) const GIT_NETWORK_TIMEOUT: Duration = Duration::from_secs(120);
/// Deadline for subcommands that may run repository hooks (a pre-commit hook
/// commonly runs a linter or test suite, which legitimately takes minutes).
pub(crate) const GIT_HOOK_TIMEOUT: Duration = Duration::from_secs(300);
/// Deadline for purely local subcommands, and for the config reads made here.
pub(crate) const GIT_LOCAL_TIMEOUT: Duration = Duration::from_secs(30);

/// Subcommands that run hooks. `push` and `pull` also contact a remote; the
/// longer of the two deadlines applies.
const GIT_HOOK_SUBCOMMANDS: &[&str] = &["commit", "merge", "rebase", "cherry-pick", "push", "pull"];

/// `git remote` verbs that only read or edit local config. Every other verb is
/// either network (`show`, `prune`, `update`) or unknown and refused.
const GIT_REMOTE_LOCAL_VERBS: &[&str] = &[
    "add",
    "rename",
    "remove",
    "rm",
    "get-url",
    "set-url",
    "set-branches",
    "set-head",
];

/// Where a remote URL points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RemoteLocation {
    /// A path or `file://` URL on this machine: no network.
    Local,
    /// A network host, lowercased, without userinfo or port.
    Host(String),
}

/// The deadline for one invocation of `subcommand`.
pub(crate) fn git_timeout(subcommand: &str, args: &[String]) -> Duration {
    if GIT_HOOK_SUBCOMMANDS.contains(&subcommand) {
        GIT_HOOK_TIMEOUT
    } else if is_network_invocation(subcommand, args) {
        GIT_NETWORK_TIMEOUT
    } else {
        GIT_LOCAL_TIMEOUT
    }
}

pub(crate) fn is_network_invocation(subcommand: &str, operands: &[String]) -> bool {
    match subcommand {
        "fetch" | "pull" | "push" | "clone" => true,
        "remote" => operands
            .iter()
            .find(|token| !token.starts_with('-'))
            .is_some_and(|verb| !GIT_REMOTE_LOCAL_VERBS.contains(&verb.as_str())),
        _ => false,
    }
}

/// Extract where `url` points, following git's own reading of a repository
/// argument (`transport.c`, `connect.c`).
///
/// * `<transport>::<address>` names a remote helper — refused, since the
///   helper decides what it contacts.
/// * `<scheme>://…` — `file` is local; the well-known network schemes yield the
///   authority's host; anything else is a helper and refused.
/// * `[user@]host:path` (a colon before any slash) is SSH's scp-like form.
/// * Anything else is a local path.
pub(crate) fn remote_location(url: &str) -> Result<RemoteLocation, String> {
    let url = url.trim();
    if url.is_empty() {
        return Err("the remote URL is empty".into());
    }
    let helper_name_len = url
        .bytes()
        .take_while(|b| b.is_ascii_alphanumeric() || b"-._~".contains(b))
        .count();
    if url[helper_name_len..].starts_with("::") {
        return Err(format!(
            "`{}::` remote helpers are not supported by the git tool",
            &url[..helper_name_len]
        ));
    }
    if let Some((scheme, rest)) = url.split_once("://") {
        let scheme = scheme.to_ascii_lowercase();
        return match scheme.as_str() {
            "file" => Ok(RemoteLocation::Local),
            "http" | "https" | "ssh" | "git" | "git+ssh" | "ssh+git" | "ftp" | "ftps" => {
                let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
                host_from_authority(authority).map(RemoteLocation::Host)
            }
            _ => Err(format!(
                "`{scheme}://` remotes are not supported by the git tool"
            )),
        };
    }
    let Some(colon) = url.find(':') else {
        return Ok(RemoteLocation::Local);
    };
    if url.find('/').is_some_and(|slash| slash < colon) {
        return Ok(RemoteLocation::Local);
    }
    // scp-like. A bracketed host may itself contain colons (`[::1]:repo`,
    // `[host:22]:repo`), so the host ends at `]` rather than the first colon.
    let host = match url.find('[') {
        Some(open) if open <= colon => {
            let close = url[open..]
                .find(']')
                .map(|i| open + i)
                .ok_or_else(|| "unterminated `[` in the remote URL".to_string())?;
            if !url[close + 1..].starts_with(':') {
                return Err("malformed scp-like remote URL".into());
            }
            let inner = &url[open + 1..close];
            let inner = inner.rsplit_once('@').map_or(inner, |(_, host)| host);
            // `[host:port]` carries a port; a bare IPv6 literal has several colons.
            match inner.split_once(':') {
                Some((host, port)) if !port.contains(':') => host.to_string(),
                _ => inner.to_string(),
            }
        }
        _ => {
            let authority = &url[..colon];
            authority
                .rsplit_once('@')
                .map_or(authority, |(_, host)| host)
                .to_string()
        }
    };
    normalize_host(&host).map(RemoteLocation::Host)
}

/// The host of a URL authority (`user:pass@host:port`, `[v6]:port`).
fn host_from_authority(authority: &str) -> Result<String, String> {
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.split_once(']')
            .map(|(host, _)| host)
            .ok_or_else(|| "unterminated `[` in the remote URL".to_string())?
    } else {
        authority.split(':').next().unwrap_or_default()
    };
    normalize_host(host)
}

fn normalize_host(host: &str) -> Result<String, String> {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() {
        return Err("the remote URL has no host".into());
    }
    Ok(host)
}

/// Refuse `subcommand` when a remote it would contact is outside the session's
/// host allowlist. Runs before git is spawned; only local config is read.
pub(crate) async fn check_git_network(
    ctx: &ToolContext,
    subcommand: &str,
    operands: &[String],
    options: &[String],
) -> Result<(), ToolError> {
    // No egress grant means the session has no sandbox network policy at all
    // (sandbox unavailable, or governance disabled), so there is no allowlist
    // to hold git to. Host git keeps its previous unrestricted behaviour there
    // rather than being cut off from the network entirely.
    let Some(egress) = ctx.egress.as_deref() else {
        return Ok(());
    };
    if !is_network_invocation(subcommand, operands) {
        return Ok(());
    }
    let repo = LocalRepo {
        root: &ctx.workspace_root,
        credential_env_names: &ctx.credential_env_names,
    };
    let targets = remote_targets(&repo, subcommand, operands, options).await?;
    for target in targets {
        let location = remote_location(&target.url).map_err(|why| {
            ToolError::Execution(format!(
                "git {subcommand}: refusing to contact {}: {why}",
                target.label
            ))
        })?;
        if let RemoteLocation::Host(host) = location {
            if !egress.permits_host(&host) {
                return Err(ToolError::SandboxDenied {
                    content: format!(
                        "git {subcommand}: network access to {host} ({}) is not allowed",
                        target.label
                    ),
                    reason: crate::egress::HOST_DENIED_EXPLANATION.to_string(),
                    denied_host: Some(host),
                });
            }
        }
    }
    Ok(())
}

/// The extra `-c` settings and environment that point one git invocation at
/// the per-call gateway.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct GatewayRouting {
    /// `-c key=value` pairs, passed before the subcommand. Git exports them to
    /// its children (`GIT_CONFIG_PARAMETERS`), so submodule fetches and remote
    /// helpers see them too.
    pub(crate) config: Vec<(String, String)>,
    pub(crate) env: Vec<(&'static str, String)>,
    pub(crate) env_remove: Vec<&'static str>,
}

/// The parts of the user's own git and ssh setup that routing builds on,
/// read before git is spawned.
#[derive(Debug, Default)]
pub(crate) struct UserGitSetup {
    /// `GIT_SSH_COMMAND` from the environment.
    pub(crate) git_ssh_command: Option<String>,
    /// The effective `core.sshCommand`.
    pub(crate) core_ssh_command: Option<String>,
    /// `GIT_SSH` from the environment: a program path, not a shell command.
    pub(crate) git_ssh: Option<String>,
    /// `GIT_SSH_VARIANT`, else `ssh.variant`.
    pub(crate) ssh_variant: Option<String>,
    /// Every configured `http.<url>.proxy` and `remote.<name>.proxy` key.
    pub(crate) proxy_keys: Vec<String>,
}

/// Where the gateway listens and how to reach it from a child process.
#[derive(Debug)]
pub(crate) struct GatewayEndpoint<'a> {
    /// `http://127.0.0.1:PORT`.
    pub(crate) proxy_url: &'a str,
    /// `127.0.0.1:PORT`, as `__egress-connect` takes it.
    pub(crate) connect_addr: &'a str,
    /// The forge executable that provides `__egress-connect`.
    pub(crate) exe: &'a str,
    /// A program git can run as `GIT_PROXY_COMMAND <host> <port>`.
    pub(crate) git_proxy_command: &'a str,
}

/// Environment variables curl-based clients (git's own `http.proxy`
/// fallback, git-lfs, credential helpers) read for their proxy.
const PROXY_ENV: &[&str] = &[
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
];

/// Short options OpenSSH takes without an argument, for reading a cluster
/// such as `-vJ bastion` the way `getopt` does.
const SSH_FLAGS_WITHOUT_ARGUMENT: &str = "46AaCfGgKkMNnqsTtVvXxYy";

/// Build the routing for one invocation. An `Err` refuses the call outright;
/// an SSH setup that cannot be pointed at the gateway is instead refused
/// lazily (see [`ssh_refusal`]), so it only fails a call that really uses SSH.
pub(crate) fn plan_routing(
    setup: &UserGitSetup,
    endpoint: &GatewayEndpoint<'_>,
) -> Result<GatewayRouting, String> {
    let mut routing = GatewayRouting::default();

    // HTTP(S). `http.proxy` is the least specific of git's proxy settings:
    // a matching `http.<url>.proxy` (urlmatch: the more specific URL wins
    // whatever the order) and `remote.<name>.proxy` both take precedence over
    // it. Each such key the user configured is overridden by name; for equal
    // keys the last value wins, and `-c` is read after every config file and
    // after `GIT_CONFIG_COUNT`.
    routing
        .config
        .push(("http.proxy".into(), endpoint.proxy_url.into()));
    for key in &setup.proxy_keys {
        if key.contains('=') {
            return Err(format!(
                "cannot override the proxy setting `{key}` (it contains `=`); \
                 refusing to contact the remote around the egress gateway"
            ));
        }
        if !routing.config.iter().any(|(k, _)| k == key) {
            routing
                .config
                .push((key.clone(), endpoint.proxy_url.into()));
        }
    }
    for name in PROXY_ENV {
        routing.env.push((name, endpoint.proxy_url.into()));
    }
    // Removed rather than emptied: no host may skip the gateway.
    routing.env_remove.extend(["NO_PROXY", "no_proxy"]);

    // SSH. `GIT_SSH_COMMAND` outranks every other ssh setting git reads, so
    // setting it is enough to be authoritative.
    match compose_ssh_command(setup, endpoint) {
        Ok(command) => routing.env.push(("GIT_SSH_COMMAND", command)),
        Err(why) => {
            routing.env.push(("GIT_SSH_COMMAND", ssh_refusal(&why)));
            // Without an explicit variant git would probe the refusal with
            // `-G` and could die on a port before printing it.
            routing.env.push(("GIT_SSH_VARIANT", "ssh".into()));
        }
    }

    // git:// (port 9418). Git runs `GIT_PROXY_COMMAND <host> <port>` without
    // a shell and lets the environment outrank `core.gitProxy`.
    routing
        .env
        .push(("GIT_PROXY_COMMAND", endpoint.git_proxy_command.into()));
    Ok(routing)
}

/// The user's ssh command with the gateway as its `ProxyCommand`, or why the
/// setup cannot be routed.
///
/// OpenSSH keeps the first value it obtains for each option, and the command
/// line is read before any config file. So the appended `-o ProxyCommand`
/// overrides a `ProxyCommand` or `ProxyJump` in `~/.ssh/config` (the two
/// compete, and the first one seen wins), but not one already on the user's
/// own command line — that case is refused. `%h` expands to the resolved
/// `HostName`, so an ssh alias is checked by the gateway against the real host
/// it names. `ControlPath=none` keeps ssh from multiplexing over a master
/// connection opened outside the gateway.
fn compose_ssh_command(
    setup: &UserGitSetup,
    endpoint: &GatewayEndpoint<'_>,
) -> Result<String, String> {
    let variant = setup
        .ssh_variant
        .as_deref()
        .map(|v| v.trim().to_ascii_lowercase());
    if let Some(variant) = variant.as_deref() {
        if !matches!(variant, "auto" | "ssh") {
            return Err(format!(
                "the ssh variant is `{variant}`; only an OpenSSH-compatible ssh can be routed \
                 through the egress gateway"
            ));
        }
    }
    let user_command = setup
        .git_ssh_command
        .as_deref()
        .map(|source| ("GIT_SSH_COMMAND", source))
        .or_else(|| {
            setup
                .core_ssh_command
                .as_deref()
                .map(|source| ("core.sshCommand", source))
        })
        .filter(|(_, command)| !command.trim().is_empty());
    let base = match user_command {
        Some((source, command)) => {
            if sets_its_own_route(command) {
                return Err(format!(
                    "{source} sets its own ProxyCommand, ProxyJump or ControlPath, which would \
                     take precedence over the egress gateway"
                ));
            }
            command.trim().to_string()
        }
        None => match setup.git_ssh.as_deref().filter(|p| !p.trim().is_empty()) {
            Some(program) => {
                let name = Path::new(program)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default();
                if !(matches!(name, "ssh" | "ssh.exe") || variant.as_deref() == Some("ssh")) {
                    return Err(format!(
                        "GIT_SSH is `{program}`, which cannot be given a ProxyCommand; set \
                         GIT_SSH_COMMAND or core.sshCommand to an OpenSSH command instead"
                    ));
                }
                shell_quote(program)
            }
            None => "ssh".to_string(),
        },
    };
    // `%` starts an ssh token in ProxyCommand; the command itself is run by
    // ssh through the shell, and the whole option again by git's shell.
    let proxy_command = format!(
        "{} __egress-connect {} %h %p",
        shell_quote(&endpoint.exe.replace('%', "%%")),
        endpoint.connect_addr
    );
    Ok(format!(
        "{base} -o ControlPath=none -o {}",
        shell_quote(&format!("ProxyCommand={proxy_command}"))
    ))
}

/// Whether a user ssh command already chooses its own route, which OpenSSH
/// would prefer over the gateway's (first value wins).
fn sets_its_own_route(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    if ["proxycommand", "proxyjump", "controlpath"]
        .iter()
        .any(|option| lower.contains(option))
    {
        return true;
    }
    command.split_whitespace().any(|token| {
        let Some(cluster) = token.strip_prefix('-').filter(|c| !c.starts_with('-')) else {
            return false;
        };
        for flag in cluster.chars() {
            if flag == 'J' || flag == 'S' {
                return true;
            }
            if !SSH_FLAGS_WITHOUT_ARGUMENT.contains(flag) {
                // The rest of the token is this option's argument.
                return false;
            }
        }
        false
    })
}

/// An ssh "command" that fails with `why`, so an SSH remote fails closed with
/// a readable message while an HTTPS remote in the same call is unaffected.
fn ssh_refusal(why: &str) -> String {
    format!(
        "printf '%s\\n' {} >&2; false",
        shell_quote(&format!("forge: refusing git over SSH: {why}"))
    )
}

/// POSIX single-quoting.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// A git invocation's routing, and the scratch files it needs while git runs.
#[derive(Debug)]
pub(crate) struct RoutedGit {
    routing: GatewayRouting,
    _scratch: Option<tempfile::TempDir>,
}

impl RoutedGit {
    /// Add the routing to `cmd`. Call before the subcommand is added: `-c`
    /// is a global option.
    pub(crate) fn apply(&self, cmd: &mut Command) {
        for (key, value) in &self.routing.config {
            cmd.arg("-c").arg(format!("{key}={value}"));
        }
        for name in &self.routing.env_remove {
            cmd.env_remove(name);
        }
        for (name, value) in &self.routing.env {
            cmd.env(name, value);
        }
    }
}

/// Point a network subcommand's traffic at `gateway`. Fails closed: when the
/// routing cannot be built, the call is refused rather than run direct.
pub(crate) async fn route_through_gateway(
    ctx: &ToolContext,
    subcommand: &str,
    gateway: &CallGateway,
) -> Result<RoutedGit, ToolError> {
    let refuse = |why: String| {
        ToolError::Execution(format!(
            "git {subcommand}: {why}; refusing to contact the remote"
        ))
    };
    let repo = LocalRepo {
        root: &ctx.workspace_root,
        credential_env_names: &ctx.credential_env_names,
    };
    // Clone reads no repository config, so the workspace's own settings
    // neither feed nor change its ssh command.
    let include_repo = subcommand != "clone";
    let setup = UserGitSetup {
        git_ssh_command: env_value("GIT_SSH_COMMAND"),
        core_ssh_command: repo
            .config_all_scoped("core.sshCommand", include_repo)
            .await?
            .pop(),
        git_ssh: env_value("GIT_SSH"),
        ssh_variant: match env_value("GIT_SSH_VARIANT") {
            Some(variant) => Some(variant),
            None => repo
                .config_all_scoped("ssh.variant", include_repo)
                .await?
                .pop(),
        },
        proxy_keys: repo.proxy_keys().await?,
    };
    let exe = match EGRESS_CONNECT_EXE.get() {
        Some(exe) => exe.clone(),
        None => std::env::current_exe()
            .map_err(|e| refuse(format!("cannot locate the forge executable: {e}")))?,
    };
    let exe = exe
        .to_str()
        .ok_or_else(|| refuse("the forge executable path is not UTF-8".into()))?
        .to_string();
    let connect_addr = gateway.addr().to_string();
    let (scratch, git_proxy_command) = git_proxy_command(&exe, &connect_addr)
        .map_err(|e| refuse(format!("cannot prepare the git:// proxy command: {e}")))?;
    let routing = plan_routing(
        &setup,
        &GatewayEndpoint {
            proxy_url: &gateway.proxy_url(),
            connect_addr: &connect_addr,
            exe: &exe,
            git_proxy_command: &git_proxy_command,
        },
    )
    .map_err(refuse)?;
    Ok(RoutedGit {
        routing,
        _scratch: scratch,
    })
}

/// The executable that provides `__egress-connect`, when it is not the
/// running one. Only [`set_egress_connect_exe`] sets it.
static EGRESS_CONNECT_EXE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// Internal, for tests only: use `exe` instead of [`std::env::current_exe`]
/// as the `__egress-connect` helper in git's routing. In production the
/// running process is the forge binary and this is never called; a test
/// binary is not, so the forge-cli end-to-end tests point it at the real
/// `forge`. Not reachable from configuration, flags or the environment. The
/// first call wins for the life of the process.
#[doc(hidden)]
pub fn set_egress_connect_exe(exe: std::path::PathBuf) {
    let _ = EGRESS_CONNECT_EXE.set(exe);
}

fn env_value(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// `GIT_PROXY_COMMAND` is a program, run without a shell as
/// `<program> <host> <port>`, so the gateway address is supplied by a small
/// wrapper script that lives as long as the call.
#[cfg(unix)]
fn git_proxy_command(
    exe: &str,
    connect_addr: &str,
) -> std::io::Result<(Option<tempfile::TempDir>, String)> {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::Builder::new()
        .prefix("forge-git-proxy-")
        .tempdir()?;
    let path = dir.path().join("git-proxy");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\nexec {} __egress-connect {connect_addr} \"$1\" \"$2\"\n",
            shell_quote(exe)
        ),
    )?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
    let path = path
        .to_str()
        .ok_or_else(|| std::io::Error::other("temporary path is not UTF-8"))?
        .to_string();
    Ok((Some(dir), path))
}

/// Without a wrapper script git:// cannot be routed; a program that does not
/// exist makes git fail with "cannot start proxy" instead of going direct.
#[cfg(not(unix))]
fn git_proxy_command(
    _exe: &str,
    _connect_addr: &str,
) -> std::io::Result<(Option<tempfile::TempDir>, String)> {
    Ok((None, "forge-git-protocol-is-not-routed".into()))
}

/// One URL a subcommand will contact, with a label that names its origin
/// without echoing the URL (which may embed credentials).
#[derive(Debug)]
struct RemoteTarget {
    label: String,
    url: String,
}

async fn remote_targets(
    repo: &LocalRepo<'_>,
    subcommand: &str,
    operands: &[String],
    options: &[String],
) -> Result<Vec<RemoteTarget>, ToolError> {
    let mut targets = Vec::new();
    match subcommand {
        "clone" => {
            if let Some(source) = operands.first() {
                // Clone reads no repository config, so a rule in the
                // workspace's own `.git/config` must not rewrite its source.
                let url = repo.rewrite_url(source, false, false).await?;
                targets.push(RemoteTarget {
                    label: "the clone source".into(),
                    url,
                });
            }
        }
        "fetch" | "pull" => {
            let all = options.iter().any(|o| o == "--all")
                || (operands.is_empty() && repo.config_bool("fetch.all").await? == Some(true));
            if all {
                for name in repo.remotes().await? {
                    repo.push_fetch_repository(&name, &mut targets).await?;
                }
            } else if let Some(repository) = operands.first() {
                repo.push_fetch_repository(repository, &mut targets).await?;
            } else {
                targets.push(repo.default_fetch_target().await?);
            }
            let recurse_off = options.iter().any(|o| o == "--no-recurse-submodules")
                || repo.config_bool("fetch.recurseSubmodules").await? == Some(false);
            if !recurse_off {
                repo.push_submodule_urls(&mut targets).await?;
            }
        }
        "push" => {
            let name = match operands.first() {
                Some(repository) => repository.clone(),
                None => repo.default_push_remote().await?,
            };
            repo.push_push_repository(&name, &mut targets).await?;
            let recurse = repo.config_get("push.recurseSubmodules").await?;
            let on_demand = match recurse.as_deref() {
                Some(value) => value.eq_ignore_ascii_case("on-demand"),
                None => repo.config_bool("submodule.recurse").await? == Some(true),
            };
            if on_demand {
                repo.push_submodule_urls(&mut targets).await?;
            }
        }
        "remote" => {
            let mut words = operands.iter();
            let verb = words.next().map(String::as_str).unwrap_or_default();
            let names: Vec<&String> = words.collect();
            match verb {
                "show" | "prune" => {
                    for name in names {
                        repo.push_fetch_repository(name, &mut targets).await?;
                    }
                }
                "update" => {
                    if names.is_empty() {
                        match repo.config_get("remotes.default").await? {
                            Some(_) => repo.push_fetch_repository("default", &mut targets).await?,
                            None => {
                                for name in repo.remotes().await? {
                                    repo.push_fetch_repository(&name, &mut targets).await?;
                                }
                            }
                        }
                    } else {
                        for name in names {
                            repo.push_fetch_repository(name, &mut targets).await?;
                        }
                    }
                }
                other => {
                    return Err(ToolError::Execution(format!(
                        "git remote: cannot tell which host `remote {other}` contacts; refusing"
                    )));
                }
            }
        }
        _ => {}
    }
    Ok(targets)
}

/// Local, network-free reads of the workspace repository's configuration.
struct LocalRepo<'a> {
    root: &'a Path,
    /// The session's custom key env names, stripped like the fixed ones.
    credential_env_names: &'a [String],
}

impl LocalRepo<'_> {
    async fn git(&self, args: &[&str]) -> Result<Output, ToolError> {
        let mut cmd = Command::new("git");
        crate::builtins::strip_credentials(cmd.as_std_mut(), self.credential_env_names);
        cmd.args(args)
            .current_dir(self.root)
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        match run_git_bounded(cmd, GIT_LOCAL_TIMEOUT).await? {
            GitRun::Finished(output) => Ok(output),
            GitRun::TimedOut => Err(ToolError::Execution(format!(
                "git: timed out reading local config (`git {}`); refusing to contact the remote",
                args.join(" ")
            ))),
        }
    }

    /// Stdout of a lookup that may legitimately find nothing (exit 1), as
    /// `None`. Any other failure is an error: the answer is unknown, and an
    /// unknown remote must not be treated as a local one.
    async fn lookup(&self, args: &[&str]) -> Result<Option<String>, ToolError> {
        let output = self.git(args).await?;
        match output.status.code() {
            Some(0) => Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned())),
            Some(1) => Ok(None),
            _ => Err(ToolError::Execution(format!(
                "git: could not read local config (`git {}`): {}; refusing to contact the remote",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            ))),
        }
    }

    async fn config_get(&self, key: &str) -> Result<Option<String>, ToolError> {
        Ok(self
            .lookup(&["config", "--get", key])
            .await?
            .map(|value| value.trim().to_string()))
    }

    async fn config_bool(&self, key: &str) -> Result<Option<bool>, ToolError> {
        // `--type=bool` would reject non-boolean values such as
        // `fetch.recurseSubmodules=on-demand`; anything that is not plainly
        // false counts as on, which over-approximates the hosts checked.
        Ok(self.config_get(key).await?.map(|value| {
            !matches!(
                value.to_ascii_lowercase().as_str(),
                "false" | "no" | "off" | "0"
            )
        }))
    }

    async fn config_all(&self, key: &str) -> Result<Vec<String>, ToolError> {
        Ok(self
            .lookup(&["config", "--get-all", key])
            .await?
            .map(|value| value.lines().map(str::to_string).collect())
            .unwrap_or_default())
    }

    /// Every value of `key`, oldest first, so the last one is git's. With
    /// `include_repo` false the repository's own scopes are skipped.
    async fn config_all_scoped(
        &self,
        key: &str,
        include_repo: bool,
    ) -> Result<Vec<String>, ToolError> {
        let listing = self
            .lookup(&["config", "--show-scope", "-z", "--get-all", key])
            .await?
            .unwrap_or_default();
        let mut tokens = listing.split('\0');
        let mut values = Vec::new();
        while let (Some(scope), Some(value)) = (tokens.next(), tokens.next()) {
            if include_repo || !matches!(scope, "local" | "worktree") {
                values.push(value.to_string());
            }
        }
        Ok(values)
    }

    /// Names of the per-URL and per-remote proxy settings, which outrank
    /// `http.proxy`.
    async fn proxy_keys(&self) -> Result<Vec<String>, ToolError> {
        Ok(self
            .lookup(&[
                "config",
                "-z",
                "--name-only",
                "--get-regexp",
                r"^(http\..+|remote\..+)\.proxy$",
            ])
            .await?
            .unwrap_or_default()
            .split('\0')
            .filter(|key| !key.is_empty())
            .map(str::to_string)
            .collect())
    }

    async fn remotes(&self) -> Result<Vec<String>, ToolError> {
        // Outside a repository `git remote` fails; there are then no remotes.
        let output = self.git(&["remote"]).await?;
        if !output.status.success() {
            return Ok(Vec::new());
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .collect())
    }

    async fn current_branch(&self) -> Result<Option<String>, ToolError> {
        let output = self
            .git(&["symbolic-ref", "--quiet", "--short", "HEAD"])
            .await?;
        Ok(output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
            .filter(|branch| !branch.is_empty()))
    }

    /// `git fetch <repository>`: a remote group, a configured remote, or a URL.
    /// Fetch consults `remotes.<name>` first, so a group wins over a remote.
    async fn push_fetch_repository(
        &self,
        repository: &str,
        targets: &mut Vec<RemoteTarget>,
    ) -> Result<(), ToolError> {
        let members: Vec<String> = self
            .config_all(&format!("remotes.{repository}"))
            .await?
            .iter()
            .flat_map(|line| line.split_whitespace().map(str::to_string))
            .collect();
        if members.is_empty() {
            targets.push(self.fetch_target(repository).await?);
        } else {
            for member in members {
                targets.push(self.fetch_target(&member).await?);
            }
        }
        Ok(())
    }

    /// `ls-remote --get-url` resolves a remote name or URL exactly as fetch
    /// does — including `insteadOf` rewriting — without contacting it.
    async fn fetch_target(&self, repository: &str) -> Result<RemoteTarget, ToolError> {
        let label = self.label_for(repository).await?;
        let url = self
            .lookup(&["ls-remote", "--get-url", "--", repository])
            .await?
            .map(|url| url.trim().to_string())
            .filter(|url| !url.is_empty())
            .ok_or_else(|| undetermined(&label))?;
        Ok(RemoteTarget { label, url })
    }

    async fn default_fetch_target(&self) -> Result<RemoteTarget, ToolError> {
        let url = self
            .lookup(&["ls-remote", "--get-url"])
            .await?
            .map(|url| url.trim().to_string())
            .filter(|url| !url.is_empty())
            .ok_or_else(|| undetermined("the default remote"))?;
        Ok(RemoteTarget {
            label: "the default remote".into(),
            url,
        })
    }

    /// Mirrors git's `pushremote_for_branch`: the branch's `pushRemote`, then
    /// `remote.pushDefault`, then the branch's `remote`, then the only
    /// configured remote, then `origin`.
    async fn default_push_remote(&self) -> Result<String, ToolError> {
        let branch = self.current_branch().await?;
        if let Some(branch) = &branch {
            if let Some(name) = self
                .config_get(&format!("branch.{branch}.pushRemote"))
                .await?
            {
                return Ok(name);
            }
        }
        if let Some(name) = self.config_get("remote.pushDefault").await? {
            return Ok(name);
        }
        if let Some(branch) = &branch {
            if let Some(name) = self.config_get(&format!("branch.{branch}.remote")).await? {
                return Ok(name);
            }
        }
        let remotes = self.remotes().await?;
        Ok(match remotes.as_slice() {
            [only] => only.clone(),
            _ => "origin".to_string(),
        })
    }

    /// `git push <repository>`: every push URL of a configured remote, or the
    /// URL itself after `pushInsteadOf`/`insteadOf` rewriting.
    async fn push_push_repository(
        &self,
        repository: &str,
        targets: &mut Vec<RemoteTarget>,
    ) -> Result<(), ToolError> {
        if self.remotes().await?.iter().any(|name| name == repository) {
            let label = format!("remote `{repository}`");
            let urls = self
                .lookup(&["remote", "get-url", "--push", "--all", "--", repository])
                .await?
                .unwrap_or_default();
            let urls: Vec<&str> = urls
                .lines()
                .map(str::trim)
                .filter(|u| !u.is_empty())
                .collect();
            if urls.is_empty() {
                return Err(undetermined(&label));
            }
            for url in urls {
                targets.push(RemoteTarget {
                    label: label.clone(),
                    url: url.to_string(),
                });
            }
        } else {
            let url = self.rewrite_url(repository, true, true).await?;
            targets.push(RemoteTarget {
                label: "the push URL".into(),
                url,
            });
        }
        Ok(())
    }

    async fn label_for(&self, repository: &str) -> Result<String, ToolError> {
        Ok(
            if self.remotes().await?.iter().any(|name| name == repository) {
                format!("remote `{repository}`")
            } else {
                "the repository URL".into()
            },
        )
    }

    /// Absolute URLs of initialised submodules, which a recursive fetch or push
    /// contacts in addition to the superproject's remote. Relative URLs resolve
    /// against the superproject's remote, which is already checked.
    async fn push_submodule_urls(&self, targets: &mut Vec<RemoteTarget>) -> Result<(), ToolError> {
        let Some(listing) = self
            .lookup(&["config", "-z", "--get-regexp", r"^submodule\..*\.url$"])
            .await?
        else {
            return Ok(());
        };
        for entry in listing.split('\0').filter(|e| !e.is_empty()) {
            let Some((_, url)) = entry.split_once('\n') else {
                continue;
            };
            if url.starts_with("./") || url.starts_with("../") {
                continue;
            }
            targets.push(RemoteTarget {
                label: "a submodule".into(),
                url: url.to_string(),
            });
        }
        Ok(())
    }

    /// Apply `url.<base>.insteadOf` (and, for a push, `pushInsteadOf` first)
    /// to a raw URL the way git does: the longest matching prefix wins.
    /// `include_repo` is false for clone, which reads no repository config.
    async fn rewrite_url(
        &self,
        url: &str,
        push: bool,
        include_repo: bool,
    ) -> Result<String, ToolError> {
        let listing = self
            .lookup(&[
                "config",
                "--show-scope",
                "-z",
                "--get-regexp",
                r"^url\..*\.(push)?insteadof$",
            ])
            .await?
            .unwrap_or_default();
        let rules = parse_rewrite_rules(&listing, include_repo);
        if push {
            if let Some(rewritten) = longest_rewrite(&rules, url, true) {
                return Ok(rewritten);
            }
        }
        Ok(longest_rewrite(&rules, url, false).unwrap_or_else(|| url.to_string()))
    }
}

fn undetermined(label: &str) -> ToolError {
    ToolError::Execution(format!(
        "git: could not determine the URL of {label}; refusing to contact it"
    ))
}

#[derive(Debug, PartialEq, Eq)]
struct RewriteRule {
    push: bool,
    base: String,
    prefix: String,
}

/// Parse `git config --show-scope -z --get-regexp` output:
/// `scope\0key\nvalue\0` per entry.
fn parse_rewrite_rules(listing: &str, include_repo: bool) -> Vec<RewriteRule> {
    let mut rules = Vec::new();
    let mut tokens = listing.split('\0');
    while let (Some(scope), Some(entry)) = (tokens.next(), tokens.next()) {
        if !include_repo && matches!(scope, "local" | "worktree") {
            continue;
        }
        let Some((key, prefix)) = entry.split_once('\n') else {
            continue;
        };
        let Some(rest) = key.strip_prefix("url.") else {
            continue;
        };
        let lower = rest.to_ascii_lowercase();
        let (push, suffix_len) = if lower.ends_with(".pushinsteadof") {
            (true, ".pushinsteadof".len())
        } else if lower.ends_with(".insteadof") {
            (false, ".insteadof".len())
        } else {
            continue;
        };
        rules.push(RewriteRule {
            push,
            base: rest[..rest.len() - suffix_len].to_string(),
            prefix: prefix.to_string(),
        });
    }
    rules
}

fn longest_rewrite(rules: &[RewriteRule], url: &str, push: bool) -> Option<String> {
    rules
        .iter()
        .filter(|rule| rule.push == push && url.starts_with(&rule.prefix))
        .max_by_key(|rule| rule.prefix.len())
        .map(|rule| format!("{}{}", rule.base, &url[rule.prefix.len()..]))
}

/// How a bounded git run ended.
pub(crate) enum GitRun {
    Finished(Output),
    TimedOut,
}

/// Run `cmd` to completion or until `timeout`, whichever comes first. On
/// timeout the whole process group is killed — git forks `ssh`, credential
/// helpers and hooks, and killing only git would leave them running.
pub(crate) async fn run_git_bounded(
    mut cmd: Command,
    timeout: Duration,
) -> Result<GitRun, ToolError> {
    crate::builtins::set_process_group(&mut cmd);
    cmd.kill_on_drop(true);
    let child = cmd
        .spawn()
        .map_err(|e| ToolError::Execution(format!("failed to run git: {e}")))?;
    let mut guard =
        crate::builtins::ProcessGroupGuard::new(child.id().and_then(|pid| i32::try_from(pid).ok()));
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(output) => {
            // The leader has exited; leave anything it deliberately detached
            // (an SSH ControlMaster, say) alone, as before.
            guard.disarm();
            output
                .map(GitRun::Finished)
                .map_err(|e| ToolError::Execution(format!("failed to run git: {e}")))
        }
        Err(_) => {
            guard.kill();
            Ok(GitRun::TimedOut)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(url: &str) -> String {
        match remote_location(url) {
            Ok(RemoteLocation::Host(host)) => host,
            other => panic!("expected a host for `{url}`, got {other:?}"),
        }
    }

    #[test]
    fn https_hosts_drop_userinfo_port_and_case() {
        assert_eq!(host("https://github.com/o/r.git"), "github.com");
        assert_eq!(host("https://user:tok@GitHub.com:8443/o/r"), "github.com");
        assert_eq!(host("http://example.com"), "example.com");
        assert_eq!(host("https://[2001:db8::1]:443/r"), "2001:db8::1");
        assert_eq!(host("git://git.kernel.org/pub/r.git"), "git.kernel.org");
    }

    #[test]
    fn ssh_url_hosts() {
        assert_eq!(host("ssh://git@github.com/o/r.git"), "github.com");
        assert_eq!(
            host("ssh://git@gitlab.example.com:2222/o/r"),
            "gitlab.example.com"
        );
        assert_eq!(host("git+ssh://me@host.example/r"), "host.example");
    }

    #[test]
    fn scp_like_hosts() {
        assert_eq!(host("git@github.com:o/r.git"), "github.com");
        assert_eq!(host("github.com:o/r.git"), "github.com");
        assert_eq!(host("me@[git.example.com:2222]:r.git"), "git.example.com");
        assert_eq!(host("[::1]:repo"), "::1");
        assert_eq!(host("a@b@host.example:r"), "host.example");
    }

    #[test]
    fn local_paths_and_file_urls_are_local() {
        for url in [
            "file:///srv/repo.git",
            "/srv/repo.git",
            "../sibling",
            "./a:b",
            "repo",
            ".",
        ] {
            assert_eq!(remote_location(url), Ok(RemoteLocation::Local), "{url}");
        }
    }

    #[test]
    fn unknown_transports_fail_closed() {
        for url in [
            "ext::ssh -o ProxyCommand=x host",
            "persistent-https::https://x/r",
            "foo://host/r",
            "https:///r",
            "",
            "git@:r",
        ] {
            assert!(remote_location(url).is_err(), "`{url}` must be refused");
        }
    }

    #[test]
    fn rewrite_rules_take_the_longest_prefix_and_respect_scope() {
        let listing = "global\0url.https://github.com/.insteadof\ngh:\0\
                       local\0url.https://evil.example/.insteadof\ngh:o\0\
                       global\0url.ssh://git@github.com/.pushinsteadof\ngh:\0";
        let all = parse_rewrite_rules(listing, true);
        assert_eq!(
            longest_rewrite(&all, "gh:o/r", false).as_deref(),
            Some("https://evil.example//r")
        );
        assert_eq!(
            longest_rewrite(&all, "gh:o/r", true).as_deref(),
            Some("ssh://git@github.com/o/r")
        );
        let without_repo = parse_rewrite_rules(listing, false);
        assert_eq!(
            longest_rewrite(&without_repo, "gh:o/r", false).as_deref(),
            Some("https://github.com/o/r")
        );
        assert_eq!(longest_rewrite(&without_repo, "x:o/r", false), None);
    }

    #[test]
    fn timeouts_follow_the_subcommand_class() {
        let none: Vec<String> = Vec::new();
        assert_eq!(git_timeout("status", &none), GIT_LOCAL_TIMEOUT);
        assert_eq!(git_timeout("fetch", &none), GIT_NETWORK_TIMEOUT);
        assert_eq!(git_timeout("commit", &none), GIT_HOOK_TIMEOUT);
        assert_eq!(
            git_timeout("remote", &["update".to_string()]),
            GIT_NETWORK_TIMEOUT
        );
        assert_eq!(
            git_timeout("remote", &["add".to_string()]),
            GIT_LOCAL_TIMEOUT
        );
    }

    const ENDPOINT: GatewayEndpoint<'static> = GatewayEndpoint {
        proxy_url: "http://127.0.0.1:4242",
        connect_addr: "127.0.0.1:4242",
        exe: "/opt/my forge/50%'s/forge",
        git_proxy_command: "/tmp/forge-git-proxy-x/git-proxy",
    };

    /// The ProxyCommand option as it appears, shell-quoted, in the command.
    const PROXY_OPTION: &str = r#"-o ControlPath=none -o 'ProxyCommand='\''/opt/my forge/50%%'\''\'\'''\''s/forge'\'' __egress-connect 127.0.0.1:4242 %h %p'"#;

    fn env<'a>(routing: &'a GatewayRouting, name: &str) -> Option<&'a str> {
        routing
            .env
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn routing_points_http_ssh_and_git_protocol_at_the_gateway() {
        let routing = plan_routing(&UserGitSetup::default(), &ENDPOINT).unwrap();
        assert_eq!(
            routing.config,
            vec![("http.proxy".to_string(), ENDPOINT.proxy_url.to_string())]
        );
        for name in PROXY_ENV {
            assert_eq!(env(&routing, name), Some(ENDPOINT.proxy_url), "{name}");
        }
        assert_eq!(routing.env_remove, vec!["NO_PROXY", "no_proxy"]);
        assert_eq!(
            env(&routing, "GIT_SSH_COMMAND"),
            Some(format!("ssh {PROXY_OPTION}").as_str())
        );
        assert_eq!(env(&routing, "GIT_SSH_VARIANT"), None);
        assert_eq!(
            env(&routing, "GIT_PROXY_COMMAND"),
            Some(ENDPOINT.git_proxy_command)
        );
    }

    /// The quoted ProxyCommand survives git's shell and then ssh's: what ssh
    /// receives as one `-o` argument is the exe path, quoted for ssh's shell,
    /// with `%` doubled for ssh's token expansion.
    #[cfg(unix)]
    #[test]
    fn the_proxy_command_survives_both_shells() {
        let routing = plan_routing(&UserGitSetup::default(), &ENDPOINT).unwrap();
        let command = env(&routing, "GIT_SSH_COMMAND").unwrap();
        let command = command.replacen("ssh", "printf '%s\\n'", 1);
        let out = std::process::Command::new("sh")
            .args(["-c", &command])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8(out.stdout).unwrap(),
            "-o\nControlPath=none\n-o\n\
             ProxyCommand='/opt/my forge/50%%'\\''s/forge' __egress-connect 127.0.0.1:4242 %h %p\n"
        );
    }

    #[test]
    fn per_url_and_per_remote_proxies_are_overridden_by_name() {
        let setup = UserGitSetup {
            proxy_keys: vec![
                "http.https://github.com/.proxy".into(),
                "remote.origin.proxy".into(),
                "remote.origin.proxy".into(),
            ],
            ..Default::default()
        };
        let routing = plan_routing(&setup, &ENDPOINT).unwrap();
        let keys: Vec<&str> = routing.config.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            [
                "http.proxy",
                "http.https://github.com/.proxy",
                "remote.origin.proxy"
            ]
        );
        assert!(routing.config.iter().all(|(_, v)| v == ENDPOINT.proxy_url));

        let unsettable = UserGitSetup {
            proxy_keys: vec!["http.https://h/?a=b.proxy".into()],
            ..Default::default()
        };
        let error = plan_routing(&unsettable, &ENDPOINT).unwrap_err();
        assert!(error.contains("cannot override"), "{error}");
    }

    #[test]
    fn the_users_ssh_command_is_kept_and_extended() {
        let core = UserGitSetup {
            core_ssh_command: Some("ssh -i ~/.ssh/work_key".into()),
            ..Default::default()
        };
        assert_eq!(
            env(&plan_routing(&core, &ENDPOINT).unwrap(), "GIT_SSH_COMMAND"),
            Some(format!("ssh -i ~/.ssh/work_key {PROXY_OPTION}").as_str())
        );
        // `GIT_SSH_COMMAND` outranks `core.sshCommand`, as in git.
        let both = UserGitSetup {
            git_ssh_command: Some("ssh -v".into()),
            core_ssh_command: Some("ssh -i k".into()),
            ..Default::default()
        };
        assert_eq!(
            env(&plan_routing(&both, &ENDPOINT).unwrap(), "GIT_SSH_COMMAND"),
            Some(format!("ssh -v {PROXY_OPTION}").as_str())
        );
        // A GIT_SSH that is OpenSSH is used as a quoted program.
        let openssh = UserGitSetup {
            git_ssh: Some("/usr/local/my bin/ssh".into()),
            ..Default::default()
        };
        assert_eq!(
            env(
                &plan_routing(&openssh, &ENDPOINT).unwrap(),
                "GIT_SSH_COMMAND"
            ),
            Some(format!("'/usr/local/my bin/ssh' {PROXY_OPTION}").as_str())
        );
    }

    fn assert_ssh_refused(setup: UserGitSetup, needle: &str) {
        let routing = plan_routing(&setup, &ENDPOINT).unwrap();
        let command = env(&routing, "GIT_SSH_COMMAND").unwrap();
        assert!(command.starts_with("printf '%s\\n' 'forge: refusing git over SSH: "));
        assert!(command.ends_with(">&2; false"), "{command}");
        assert!(command.contains(needle), "{command}");
        assert!(!command.contains("ProxyCommand="), "{command}");
        assert_eq!(env(&routing, "GIT_SSH_VARIANT"), Some("ssh"));
        // HTTP routing is unaffected by an SSH refusal.
        assert_eq!(env(&routing, "HTTPS_PROXY"), Some(ENDPOINT.proxy_url));
    }

    #[test]
    fn ssh_setups_that_cannot_be_routed_fail_closed() {
        assert_ssh_refused(
            UserGitSetup {
                git_ssh: Some("C:/PuTTY/plink.exe".into()),
                ..Default::default()
            },
            "GIT_SSH is `C:/PuTTY/plink.exe`",
        );
        assert_ssh_refused(
            UserGitSetup {
                ssh_variant: Some("plink".into()),
                ..Default::default()
            },
            "variant is `plink`",
        );
        for command in [
            "ssh -J bastion",
            "ssh -vJ bastion",
            "ssh -o ProxyCommand=nc\\ %h\\ %p",
            "ssh -oProxyJump=b",
            "ssh -S /tmp/mux",
            "ssh -o ControlPath=/tmp/mux",
        ] {
            assert_ssh_refused(
                UserGitSetup {
                    core_ssh_command: Some(command.into()),
                    ..Default::default()
                },
                "core.sshCommand sets its own",
            );
        }
        // An option argument that merely contains a `J` is not a jump host.
        for command in ["ssh -iJkey", "ssh -i ~/.ssh/Jkey", "ssh -p 22 -v"] {
            assert!(!sets_its_own_route(command), "{command}");
        }
    }

    #[cfg(unix)]
    fn recorder(dir: &Path) -> String {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("fake-forge");
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nexit 1\n",
                dir.join("argv").display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        path.to_str().unwrap().to_string()
    }

    /// git runs `GIT_PROXY_COMMAND <host> <port>` without a shell; the wrapper
    /// turns that into `<exe> __egress-connect <gateway> <host> <port>`.
    #[cfg(unix)]
    #[test]
    fn git_protocol_goes_through_the_connect_helper() {
        let dir = tempfile::tempdir().unwrap();
        let exe = recorder(dir.path());
        let (scratch, script) = git_proxy_command(&exe, "127.0.0.1:4242").unwrap();
        let out = std::process::Command::new("git")
            .args(["ls-remote", "git://allowed.example:9999/r.git"])
            .current_dir(dir.path())
            .env("GIT_PROXY_COMMAND", &script)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap();
        assert!(!out.status.success());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("argv")).unwrap(),
            "__egress-connect\n127.0.0.1:4242\nallowed.example\n9999\n"
        );
        let scratch = scratch.unwrap();
        let script_dir = scratch.path().to_path_buf();
        drop(scratch);
        assert!(!script_dir.exists(), "the wrapper goes with the call");
    }

    /// A blocking git invocation (an alias that sleeps) is cut off at the
    /// deadline, and the sleeping grandchild is killed with the group.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_git_run_past_its_deadline_is_killed() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("survived");
        let mut cmd = Command::new("git");
        cmd.args([
            "-c",
            &format!("alias.hang=!sleep 2 && touch '{}'", marker.display()),
            "hang",
        ])
        .current_dir(dir.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        let started = std::time::Instant::now();
        let run = run_git_bounded(cmd, Duration::from_millis(200))
            .await
            .unwrap();
        assert!(matches!(run, GitRun::TimedOut));
        assert!(started.elapsed() < Duration::from_secs(2));
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert!(
            !marker.exists(),
            "the hook's grandchild must die with the group"
        );
    }
}
