//! OS-level confinement for agent-spawned processes.
//!
//! This is the *enforcement* half of the permission model. The other half —
//! whether the user is asked at all — lives in `forge-governance` and never
//! reaches a process. The separation is deliberate and is visible in the
//! dependency graph: `forge-governance` depends on `forge-types` alone and so
//! cannot confine anything, and this crate does not depend on
//! `forge-governance` and so cannot reason about approval.
//!
//! What that buys: a misclassification on the decision side costs a spurious
//! prompt or a missing one, not a breach, because the perimeter here is
//! enforced by the kernel rather than by parsing a shell command.
//!
//! Confinement is applied **at spawn**. A process that started unsandboxed
//! stays unsandboxed for its whole life — there is no retrofit. This matters
//! for `unified_exec`, whose sessions outlive a single turn.
//!
//! Scope (matching Codex's `workspace-write`):
//!
//! * writes — workspace root and the system temp dir only
//! * `.git` / `.forge` — read-only even inside the workspace, because git is
//!   the recovery mechanism and `.forge/permissions.toml` would otherwise let
//!   a confined process widen its own permissions on the next load
//! * network — denied
//! * reads — **allowed broadly**. Toolchains need `~/.gitconfig`, `~/.cargo`
//!   and friends. Reading a secret is therefore possible; exfiltrating it is
//!   not, because network egress is denied. Narrowing reads is a separate
//!   decision, not an oversight.

use std::path::{Path, PathBuf};

/// Why a host cannot confine a process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unavailable {
    /// No implementation for this operating system yet.
    UnsupportedPlatform,
    /// The mechanism exists for this platform but is not installed.
    MissingDependency(&'static str),
}

impl Unavailable {
    /// Short reason, shown to the user next to the permission mode so an
    /// unexpected prompt is explainable without reading docs.
    pub fn reason(&self) -> String {
        match self {
            Self::UnsupportedPlatform => {
                "no sandbox on this platform — run under WSL2 on Windows".into()
            }
            Self::MissingDependency(what) => format!("sandbox unavailable: {what} not found"),
        }
    }
}

/// Whether this host can confine a spawned process.
///
/// Callers must treat `Err` as "fall back to asking the user", never as
/// "run it anyway". Forge does not run agent commands unconfined silently.
pub fn availability() -> Result<(), Unavailable> {
    if cfg!(target_os = "macos") {
        return if Path::new("/usr/bin/sandbox-exec").exists() {
            Ok(())
        } else {
            Err(Unavailable::MissingDependency("sandbox-exec"))
        };
    }
    if cfg!(target_os = "linux") {
        return if bwrap_path().is_some() {
            Ok(())
        } else {
            Err(Unavailable::MissingDependency("bubblewrap"))
        };
    }
    Err(Unavailable::UnsupportedPlatform)
}

/// Where `bwrap` lives, if it is installed.
///
/// Looked up by absolute path rather than through `PATH`: this decides whether
/// a process gets confined, so resolving it through an environment variable the
/// agent could influence would put the boundary in reach of the thing it is
/// meant to contain.
fn bwrap_path() -> Option<&'static str> {
    ["/usr/bin/bwrap", "/bin/bwrap", "/usr/local/bin/bwrap"]
        .into_iter()
        .find(|candidate| Path::new(candidate).exists())
}

/// Single-quote a string for POSIX `sh`.
///
/// The command is embedded in a script that also starts the relay, so it stops
/// being its own argv element. Without quoting, a command containing a quote or
/// a `;` would change the script's meaning — the same injection hazard as the
/// SBPL literal escaping, in a different syntax.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Where `socat` lives, if it is installed.
///
/// Used as the relay that makes the egress proxy reachable from inside the
/// sandbox — the same dependency and the same role Claude Code gives it. No
/// client speaks "proxy over a Unix socket": `curl --unix-socket` addresses the
/// target rather than the proxy, and cargo, git and npm have no equivalent at
/// all. So something has to present the proxy as an ordinary TCP endpoint on
/// the namespace's own loopback, and a battle-tested relay is a better answer
/// than a hand-written one in the security path.
///
/// Absolute paths only, for the reason `bwrap_path` gives.
fn socat_path() -> Option<&'static str> {
    ["/usr/bin/socat", "/bin/socat", "/usr/local/bin/socat"]
        .into_iter()
        .find(|candidate| Path::new(candidate).exists())
}

/// The loopback port the in-sandbox relay listens on.
///
/// Fixed rather than negotiated: it lives inside the sandbox's own network
/// namespace, which contains nothing else, so there is nobody to collide with.
pub const SANDBOX_PROXY_PORT: u16 = 8118;

/// Proxy environment for a confined command, when egress is granted.
///
/// Points at the in-namespace relay, never at the host. These variables are a
/// convenience for well-behaved clients, not the boundary — the boundary is
/// that the namespace has no other route out.
pub fn egress_env(policy: &SandboxPolicy) -> Vec<(String, String)> {
    if policy.egress_socket.is_none() && policy.egress_proxy_port.is_none() {
        return Vec::new();
    }
    let port = if cfg!(target_os = "linux") {
        SANDBOX_PROXY_PORT
    } else {
        match policy.egress_proxy_port {
            Some(port) => port,
            None => return Vec::new(),
        }
    };
    let url = format!("http://127.0.0.1:{port}");
    [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ]
    .into_iter()
    .map(|name| (name.to_string(), url.clone()))
    .collect()
}

