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
}

impl SandboxPolicy {
    /// The default policy for an agent-spawned process in `workspace_root`.
    pub fn for_workspace(workspace_root: impl AsRef<Path>) -> Self {
        Self {
            workspace_root: workspace_root.as_ref().to_path_buf(),
            session_tmp: None,
            egress_proxy_port: None,
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

    let mut args = vec![
        "--ro-bind".into(),
        "/".into(),
        "/".into(),
        "--dev".into(),
        "/dev".into(),
        "--proc".into(),
        "/proc".into(),
        "--unshare-net".into(),
        "--bind".into(),
        root.clone(),
        root.clone(),
    ];

    if let Some(tmp) = &policy.session_tmp {
        let tmp = tmp.canonicalize().ok()?.to_str()?.to_string();
        args.extend(["--bind".into(), tmp.clone(), tmp]);
    }

    for path in SandboxPolicy::readonly_subpaths_of(Path::new(&root)) {
        let path = path.to_str()?.to_string();
        args.extend(["--ro-bind-try".into(), path.clone(), path]);
    }

    args.extend([
        "--chdir".into(),
        root,
        "--".into(),
        shell.to_string(),
        "-c".into(),
        command.to_string(),
    ]);

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

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn wrap_shell_command_is_none_without_a_sandbox() {
        let ws = workspace();
        assert!(wrap_shell_command(
            "/bin/sh",
            "echo hi",
            &SandboxPolicy::for_workspace(ws.path())
        )
        .is_none());
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
pub fn explain_denial(output: &str) -> Option<&'static str> {
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
        return Some(
            "blocked by the sandbox: writes are confined to the workspace, and \
             .git/.forge are read-only inside it. This is not a file-permission \
             problem on disk.",
        );
    }
    None
}

#[cfg(test)]
mod denial_tests {
    use super::*;

    /// The signature captured from a real confined `curl`: blocking egress
    /// also blocks DNS, so the denial arrives wearing a DNS outage's clothes.
    #[test]
    fn a_blocked_network_call_is_not_reported_as_a_dns_outage() {
        let out = "curl: (6) Could not resolve host: example.com";
        let explained = explain_denial(out).expect("must be recognised as a denial");
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
        assert!(explain_denial(out).is_some_and(|e| e.contains("network")));
    }

    #[test]
    fn a_blocked_write_is_named_as_a_boundary_not_a_file_permission() {
        let out = "/bin/sh: /tmp/escape.txt: Operation not permitted";
        let explained = explain_denial(out).expect("must be recognised");
        assert!(explained.contains("confined to the workspace"));
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
            assert!(explain_denial(out).is_none(), "must not claim: {out}");
        }
    }
}