/// What a confined process may touch.
#[derive(Debug, Clone)]
pub struct SandboxPolicy {
    workspace_root: PathBuf,
    /// A single writable scratch directory, if the caller provides one.
    ///
    /// Deliberately *not* the whole of `$TMPDIR`. On macOS that is
    /// `/var/folders/<…>/T`, a per-user tree shared by every process; allowing
    /// it wholesale would make "writes outside the workspace are denied" false
    /// for a large part of the filesystem.
    session_tmp: Option<PathBuf>,
    /// Loopback port of the egress proxy, when domain-filtered network access
    /// is granted. `None` denies the network outright.
    egress_proxy_port: Option<u16>,
    /// Unix socket the egress proxy also serves on.
    ///
    /// Linux needs this and macOS does not. Under `--unshare-net` there is no
    /// route to the host's loopback, so a TCP port is unreachable from inside;
    /// a Unix socket is a filesystem object and a bind-mount still reaches it.
    egress_socket: Option<PathBuf>,
}

impl SandboxPolicy {
    /// The default policy for an agent-spawned process in `workspace_root`.
    pub fn for_workspace(workspace_root: impl AsRef<Path>) -> Self {
        Self {
            workspace_root: workspace_root.as_ref().to_path_buf(),
            session_tmp: None,
            egress_proxy_port: None,
            egress_socket: None,
        }
    }

    /// Grant one writable scratch directory. Callers that need a toolchain to
    /// have somewhere to write should create a session-scoped directory and
    /// point `TMPDIR` at it, rather than widening the policy.
    pub fn with_session_tmp(mut self, dir: impl AsRef<Path>) -> Self {
        self.session_tmp = Some(dir.as_ref().to_path_buf());
        self
    }

    /// Permit outbound traffic to the egress proxy on loopback, and nothing
    /// else.
    ///
    /// This is what turns the allowlist from advice into enforcement. Setting
    /// `HTTPS_PROXY` alone is not a boundary — a process free to open sockets
    /// ignores it and connects directly. Denying every outbound destination
    /// *except* the proxy's port is what leaves it no other route, so the
    /// proxy's domain decision becomes the only decision available.
    pub fn with_egress_proxy(mut self, port: u16) -> Self {
        self.egress_proxy_port = Some(port);
        self
    }

    pub fn egress_proxy_port(&self) -> Option<u16> {
        self.egress_proxy_port
    }

    /// Bind-mount the egress proxy's Unix socket into the sandbox.
    ///
    /// The Linux counterpart of [`Self::with_egress_proxy`]. Nothing else is
    /// bind-mounted in, so this socket is the only one a confined process can
    /// reach — which is what makes the proxy the only route out.
    ///
    /// Enforcement here is by what is *reachable*, not by what is *creatable*:
    /// a confined process can still make its own `AF_UNIX` sockets. Blocking
    /// that needs seccomp, and forge deliberately does not do it — the relay
    /// runs *inside* the sandbox and needs exactly the syscall a filter would
    /// remove, so a filter covering the whole sandbox would sever forge's own
    /// egress. Codex can filter because it installs the filter after its
    /// bridge is established; matching that means passing a connected file
    /// descriptor in instead of running socat inside, which is a redesign of
    /// the relay rather than a dependency away.
    ///
    /// What that leaves is a filesystem question, and it is answered where it
    /// can be checked: `/run` and `/tmp` are masked with a tmpfs (docker.sock,
    /// systemd, D-Bus, X11, `$XDG_RUNTIME_DIR`), and the abstract `AF_UNIX`
    /// namespace is scoped to the network namespace that `--unshare-net`
    /// replaces. `a_confined_command_reaches_workspace_sockets_but_not_host_sockets`
    /// holds the remainder, with an in-workspace connect as the control that
    /// keeps it from passing vacuously.
    ///
    /// macOS has no such gap: Seatbelt refuses `AF_UNIX` connects outright.
    pub fn with_egress_socket(mut self, path: impl AsRef<Path>) -> Self {
        self.egress_socket = Some(path.as_ref().to_path_buf());
        self
    }

    pub fn egress_socket(&self) -> Option<&Path> {
        self.egress_socket.as_deref()
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    /// Paths that stay read-only even though they sit inside the writable
    /// root, resolved against `root`.
    fn readonly_subpaths_of(root: &Path) -> Vec<PathBuf> {
        vec![root.join(".git"), root.join(".forge")]
    }

    /// The read-only carve-outs for this policy's workspace, as configured.
    ///
    /// Note these are *unresolved*; the profile resolves the root first. See
    /// [`seatbelt_profile`] for why that matters.
    pub fn readonly_subpaths(&self) -> Vec<PathBuf> {
        Self::readonly_subpaths_of(&self.workspace_root)
    }
}

/// Escape a path for an SBPL string literal.
///
/// Seatbelt profiles are s-expressions; a path containing `"` or `\` would
/// otherwise terminate the literal early and change the meaning of the
/// profile. Fails closed at the call site by returning `None` for a path that
/// is not valid UTF-8, rather than confining a process with a mangled rule.
fn sbpl_literal(path: &Path) -> Option<String> {
    let raw = path.to_str()?;
    let mut out = String::with_capacity(raw.len() + 2);
    for ch in raw.chars() {
        if ch == '"' || ch == '\\' {
            out.push('\\');
        }
        out.push(ch);
    }
    Some(out)
}

/// Build the Seatbelt profile for `policy`.
///
/// `deny default` first, so anything this function forgets to mention is
/// denied rather than allowed — the failure mode is a broken command, not a
/// silent hole.
///
/// **Every path is canonicalised first.** The kernel resolves symlinks before
/// matching, so a rule written against an unresolved path silently never
/// matches. On macOS `/var` is a symlink to `/private/var`, so a workspace
/// under `$TMPDIR` would keep its `(deny … "/var/…/.git")` rule while the
/// kernel checked `/private/var/…/.git` — the `.git` protection would appear
/// to be configured and simply not apply. That failure is invisible: the
/// sandbox still starts, still blocks other things, and only this rule is
/// dead. Canonicalisation failure returns `None`, which callers must treat as
/// "no sandbox" and therefore "ask the user".
pub fn seatbelt_profile(policy: &SandboxPolicy) -> Option<String> {
    let canonical_root = policy.workspace_root.canonicalize().ok()?;
    let root = sbpl_literal(&canonical_root)?;
    let mut profile = String::from(
        "(version 1)\n\
         (deny default)\n\
         (allow process-exec)\n\
         (allow process-fork)\n\
         (allow sysctl-read)\n\
         (allow mach-lookup)\n\
         (allow signal (target same-sandbox))\n\
         (allow file-read*)\n\
         (allow file-write-data \
           (literal \"/dev/null\") \
           (literal \"/dev/zero\") \
           (literal \"/dev/stdout\") \
           (literal \"/dev/stderr\") \
           (literal \"/dev/tty\") \
           (literal \"/dev/dtracehelper\"))\n\
         (allow file-ioctl (literal \"/dev/tty\") (literal \"/dev/dtracehelper\"))\n",
    );

    // Network. `deny network*` first so anything not named below is denied,
    // then — only when an egress proxy exists — one hole to its loopback port.
    // Order matters as ever: the allow must follow the deny to take effect.
    profile.push_str("(deny network*)\n");
    if let Some(port) = policy.egress_proxy_port {
        profile.push_str(&format!(
            "(allow network-outbound (remote ip \"localhost:{port}\"))\n"
        ));
    }

    // Writable: the workspace, and a session scratch directory if one was
    // granted. Nothing else — in particular not all of `$TMPDIR`.
    profile.push_str(&format!("(allow file-write* (subpath \"{root}\")"));
    if let Some(tmp) = &policy.session_tmp {
        let tmp = sbpl_literal(&tmp.canonicalize().ok()?)?;
        profile.push_str(&format!(" (subpath \"{tmp}\")"));
    }
    profile.push_str(")\n");

    // Carved back out *after* the allow, because in SBPL the last matching
    // rule wins. Ordering here is load-bearing: swap these two blocks and
    // `.git` becomes writable.
    //
    // Derived from the canonical root, not the configured one, for the same
    // reason the root itself is canonicalised.
    for path in SandboxPolicy::readonly_subpaths_of(&canonical_root) {
        let literal = sbpl_literal(&path)?;
        profile.push_str(&format!("(deny file-write* (subpath \"{literal}\"))\n"));
    }

    Some(profile)
}

/// Rewrite a shell invocation so it runs confined.
///
/// Returns the program and arguments to spawn in place of the original. On a
/// host with no sandbox, or for a policy that cannot be expressed, returns
/// `None` — callers must then ask the user rather than spawning unconfined.
pub fn wrap_shell_command(
    shell: &str,
    command: &str,
    policy: &SandboxPolicy,
) -> Option<(String, Vec<String>)> {
    availability().ok()?;
    if cfg!(target_os = "linux") {
        return bubblewrap_invocation(shell, command, policy);
    }
    let profile = seatbelt_profile(policy)?;
    Some((
        "/usr/bin/sandbox-exec".to_string(),
        vec![
            "-p".to_string(),
            profile,
            shell.to_string(),
            "-c".to_string(),
            command.to_string(),
        ],
    ))
}

/// Build the `bwrap` invocation for `policy`.
///
/// Bubblewrap applies binds in order and a later bind wins, which is the same
/// last-match-wins property the Seatbelt profile relies on — and the same
/// ordering hazard. The sequence is deliberate:
///
/// 1. `--ro-bind / /` — everything visible and readable, nothing writable.
///    Matches the macOS policy: toolchains need `~/.gitconfig` and `~/.cargo`,
///    so reads stay broad and the network denial is what stops a secret
///    leaving.
/// 2. `--bind <workspace>` — carve the workspace back out as writable.
/// 3. `--ro-bind-try <workspace>/.git`, `.forge` — carve those back to
///    read-only. Emitted last or they would be writable. `-try` because a
///    workspace need not have either directory yet, and bwrap fails on a
///    missing bind source.
///
/// `--unshare-net` denies egress. `--dev` and `--proc` are the minimum a shell
/// needs; without them ordinary redirects fail the way `/dev/null` did on
/// macOS.
fn bubblewrap_invocation(
    shell: &str,
    command: &str,
    policy: &SandboxPolicy,
) -> Option<(String, Vec<String>)> {
    let bwrap = bwrap_path()?;
    let root = policy.workspace_root.canonicalize().ok()?;
    let root = root.to_str()?.to_string();

    // Order is the security property here, and bwrap applies operations in
    // sequence with the last one winning. The sequence below is not arbitrary:
    //
    //   1. read-only root      everything visible, nothing writable
    //   2. mask socket dirs    hide /run and /tmp, where host sockets live
    //   3. bind the workspace  the one writable place, re-exposed over the mask
    //   4. bind the egress socket, if granted
    //   5. carve .git/.forge back to read-only
    //
    // Steps 2 and 3 are in this order because a workspace can itself live
    // under /tmp — a temp dir, or a checkout someone made there. Masking after
    // binding hides the workspace, and bwrap then fails with "Can't chdir to
    // /tmp/...". Masking first and re-binding over it keeps both properties.
    let mut args = vec![
        "--ro-bind".into(),
        "/".into(),
        "/".into(),
        "--dev".into(),
        "/dev".into(),
        "--proc".into(),
        "/proc".into(),
        "--unshare-net".into(),
    ];

    // Mask the directories where Unix sockets live. `--ro-bind / /` puts every
    // host socket into view — /var/run/docker.sock among them, which is root on
    // the host — and a read-only *mount* does not reliably stop `connect()`,
    // because that checks the inode rather than MNT_READONLY.
    //
    // `/var/run` is deliberately absent: on modern Linux it is a symlink to
    // `/run`, and bwrap cannot mount a tmpfs onto a symlink — it fails with
    // "Can't mount tmpfs on /newroot/var/run" and takes the whole invocation
    // with it. Masking `/run` covers both, because the symlink resolves there.
    args.extend([
        "--tmpfs".into(),
        "/run".into(),
        "--tmpfs".into(),
        "/tmp".into(),
    ]);

    // The one writable place, re-exposed over the mask above.
    args.extend(["--bind".into(), root.clone(), root.clone()]);

    if let Some(tmp) = &policy.session_tmp {
        let tmp = tmp.canonicalize().ok()?.to_str()?.to_string();
        args.extend(["--bind".into(), tmp.clone(), tmp]);
    }

    // The one route out, when egress is granted at all. Bound after the mask
    // for the same reason as the workspace: the socket may live under /tmp.
    let relay = match (&policy.egress_socket, socat_path()) {
        (Some(socket), Some(socat)) => {
            let socket = socket.to_str()?.to_string();
            args.extend(["--bind".into(), socket.clone(), socket.clone()]);
            Some((socat, socket))
        }
        // Granting egress without a relay would bind a socket no client can
        // use. Fail closed: no relay means no route out, not a broken one.
        (Some(_), None) => return None,
        (None, _) => None,
    };

    // Carved back out last, because the last matching bind wins: emitted
    // before the workspace bind, these would be overridden and `.git` would be
    // writable.
    for path in SandboxPolicy::readonly_subpaths_of(Path::new(&root)) {
        let path = path.to_str()?.to_string();
        args.extend(["--ro-bind-try".into(), path.clone(), path]);
    }

    args.extend(["--chdir".into(), root, "--".into()]);

    match relay {
        Some((socat, socket)) => {
            // Start the relay, wait for it to listen, then hand over to the
            // command. `exec` so the command keeps the shell's pid and signals
            // reach it; the relay dies with the namespace.
            //
            // The relay's stdout and stderr go to /dev/null, and this is not
            // tidiness. A backgrounded process inherits the pipes, so a caller
            // reading the command's output to EOF — which is what
            // `Command::output()` does — waits on the relay too. The relay runs
            // for the lifetime of the sandbox, so that wait never ends: every
            // sandboxed command with egress would hang forever rather than
            // return. Closing its ends is what lets the caller see EOF when the
            // command itself finishes.
            let script = format!(
                "{socat} TCP-LISTEN:{port},bind=127.0.0.1,reuseaddr,fork \
                 UNIX-CONNECT:{socket} >/dev/null 2>&1 & \
                 for _ in 1 2 3 4 5 6 7 8 9 10; do \
                 {socat} -u OPEN:/dev/null TCP:127.0.0.1:{port} >/dev/null 2>&1 && break; \
                 sleep 0.1; done; exec {shell} -c {command}",
                port = SANDBOX_PROXY_PORT,
                command = shell_quote(command),
            );
            args.extend([shell.to_string(), "-c".into(), script]);
        }
        None => {
            args.extend([shell.to_string(), "-c".into(), command.to_string()]);
        }
    }

    Some((bwrap.to_string(), args))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real directory, because the profile canonicalises and a path that
    /// does not exist cannot be resolved.
    fn workspace() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn policy_marks_git_and_forge_readonly() {
        let ws = workspace();
        let p = SandboxPolicy::for_workspace(ws.path());
        assert_eq!(
            p.readonly_subpaths(),
            vec![ws.path().join(".git"), ws.path().join(".forge")]
        );
    }

    #[test]
    fn profile_denies_by_default_and_denies_network() {
        let ws = workspace();
        let profile = seatbelt_profile(&SandboxPolicy::for_workspace(ws.path())).unwrap();
        assert!(profile.starts_with("(version 1)\n(deny default)"));
        assert!(profile.contains("(deny network*)"));
    }

    /// The bug this pins: `/var` is a symlink to `/private/var`, the kernel
    /// matches canonical paths, and a rule written against the unresolved path
    /// silently never fires. The sandbox still starts, so nothing looks wrong.
    #[test]
    fn every_rule_uses_canonical_paths() {
        let ws = workspace();
        let canonical = ws.path().canonicalize().unwrap();
        let profile = seatbelt_profile(&SandboxPolicy::for_workspace(ws.path())).unwrap();

        let canonical_str = canonical.to_str().unwrap();
        assert!(
            profile.contains(&format!("(subpath \"{canonical_str}\")")),
            "workspace rule must use the resolved path"
        );
        assert!(
            profile.contains(&format!(
                "(deny file-write* (subpath \"{canonical_str}/.git\"))"
            )),
            "the .git deny must use the resolved path or it never matches"
        );

        // And nothing may reference the unresolved spelling.
        if ws.path() != canonical {
            let unresolved = ws.path().to_str().unwrap();
            assert!(
                !profile.contains(&format!("(subpath \"{unresolved}\")")),
                "an unresolved path in the profile is a rule that never fires"
            );
        }
    }

    /// SBPL takes the last matching rule, so the read-only carve-outs must be
    /// emitted after the writable-root allow. Reversed, `.git` is writable and
    /// the recovery mechanism can delete itself.
    #[test]
    fn readonly_denies_come_after_the_writable_allow() {
        let ws = workspace();
        let canonical = ws.path().canonicalize().unwrap();
        let canonical = canonical.to_str().unwrap();
        let profile = seatbelt_profile(&SandboxPolicy::for_workspace(ws.path())).unwrap();

        let allow = profile
            .find(&format!("(allow file-write* (subpath \"{canonical}\")"))
            .unwrap();
        for carve in [".git", ".forge"] {
            let deny = profile
                .find(&format!(
                    "(deny file-write* (subpath \"{canonical}/{carve}\"))"
                ))
                .unwrap();
            assert!(allow < deny, "the {carve} deny must win over the allow");
        }
    }

    /// `$TMPDIR` on macOS is a shared per-user tree. Granting it wholesale
    /// would make "writes outside the workspace are denied" false for a large
    /// part of the filesystem, so a scratch dir is opt-in and scoped.
    #[test]
    fn no_temp_directory_is_writable_unless_granted() {
        let ws = workspace();
        let profile = seatbelt_profile(&SandboxPolicy::for_workspace(ws.path())).unwrap();
        assert!(!profile.contains("/private/var/folders\""));
        assert!(!profile.contains("(subpath \"/private/tmp\")"));

        let scratch = workspace();
        let granted = seatbelt_profile(
            &SandboxPolicy::for_workspace(ws.path()).with_session_tmp(scratch.path()),
        )
        .unwrap();
        let scratch_canonical = scratch.path().canonicalize().unwrap();
        assert!(granted.contains(&format!(
            "(subpath \"{}\")",
            scratch_canonical.to_str().unwrap()
        )));
    }

    /// A path containing a quote must not be able to close the SBPL literal
    /// early and inject its own rules. Tested on the escaper directly, since
    /// such a path is awkward to create on every filesystem.
    #[test]
    fn quotes_and_backslashes_in_paths_are_escaped() {
        assert_eq!(
            sbpl_literal(Path::new("/ws/a\"b\\c")).unwrap(),
            "/ws/a\\\"b\\\\c"
        );
    }

    #[test]
    fn devices_needed_by_ordinary_shell_usage_are_writable() {
        let ws = workspace();
        let profile = seatbelt_profile(&SandboxPolicy::for_workspace(ws.path())).unwrap();
        for device in ["/dev/null", "/dev/stdout", "/dev/stderr"] {
            assert!(
                profile.contains(&format!("(literal \"{device}\")")),
                "{device} must be writable or redirects fail"
            );
        }
    }

    /// A workspace that cannot be resolved yields no profile, and callers must
    /// read that as "ask the user" rather than "run it anyway".
    #[test]
    fn an_unresolvable_workspace_produces_no_profile() {
        let missing = SandboxPolicy::for_workspace("/nonexistent/forge/workspace");
        assert!(seatbelt_profile(&missing).is_none());
        assert!(wrap_shell_command("/bin/bash", "echo hi", &missing).is_none());
    }

    #[test]
    fn unavailable_reasons_are_user_facing() {
        assert!(Unavailable::UnsupportedPlatform.reason().contains("WSL2"));
        assert!(Unavailable::MissingDependency("bubblewrap")
            .reason()
            .contains("bubblewrap"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn wrap_shell_command_produces_a_sandbox_exec_invocation() {
        let ws = workspace();
        let (program, args) = wrap_shell_command(
            "/bin/bash",
            "echo hi",
            &SandboxPolicy::for_workspace(ws.path()),
        )
        .unwrap();
        assert_eq!(program, "/usr/bin/sandbox-exec");
        assert_eq!(args[0], "-p");
        assert!(args[1].contains("(deny default)"));
        assert_eq!(&args[2..], &["/bin/bash", "-c", "echo hi"]);
    }

    /// Written when Linux had no backend, so it asserted `None` on every
    /// non-macOS host. Linux confines now, so the contract is not "not macOS
    /// means no sandbox" — it is "no sandbox means no invocation".
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn wrap_shell_command_follows_availability() {
        let ws = workspace();
        let wrapped = wrap_shell_command(
            "/bin/sh",
            "echo hi",
            &SandboxPolicy::for_workspace(ws.path()),
        );
        assert_eq!(
            wrapped.is_some(),
            availability().is_ok(),
            "a host that can confine must produce an invocation, and one that cannot must not"
        );
    }
}

#[cfg(test)]
mod bubblewrap_tests {
    use super::*;

    fn workspace() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    /// Argument order is the security property, so it is asserted directly
    /// rather than by set membership. Bubblewrap takes the last matching bind,
    /// so a `.git` carve-out emitted before the writable workspace bind would
    /// be silently overridden — the same hazard as the Seatbelt rule ordering.
    #[test]
    fn readonly_carveouts_come_after_the_writable_bind() {
        let ws = workspace();
        let Some((_, args)) =
            bubblewrap_invocation("sh", "echo hi", &SandboxPolicy::for_workspace(ws.path()))
        else {
            return; // bwrap not installed on this host; nothing to assert
        };
        let root = ws.path().canonicalize().unwrap();
        let root = root.to_str().unwrap();

        let writable = args.iter().position(|a| a == "--bind").unwrap();
        let git = args
            .iter()
            .position(|a| a == &format!("{root}/.git"))
            .expect("the .git carve-out must be present");
        assert!(
            writable < git,
            "a .git bind before the workspace bind would be overridden"
        );
    }

    #[test]
    fn network_is_unshared_and_reads_stay_broad() {
        let ws = workspace();
        let Some((_, args)) =
            bubblewrap_invocation("sh", "echo hi", &SandboxPolicy::for_workspace(ws.path()))
        else {
            return;
        };
        assert!(
            args.iter().any(|a| a == "--unshare-net"),
            "egress must be denied"
        );
        let ro_root = args
            .windows(3)
            .any(|w| w[0] == "--ro-bind" && w[1] == "/" && w[2] == "/");
        assert!(
            ro_root,
            "the whole filesystem stays readable but not writable"
        );
    }

    /// `bwrap` is resolved by absolute path, never through `PATH`: this call
    /// decides whether a process is confined, so letting an environment
    /// variable pick the binary would put the boundary inside the blast radius.
    #[test]
    fn bwrap_is_never_resolved_through_path() {
        if let Some(path) = bwrap_path() {
            assert!(path.starts_with('/'), "must be absolute, got {path}");
        }
    }

    /// Without a grant there is no route out at all: the network namespace is
    /// unshared and nothing is bind-mounted to reach past it.
    #[test]
    fn no_egress_grant_means_no_route_out() {
        let ws = workspace();
        let Some((_, args)) =
            bubblewrap_invocation("sh", "true", &SandboxPolicy::for_workspace(ws.path()))
        else {
            return;
        };
        assert!(args.iter().any(|a| a == "--unshare-net"));
        assert!(
            !args.iter().any(|a| a.ends_with(".sock")),
            "nothing should be bound in without a grant"
        );
    }

    /// The socket is bind-mounted *after* --unshare-net, which is the whole
    /// trick: the namespace removes every network route, and a filesystem
    /// object survives that.
    #[test]
    fn a_granted_socket_is_bound_in_after_the_network_is_unshared() {
        let ws = workspace();
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("egress.sock");
        std::fs::write(&sock, b"").unwrap();

        let policy = SandboxPolicy::for_workspace(ws.path()).with_egress_socket(&sock);
        let Some((_, args)) = bubblewrap_invocation("sh", "true", &policy) else {
            return;
        };

        let unshare = args.iter().position(|a| a == "--unshare-net").unwrap();
        let bound = args
            .iter()
            .position(|a| a == sock.to_str().unwrap())
            .expect("the granted socket must be bound in");
        assert!(
            unshare < bound,
            "the bind must follow the unshare, or it is binding into the host namespace"
        );
    }

    #[test]
    fn an_unresolvable_workspace_yields_no_invocation() {
        assert!(bubblewrap_invocation(
            "sh",
            "echo hi",
            &SandboxPolicy::for_workspace("/nope/missing")
        )
        .is_none());
    }
}

/// Why a confined command failed, when the sandbox is the reason.
///
/// A denial does not announce itself. The filesystem boundary surfaces as
/// `Operation not permitted`, which at least looks like a permission problem —
/// but the network boundary surfaces as `Could not resolve host`, because
/// blocking egress also blocks DNS. That is indistinguishable from a real
/// outage, and a model reading it concludes the network is flaky and retries,
/// or that the host does not exist.
///
/// So the sandbox has to say so itself. This is the "blocked by sandbox" ≠
/// "denied by the user" distinction: the two need different responses, and
/// fusing them is what makes Codex's escalation flow confusing.
///
/// Returns `None` when nothing in the output looks like a denial — an ordinary
/// compile error or test failure must not be dressed up as a sandbox problem.
pub fn explain_denial(output: &str, workspace_root: &Path) -> Option<&'static str> {
    const NETWORK: &[&str] = &[
        "Could not resolve host",
        "Temporary failure in name resolution",
        "Network is unreachable",
        "nodename nor servname provided",
    ];
    const FILESYSTEM: &[&str] = &["Operation not permitted", "Read-only file system"];

    if NETWORK.iter().any(|sig| output.contains(sig)) {
        return Some(
            "blocked by the sandbox: network access is denied. This is not a DNS or \
             connectivity problem — the command ran confined. Fetching dependencies \
             needs a network-enabled run.",
        );
    }
    if FILESYSTEM.iter().any(|sig| output.contains(sig)) {
        return Some(FILESYSTEM_EXPLANATION);
    }

    // Linux reports a blocked write as "No such file or directory", not as a
    // permission error: masked and unbound paths genuinely do not exist inside
    // the sandbox. That string is also the single most common legitimate
    // error, so matching on it alone would blame the sandbox for every typo —
    // which is worse than saying nothing.
    //
    // The distinguishing fact is *which* path is missing. Inside the sandbox
    // an absolute path outside the workspace really is absent, and that is the
    // boundary. A missing file inside the workspace is an ordinary mistake and
    // is left alone.
    if output.contains("No such file or directory") && mentions_path_outside(output, workspace_root)
    {
        return Some(FILESYSTEM_EXPLANATION);
    }
    None
}

const FILESYSTEM_EXPLANATION: &str =
    "blocked by the sandbox: writes are confined to the workspace, and \
     .git/.forge are read-only inside it. Paths outside the workspace do not \
     exist inside the sandbox, so they report as missing rather than \
     forbidden. This is not a file-permission problem on disk.";

/// Whether `output` names an absolute path that is not under `workspace_root`.
///
/// Deliberately conservative: only absolute paths count, and a path under the
/// workspace never does. A false positive here tells the agent to stop trying
/// something that would have worked.
fn mentions_path_outside(output: &str, workspace_root: &Path) -> bool {
    let root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    let root = root.to_string_lossy().to_string();
    output
        .split(|c: char| c.is_whitespace() || c == ':')
        .filter(|token| token.starts_with('/') && token.len() > 1)
        .any(|token| !token.starts_with(&root))
}

#[cfg(test)]
mod denial_tests {
    use super::*;

    /// A workspace root the "outside path" checks are measured against.
    fn ws() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    /// The signature captured from a real confined `curl`: blocking egress
    /// also blocks DNS, so the denial arrives wearing a DNS outage's clothes.
    #[test]
    fn a_blocked_network_call_is_not_reported_as_a_dns_outage() {
        let out = "curl: (6) Could not resolve host: example.com";
        let explained = explain_denial(out, ws().path()).expect("must be recognised as a denial");
        assert!(explained.contains("network access is denied"));
        assert!(
            explained.contains("not a DNS"),
            "the whole point is to contradict the obvious reading"
        );
    }

    #[test]
    fn git_over_https_gets_the_same_explanation() {
        let out =
            "fatal: unable to access 'https://github.com/x/y': Could not resolve host: github.com";
        assert!(explain_denial(out, ws().path()).is_some_and(|e| e.contains("network")));
    }

    #[test]
    fn a_blocked_write_is_named_as_a_boundary_not_a_file_permission() {
        let out = "/bin/sh: /tmp/escape.txt: Operation not permitted";
        let explained = explain_denial(out, ws().path()).expect("must be recognised");
        assert!(explained.contains("confined to the workspace"));
    }

    /// Linux reports a blocked write as a *missing* file, because the path
    /// genuinely does not exist inside the sandbox. Recognised only when the
    /// missing path lies outside the workspace.
    #[test]
    fn a_missing_path_outside_the_workspace_is_named_as_the_boundary() {
        let ws = ws();
        let out = "bash: line 1: /tmp/.tmpABCDEF/nope.txt: No such file or directory";
        let explained = explain_denial(out, ws.path()).expect("must be recognised");
        assert!(explained.contains("do not exist inside the sandbox"));
    }

    /// ...but a missing file *inside* the workspace is an ordinary mistake and
    /// must not be blamed on the sandbox. This is the case that stops every
    /// typo being reported as a policy decision.
    #[test]
    fn a_missing_path_inside_the_workspace_is_left_alone() {
        let ws = ws();
        let inside = ws.path().canonicalize().unwrap().join("typo.txt");
        let out = format!(
            "bash: line 1: {}: No such file or directory",
            inside.display()
        );
        assert!(
            explain_denial(&out, ws.path()).is_none(),
            "an ordinary missing file must not be called a denial"
        );
    }

    /// The failure mode worth guarding: dressing an ordinary error up as a
    /// sandbox problem sends the model chasing the wrong fix.
    #[test]
    fn ordinary_failures_are_left_alone() {
        for out in [
            "error[E0308]: mismatched types",
            "test result: FAILED. 1 failed",
            "bash: frobnicate: command not found",
            "assertion `left == right` failed",
        ] {
            assert!(
                explain_denial(out, ws().path()).is_none(),
                "must not claim: {out}"
            );
        }
    }
}

#[cfg(test)]
mod relay_tests {
    use super::*;

    fn workspace() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    /// Host Unix sockets live in /run, /var/run and /tmp. `--ro-bind / /` puts
    /// them all in view — /var/run/docker.sock among them, which is root on the
    /// host — and a read-only *mount* does not reliably stop `connect()`,
    /// because that checks the inode rather than MNT_READONLY. Masking the
    /// three conventional locations removes the class instead of denylisting
    /// the ones we happened to think of.
    #[test]
    fn socket_directories_are_masked() {
        let ws = workspace();
        let Some((_, args)) =
            bubblewrap_invocation("sh", "true", &SandboxPolicy::for_workspace(ws.path()))
        else {
            return;
        };
        for dir in ["/run", "/tmp"] {
            assert!(
                args.windows(2).any(|w| w[0] == "--tmpfs" && w[1] == dir),
                "{dir} must be masked or host sockets stay reachable"
            );
        }
        // Masking this would abort the whole invocation: it is a symlink to
        // /run on modern Linux and bwrap cannot mount a tmpfs onto a symlink.
        assert!(
            !args
                .windows(2)
                .any(|w| w[0] == "--tmpfs" && w[1] == "/var/run"),
            "/var/run is a symlink to /run; masking it makes bwrap fail"
        );
    }

    /// A workspace can live under /tmp — a temp dir, or a checkout someone
    /// made there. Masking /tmp after binding it hides the workspace and bwrap
    /// fails with "Can't chdir". The mask must come first and the bind must
    /// re-expose it.
    #[test]
    fn a_workspace_under_tmp_survives_the_mask() {
        let ws = workspace();
        let Some((_, args)) =
            bubblewrap_invocation("sh", "true", &SandboxPolicy::for_workspace(ws.path()))
        else {
            return;
        };
        let root = ws.path().canonicalize().unwrap();
        let root = root.to_str().unwrap();

        let mask = args
            .windows(2)
            .position(|w| w[0] == "--tmpfs" && w[1] == "/tmp")
            .expect("/tmp must be masked");
        let bind = args
            .windows(3)
            .position(|w| w[0] == "--bind" && w[1] == root && w[2] == root)
            .expect("the workspace must be bound");
        assert!(
            mask < bind,
            "the mask must precede the workspace bind, or a workspace under /tmp is hidden"
        );
    }

    /// The workspace was bound twice at one point, across two commits that each
    /// added it. Harmless but a sign the ordering was not being read as a
    /// whole; pinned so it stays single.
    #[test]
    fn each_path_is_bound_exactly_once() {
        let ws = workspace();
        let Some((_, args)) =
            bubblewrap_invocation("sh", "true", &SandboxPolicy::for_workspace(ws.path()))
        else {
            return;
        };
        let root = ws.path().canonicalize().unwrap();
        let binds = args
            .windows(3)
            .filter(|w| w[0] == "--bind" && w[1] == root.to_str().unwrap())
            .count();
        assert_eq!(
            binds, 1,
            "the workspace must be bound once, not {binds} times"
        );
    }

    /// The mask must follow the read-only root, or the root wins and the
    /// sockets are back.
    #[test]
    fn masking_follows_the_readonly_root() {
        let ws = workspace();
        let Some((_, args)) =
            bubblewrap_invocation("sh", "true", &SandboxPolicy::for_workspace(ws.path()))
        else {
            return;
        };
        let ro_root = args
            .windows(3)
            .position(|w| w[0] == "--ro-bind" && w[1] == "/" && w[2] == "/")
            .unwrap();
        let mask = args.iter().position(|a| a == "--tmpfs").unwrap();
        assert!(ro_root < mask, "a mask before the root would be overridden");
    }

    /// Granting egress without a relay would bind a socket no client can speak
    /// to. Fail closed rather than ship a route that silently does not work.
    #[test]
    fn egress_without_a_relay_yields_no_invocation() {
        if socat_path().is_some() {
            return; // this host has socat, so the negative case is unobservable
        }
        let ws = workspace();
        let dir = workspace();
        let sock = dir.path().join("egress.sock");
        std::fs::write(&sock, b"").unwrap();
        let policy = SandboxPolicy::for_workspace(ws.path()).with_egress_socket(&sock);
        assert!(bubblewrap_invocation("sh", "true", &policy).is_none());
    }

    /// The command is embedded in a script alongside the relay, so it stops
    /// being its own argv element. A quote or a `;` in it must not be able to
    /// change what the script does — the same injection hazard as the SBPL
    /// literal escaping, in a different syntax.
    ///
    /// Asserted by round-tripping through a real shell rather than by
    /// inspecting the escaped text: `'\''` is correct POSIX quoting but
    /// contains substrings that look alarming, so eyeballing it proves
    /// nothing. What matters is that the shell hands the value back unchanged
    /// as exactly one word.
    #[test]
    fn the_user_command_cannot_break_out_of_the_relay_script() {
        for hostile in [
            "echo hi",
            "x'; reboot; echo '",
            "a\"b",
            "$(reboot)",
            "`reboot`",
            "x; rm -rf /",
            "trailing'",
        ] {
            let script = format!("printf %s {}", shell_quote(hostile));
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(&script)
                .output()
                .expect("run sh");
            assert!(out.status.success(), "script did not parse: {script}");
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                hostile,
                "the shell must return the command unchanged, not execute part of it"
            );
        }
    }

    #[test]
    fn proxy_env_points_at_the_relay_never_the_host() {
        let ws = workspace();
        let dir = workspace();
        let sock = dir.path().join("egress.sock");
        // Grant both forms: Linux routes over the socket, macOS over the port.
        let policy = SandboxPolicy::for_workspace(ws.path())
            .with_egress_socket(&sock)
            .with_egress_proxy(9418);
        let env = egress_env(&policy);
        assert!(!env.is_empty());
        for (name, value) in &env {
            assert!(
                value.contains("127.0.0.1"),
                "{name} must point inside the namespace, got {value}"
            );
        }
        // Both spellings, because tools disagree about which they read.
        assert!(env.iter().any(|(n, _)| n == "HTTP_PROXY"));
        assert!(env.iter().any(|(n, _)| n == "http_proxy"));
    }

    #[test]
    fn no_egress_grant_means_no_proxy_env() {
        let ws = workspace();
        assert!(egress_env(&SandboxPolicy::for_workspace(ws.path())).is_empty());
    }
}

/// Where a confined command may reach the network.
///
/// Produced by whoever starts the egress proxy and carried on `ToolContext`,
/// so every shell spawn is confined the same way. Both fields are needed
/// because the two platforms route differently: macOS reaches the proxy over a
/// loopback port, Linux over a Unix socket bind-mounted past its network
/// namespace.
#[derive(Debug, Clone)]
pub struct EgressGrant {
    pub proxy_port: u16,
    pub socket_path: PathBuf,
}

impl SandboxPolicy {
    /// Apply an egress grant, if one exists.
    ///
    /// Without a grant the policy denies the network outright, which is the
    /// default everywhere.
    pub fn with_egress(self, grant: Option<&EgressGrant>) -> Self {
        match grant {
            Some(grant) => self
                .with_egress_proxy(grant.proxy_port)
                .with_egress_socket(&grant.socket_path),
            None => self,
        }
    }
}
