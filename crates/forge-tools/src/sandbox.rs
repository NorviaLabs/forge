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
//! * reads — the workspace, a granted session scratch directory, and the
//!   OS-owned paths a process needs merely to start (system binaries,
//!   libraries, frameworks, configuration, device nodes). Everything
//!   user-writable is outside that boundary — `~/.ssh`, `~/.aws`, per-user
//!   temp, mounted volumes — exactly as it is for `read_file`/`write_file`.
//!   macOS expresses the deny as a refused read; Linux masks the tree so the
//!   path does not exist.
//!   A shell spawn additionally gets the selected host runtime paths it needs
//!   for the command (for example, a Rustup installation or a user-installed
//!   CLI), all read-only.
//! * writes — workspace root and the granted session scratch directory only
//! * `.git` / `.forge` — read-only even inside the workspace, because git is
//!   the recovery mechanism and `.forge/permissions.toml` would otherwise let
//!   a confined process widen its own permissions on the next load
//! * network — denied. Egress exists only via the in-sandbox proxy, and only
//!   when a host grant created it.

use std::env;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Why a host cannot confine a process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unavailable {
    /// No implementation for this operating system yet.
    UnsupportedPlatform,
    /// The mechanism exists for this platform but is not installed.
    MissingDependency(&'static str),
    /// The mechanism exists but applying it failed — typically because Forge
    /// itself already runs inside another confinement layer, and a process
    /// cannot enter a second sandbox.
    CannotApply,
}

pub fn temp_env(policy: &SandboxPolicy) -> Vec<(&'static str, &Path)> {
    match policy.session_tmp.as_deref() {
        Some(path) => vec![("TMPDIR", path), ("TMP", path), ("TEMP", path)],
        None => Vec::new(),
    }
}

/// The small, read-only part of a user's toolchain that a shell command may
/// need. The rest of the home directory remains outside the sandbox.
#[derive(Debug, Default)]
struct HostRuntimeAccess {
    read_paths: Vec<PathBuf>,
    executable_paths: Vec<PathBuf>,
    rustup_home: Option<PathBuf>,
}

fn configured_home(name: &str, default: Option<PathBuf>) -> Option<PathBuf> {
    env::var_os(name)
        .map(PathBuf::from)
        .or(default)
        .filter(|path| path.is_absolute())
}

fn is_unscoped_runtime_root(path: &Path) -> bool {
    ["/", "/home", "/root", "/Users"]
        .iter()
        .any(|root| path == Path::new(root))
}

fn add_existing_path(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !path.is_absolute()
        || is_unscoped_runtime_root(&path)
        || !path.exists()
        || paths.iter().any(|existing| existing == &path)
    {
        return;
    }
    paths.push(path.clone());
    if let Ok(canonical) = path.canonicalize() {
        if !paths.iter().any(|existing| existing == &canonical) {
            paths.push(canonical);
        }
    }
}

fn add_existing_executable(access: &mut HostRuntimeAccess, path: PathBuf) {
    add_existing_path(&mut access.executable_paths, path);
}

fn path_is_under(path: &Path, root: &Path) -> bool {
    path == root || path.starts_with(root)
}

fn add_rustup_shims(access: &mut HostRuntimeAccess, cargo_bin: &Path) {
    let rustup = cargo_bin.join("rustup");
    let Ok(rustup_target) = rustup.canonicalize() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(cargo_bin) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.file_type().is_symlink()
            && path
                .canonicalize()
                .is_ok_and(|target| target == rustup_target)
        {
            add_existing_executable(access, path);
        }
    }
}

fn find_executable(name: &str, workspace_root: Option<&Path>) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    find_executable_in_path(name, path.as_os_str(), workspace_root)
}

fn find_executable_in_path(
    name: &str,
    path: &OsStr,
    workspace_root: Option<&Path>,
) -> Option<PathBuf> {
    let (candidate, relative) = env::split_paths(path)
        .filter_map(|directory| {
            let relative = directory.is_relative();
            if directory.is_absolute() {
                Some((directory.join(name), relative))
            } else {
                workspace_root.map(|root| (root.join(directory).join(name), relative))
            }
        })
        .find(|(candidate, _)| candidate.is_file())?;
    let canonical = candidate.canonicalize().ok()?;
    if relative && !workspace_root.is_some_and(|root| path_is_under(&canonical, root)) {
        return None;
    }
    Some(candidate)
}

fn command_segments(command: &str) -> impl Iterator<Item = &str> {
    command
        .split([';', '\n', '|', '&', '(', ')'])
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
}

fn leading_executable(command: &str) -> Option<&str> {
    command
        .split_whitespace()
        .find(|token| !token.contains('='))
}

fn discover_command_executables(command: &str, workspace_root: &Path) -> Vec<PathBuf> {
    let workspace_root = workspace_root.canonicalize().ok();
    let mut paths = Vec::new();
    for segment in command_segments(command) {
        let Some(token) = leading_executable(segment) else {
            continue;
        };
        let candidate = Path::new(token);
        let path = if candidate.is_absolute() {
            candidate.is_file().then(|| candidate.to_path_buf())
        } else if candidate.components().count() > 1 {
            // A path containing a separator is resolved by the shell relative
            // to the working directory, not through PATH. A path that leaves
            // the workspace is intentionally not added here: commands may
            // use relative paths for in-workspace scripts, but this helper
            // must not turn `../outside/tool` into a host-runtime exception.
            workspace_root.as_ref().and_then(|workspace_root| {
                workspace_root
                    .join(candidate)
                    .canonicalize()
                    .ok()
                    .filter(|path| path_is_under(path, workspace_root))
            })
        } else {
            find_executable(token, workspace_root.as_deref())
        };
        if let Some(path) = path {
            add_existing_path(&mut paths, path);
        }
    }
    paths
}

fn discover_host_runtime_access(command: &str, workspace_root: &Path) -> HostRuntimeAccess {
    let mut access = HostRuntimeAccess::default();
    let home = env::var_os("HOME").map(PathBuf::from);
    let cargo_home = configured_home("CARGO_HOME", home.clone().map(|path| path.join(".cargo")));
    let rustup_home = configured_home("RUSTUP_HOME", home.map(|path| path.join(".rustup")));

    // Resolve only the executable at the start of each shell command segment.
    // This keeps a user-writable PATH directory from becoming a general
    // read/exec allowlist while supporting any installed CLI, not just the
    // commands used by the regression tests.
    let command_paths = discover_command_executables(command, workspace_root);
    for path in &command_paths {
        add_existing_executable(&mut access, path.clone());
    }

    // Rustup installs a family of symlink shims in Cargo's bin directory. A
    // cargo build can invoke rustc/rustdoc through those shims even when the
    // original command named only cargo. Select the shims by their common
    // Rustup target rather than by a fixed command-name list; unrelated files
    // in the same directory remain outside the profile.
    let cargo_bin = cargo_home.as_deref().map(|home| home.join("bin"));
    let uses_rustup = command_paths.iter().any(|path| {
        cargo_bin
            .as_deref()
            .is_some_and(|bin| path_is_under(path, bin))
            || rustup_home
                .as_deref()
                .is_some_and(|home| path_is_under(path, home))
    });
    if uses_rustup {
        if let Some(rustup_home) = rustup_home.filter(|home| home.exists()) {
            add_existing_path(&mut access.read_paths, rustup_home.clone());
            if let Some(cargo_bin) = cargo_bin.as_deref() {
                add_rustup_shims(&mut access, cargo_bin);
            }
            access.rustup_home = Some(rustup_home);
        }
    }
    access
}

impl Unavailable {
    /// Short reason for logs and tests.
    pub fn reason(&self) -> String {
        match self {
            Self::UnsupportedPlatform => {
                "no sandbox on this platform — run under WSL2 on Windows".into()
            }
            Self::MissingDependency(what) => format!("sandbox unavailable: {what} not found"),
            Self::CannotApply => {
                "sandbox unavailable: this host refused to apply confinement".into()
            }
        }
    }

    /// Stderr copy when Forge refuses to start. The CLI prints this and
    /// exits; it is the only user-facing surface for a missing sandbox.
    pub fn startup_message(&self) -> String {
        match self {
            Self::UnsupportedPlatform => {
                format!(
                    "{}\nInside WSL2, install bubblewrap: sudo apt install bubblewrap",
                    self.reason()
                )
            }
            Self::MissingDependency("bubblewrap") => {
                format!(
                    "{}\nInstall it with: sudo apt install bubblewrap   or   brew install bubblewrap",
                    self.reason()
                )
            }
            Self::MissingDependency("sandbox-exec") => {
                format!(
                    "{}\nForge needs /usr/bin/sandbox-exec, which ships with macOS.",
                    self.reason()
                )
            }
            Self::MissingDependency(what) => {
                format!("{}\nInstall {what} and retry.", self.reason())
            }
            Self::CannotApply => format!(
                "{}\nForge is likely running inside another sandbox, and a process \
                 cannot enter a second one. Run Forge outside that layer.",
                self.reason()
            ),
        }
    }
}

/// Whether this host can confine a spawned process.
///
/// The supported CLI treats `Err` as "do not start". `wrap_shell_command`
/// still returns `None` here so a caller that reaches spawn without a
/// sandbox cannot run the command unconfined.
pub fn availability() -> Result<(), Unavailable> {
    if cfg!(target_os = "macos") {
        return if !Path::new("/usr/bin/sandbox-exec").exists() {
            Err(Unavailable::MissingDependency("sandbox-exec"))
        } else if seatbelt_applicable() {
            Ok(())
        } else {
            Err(Unavailable::CannotApply)
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

/// Whether the kernel actually lets us apply a Seatbelt profile.
///
/// `/usr/bin/sandbox-exec` existing says nothing about whether
/// `sandbox_apply` succeeds: when Forge itself runs inside another
/// confinement layer (a CI sandbox, an agent harness), entering a second
/// sandbox fails with `Operation not permitted`. Probed once and cached —
/// a host does not change its mind mid-process.
fn seatbelt_applicable() -> bool {
    static APPLICABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *APPLICABLE.get_or_init(|| {
        std::process::Command::new("/usr/bin/sandbox-exec")
            .arg("-p")
            .arg("(version 1)\n(allow default)\n")
            .arg("/usr/bin/true")
            .stdin(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    })
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
    /// When true, `.git` is writable except for `.git/hooks`. Default is
    /// false: git is the recovery mechanism. Only spawns that run git or a
    /// git frontend opt in — a host grant alone must not lift the carve-out
    /// for every command in the session.
    git_writable: bool,
    /// Read-only user runtime directories needed by the selected command.
    /// This is opt-in on the policy so low-level callers that build a stricter
    /// profile keep the old boundary; shell spawns opt in through
    /// [`Self::with_command_access`].
    toolchain_read_paths: Vec<PathBuf>,
    /// Individual executables (not whole PATH directories) that are selected
    /// by a shell command and live outside the system runtime paths.
    toolchain_executable_paths: Vec<PathBuf>,
    /// The host Rustup state needed by a selected Rust toolchain. It is never
    /// made writable by the sandbox.
    rustup_home: Option<PathBuf>,
}

impl SandboxPolicy {
    /// The default policy for an agent-spawned process in `workspace_root`.
    pub fn for_workspace(workspace_root: impl AsRef<Path>) -> Self {
        Self {
            workspace_root: workspace_root.as_ref().to_path_buf(),
            session_tmp: None,
            egress_proxy_port: None,
            egress_socket: None,
            git_writable: false,
            toolchain_read_paths: Vec::new(),
            toolchain_executable_paths: Vec::new(),
            rustup_home: None,
        }
    }

    /// Add the read-only runtime paths selected from the host environment and
    /// the exact executables named by this shell command. Resolving the
    /// command from the host `PATH` avoids a fixed binary list: any installed
    /// tool can work without exposing unrelated home directories.
    pub fn with_command_access(self, command: &str) -> Self {
        let access = discover_host_runtime_access(command, &self.workspace_root);
        Self {
            toolchain_read_paths: access.read_paths,
            toolchain_executable_paths: access.executable_paths,
            rustup_home: access.rustup_home,
            ..self
        }
    }

    /// Set child environment for mutable tool state without exposing the
    /// host's Cargo home. No environment is changed for an unconfined retry,
    /// which should use the user's normal configuration.
    pub fn toolchain_env(&self) -> Vec<(String, String)> {
        let Some(session_tmp) = &self.session_tmp else {
            return Vec::new();
        };
        let cargo_home = session_tmp.join("cargo-home");
        let _ = std::fs::create_dir_all(&cargo_home);
        let mut env = vec![(
            "CARGO_HOME".to_string(),
            cargo_home.to_string_lossy().into_owned(),
        )];
        if let Some(rustup_home) = &self.rustup_home {
            env.push((
                "RUSTUP_HOME".to_string(),
                rustup_home.to_string_lossy().into_owned(),
            ));
        }
        env
    }

    /// Permit writes under `.git` for this spawn only. `.git/hooks` stays
    /// read-only regardless — see [`Self::readonly_subpaths_of`].
    pub fn with_git_writable(mut self) -> Self {
        self.git_writable = true;
        self
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
    /// What that leaves is a filesystem question, and the answer is partial.
    /// State it plainly, because the shape of the guarantee is the security
    /// property:
    ///
    /// **Covered.** `/run` and `/tmp` are masked with a tmpfs, which accounts
    /// for docker.sock (root on the host), systemd, D-Bus, X11, and
    /// `$XDG_RUNTIME_DIR` at `/run/user/$UID` — where ssh-agent and gpg-agent
    /// live. The abstract `AF_UNIX` namespace is scoped to the network
    /// namespace that `--unshare-net` replaces. Held by
    /// `a_confined_command_reaches_workspace_sockets_but_not_masked_host_sockets`,
    /// with an in-workspace connect as the control that keeps it from passing
    /// vacuously.
    ///
    /// **Not covered.** `--ro-bind / /` exposes every other host path, and a
    /// read-only *mount* does not stop `connect()` — that checks the inode,
    /// not `MNT_READONLY`. A pathname socket outside the masked directories —
    /// under `/var` or `/opt` most realistically, after `/home` was masked —
    /// is reachable from inside the sandbox. The old worst case under `$HOME`
    /// (`~/.docker`, Docker Desktop) is closed by the home-tree tmpfs above.
    /// Reproduced by the `#[ignore]`d
    /// `a_confined_command_can_still_reach_a_socket_under_home`, and tracked
    /// in #390.
    ///
    /// Closing it needs a path-aware mechanism the mount namespace lacks.
    /// seccomp-bpf cannot: it sees scalar registers and cannot dereference the
    /// `sockaddr_un *`. seccomp user-notification can read the target's memory
    /// but `seccomp_unotify(2)` says it "must not be used to make security
    /// policy decisions about the system call, which would be inherently
    /// race-prone". Landlock ABI 9's `LANDLOCK_ACCESS_FS_RESOLVE_UNIX` is the
    /// right primitive and restricts `connect(2)` on pathname sockets — ABI 5
    /// shipped in 6.10, 6 in 6.12, 7 in 6.15, so it is far ahead of the kernels
    /// forge runs on today. Adopt it opportunistically when it is reachable.
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
    fn readonly_subpaths_of(root: &Path, git_writable: bool) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        if git_writable {
            // Refs, objects and config have to move for a push to happen.
            // Hooks do not, and they are the one part of `.git` that is
            // executed rather than read: a hook written here runs with the
            // user's full privileges the next time they use git *outside*
            // the sandbox. Publishing never needs to write one, so the
            // carve-out stops short of them.
            paths.push(root.join(".git/hooks"));
        } else {
            paths.push(root.join(".git"));
        }
        paths.push(root.join(".forge"));
        paths
    }

    /// The read-only carve-outs for this policy's workspace, as configured.
    ///
    /// Note these are *unresolved*; the profile resolves the root first. See
    /// [`seatbelt_profile`] for why that matters.
    pub fn readonly_subpaths(&self) -> Vec<PathBuf> {
        Self::readonly_subpaths_of(&self.workspace_root, self.git_writable)
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
///
/// Launch Services is the other deliberate hole. `open <path>` does not read
/// the file; it asks `launchservicesd` to hand it to another app. That needs
/// `appleevent-send` and `user-preference-read` (the default-app binding).
/// Those apps are not confined, and the same rights let `osascript` drive
/// GUI apps. Accepted so `open` works the way it did before confinement.
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
         (allow appleevent-send)\n\
         (allow user-preference-read)\n\
         (allow signal (target same-sandbox))\n\
         (allow file-write-data \
           (literal \"/dev/null\") \
           (literal \"/dev/zero\") \
           (literal \"/dev/stdout\") \
           (literal \"/dev/stderr\") \
           (literal \"/dev/tty\") \
           (literal \"/dev/dtracehelper\"))\n\
         (allow file-ioctl (literal \"/dev/tty\") (literal \"/dev/dtracehelper\"))\n",
    );

    // Reads. `(deny default)` closed the filesystem above; this block is the
    // complete list of what a confined process may read, and it is the same
    // boundary `read_file`/`write_file` enforce path by path: the workspace
    // and the granted session scratch directory, plus the OS-owned paths a
    // process needs merely to start.
    //
    // The system paths below are deliberately a list of *system-owned* trees,
    // not a sampling of convenient ones: binaries and dylibs (/bin, /sbin,
    // /usr, Launch Services' frameworks, the macOS SDK), standard configuration and
    // user-lookup databases (/private/etc, /Library/Preferences, the
    // DarwinDirectory record store and timezone data), the root cert store
    // needed to speak TLS through an egress grant (/Library/Keychains — user
    // keychains live under $HOME and stay denied), and the standard device
    // nodes. None of these are user-writable, so none of them can hold a
    // user secret.
    //
    // Everything user-writable is outside: $HOME (/Users/...), per-user temp
    // (/private/var/folders/... — only the granted scratch dir below reaches
    // it), mounted volumes, /Network, and /opt except the root-owned
    // Apple-Silicon toolchain prefix. The selected user runtime paths and
    // resolved executables are added below; a path omitted here is denied by
    // the default.
    profile.push_str(&format!(
        "(allow file-read* file-test-existence\n  (subpath \"{root}\")"
    ));
    if let Some(tmp) = &policy.session_tmp {
        let tmp = sbpl_literal(&tmp.canonicalize().ok()?)?;
        profile.push_str(&format!("\n  (subpath \"{tmp}\")"));
    }
    for path in &policy.toolchain_read_paths {
        let path = sbpl_literal(&path.canonicalize().ok()?)?;
        profile.push_str(&format!("\n  (subpath \"{path}\")"));
    }
    for path in &policy.toolchain_executable_paths {
        let path = sbpl_literal(&path.canonicalize().ok()?)?;
        profile.push_str(&format!("\n  (literal \"{path}\")"));
    }
    profile.push_str(
        "\n  (literal \"/\")\n\
         \n  (subpath \"/System\")\n\
         \n  (subpath \"/usr\")\n\
         \n  (subpath \"/bin\")\n\
         \n  (subpath \"/sbin\")\n\
         \n  (subpath \"/Library/Apple\")\n\
         \n  (subpath \"/Library/Developer\")\n\
         \n  (subpath \"/Library/Keychains\")\n\
         \n  (subpath \"/Library/Preferences\")\n\
         \n  (subpath \"/private/etc\")\n\
         \n  (subpath \"/etc\")\n\
         \n  (subpath \"/opt/homebrew\")\n\
         \n  (subpath \"/private/var/db/timezone\")\n\
         \n  (subpath \"/private/var/db/DarwinDirectory/local/recordStore.data\"))\n\
         \n\
         (allow file-map-executable\n  (subpath \"/System\")\n  (subpath \"/usr\")\n  (subpath \"/bin\")\n  (subpath \"/sbin\"))\n\
         \n\
         (allow file-read-metadata (subpath \"/var\") (subpath \"/private/var\"))\n\
         \n\
         (allow file-read* file-test-existence\n\
         (literal \"/dev/null\") (literal \"/dev/zero\")\n\
         (literal \"/dev/random\") (literal \"/dev/urandom\")\n\
         (literal \"/dev/tty\") (literal \"/dev/ptmx\")\n\
         (literal \"/dev/dtracehelper\")\n\
         (literal \"/dev/stdin\") (literal \"/dev/stdout\") (literal \"/dev/stderr\"))\n\
         (allow file-read-metadata (literal \"/dev\"))\n",
    );

    // Some runtimes canonicalize their output and cache paths before opening
    // them. Allow metadata on the parent chain needed to walk to each scoped
    // root, without allowing file contents in any additional directory.
    let mut metadata_paths = Vec::new();
    for path in std::iter::once(canonical_root.as_path())
        .chain(policy.session_tmp.iter().map(|path| path.as_path()))
        .chain(
            policy
                .toolchain_read_paths
                .iter()
                .map(|path| path.as_path()),
        )
        .chain(
            policy
                .toolchain_executable_paths
                .iter()
                .map(|path| path.as_path()),
        )
    {
        let mut parent = path.parent();
        while let Some(path) = parent {
            if metadata_paths.iter().any(|existing| existing == path) {
                break;
            }
            metadata_paths.push(path.to_path_buf());
            parent = path.parent();
        }
    }
    profile.push_str("(allow file-read-metadata");
    for path in metadata_paths {
        let path = sbpl_literal(&path)?;
        profile.push_str(&format!("\n  (subpath \"{path}\")"));
    }
    profile.push_str(")\n");

    // Executables in a read allowlist still need to be mapped with execute
    // permission. The parent metadata rule lets the shell traverse a
    // user-writable PATH directory without making sibling binaries readable.
    if !policy.toolchain_read_paths.is_empty() || !policy.toolchain_executable_paths.is_empty() {
        profile.push_str("(allow file-map-executable");
        for path in &policy.toolchain_read_paths {
            let path = sbpl_literal(&path.canonicalize().ok()?)?;
            profile.push_str(&format!("\n  (subpath \"{path}\")"));
        }
        for path in &policy.toolchain_executable_paths {
            let path = sbpl_literal(&path.canonicalize().ok()?)?;
            profile.push_str(&format!("\n  (literal \"{path}\")"));
        }
        profile.push_str(")\n");
    }

    if !policy.toolchain_executable_paths.is_empty() {
        profile.push_str("(allow file-read-metadata");
        for path in &policy.toolchain_executable_paths {
            let parent = path.parent()?.canonicalize().ok()?;
            let parent = sbpl_literal(&parent)?;
            profile.push_str(&format!("\n  (subpath \"{parent}\")"));
        }
        profile.push_str(")\n");
    }

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
    for path in SandboxPolicy::readonly_subpaths_of(&canonical_root, policy.git_writable) {
        let literal = sbpl_literal(&path)?;
        profile.push_str(&format!("(deny file-write* (subpath \"{literal}\"))\n"));
    }

    Some(profile)
}

#[cfg(test)]
fn primary_file_read_rule(profile: &str) -> &str {
    let (_, rule) = profile
        .split_once("(allow file-read* file-test-existence")
        .expect("profile must contain a primary file-read rule");
    rule.split_once("))").map_or(rule, |(rule, _)| rule)
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
///    System paths and toolchains work; the network denial plus the masked
///    user trees are what stop a secret leaving.
/// 2. `--tmpfs /run /tmp /home /root` — user and socket trees vanish, which
///    is the read boundary on Linux: `~/.ssh` and `~/.aws` do not exist here.
/// 3. `--ro-bind-try <runtime path>` — re-expose only the selected user
///    runtime and executable paths needed by the command.
/// 4. `--bind <workspace>` — carve the workspace back out as writable.
/// 5. `--ro-bind-try <workspace>/.git`, `.forge` — carve those back to
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
    //   2. mask user + socket dirs  hide /run, /tmp, /home, /root
    //   3. re-expose the selected toolchain paths
    //   4. bind the workspace  the one writable place, re-exposed over the mask
    //   5. bind the egress socket, if granted
    //   6. carve .git/.forge back to read-only
    //
    // Steps 2 and 3 are in this order because a workspace can itself live
    // under one of the masked trees — a temp dir, or a checkout someone made
    // under $HOME. Masking after binding hides the workspace, and bwrap then
    // fails with "Can't chdir to /home/...". Masking first and re-binding
    // over it keeps both properties.
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

    // Mask the directories where user data and Unix sockets live.
    // `--ro-bind / /` puts every host path into view — /var/run/docker.sock
    // (root on the host) among them — and a read-only *mount* does not
    // reliably stop `connect()`, because that checks the inode rather than
    // MNT_READONLY.
    //
    // A tmpfs over a directory makes every path under it vanish: reads
    // report ENOENT rather than EPERM. That is the Linux half of the read
    // boundary — the macOS Seatbelt profile expresses the same deny as a
    // file-read refusal — and it covers the two places secrets live per-user:
    // $HOME and the sockets under it (~/.docker, ssh-agent, gpg-agent), and
    // /run+/tmp where the rest of the host sockets sit (docker.sock, systemd,
    // D-Bus, $XDG_RUNTIME_DIR).
    //
    // `/var/run` is deliberately absent: on modern Linux it is a symlink to
    // `/run`, and bwrap cannot mount a tmpfs onto a symlink — it fails with
    // "Can't mount tmpfs on /newroot/var/run" and takes the whole invocation
    // with it. Masking `/run` covers both, because the symlink resolves there.
    //
    // The home masks come before the workspace bind below: a workspace that
    // lives under /home or /root must be re-exposed by that bind, exactly as
    // a workspace under /tmp is.
    args.extend([
        "--tmpfs".into(),
        "/run".into(),
        "--tmpfs".into(),
        "/tmp".into(),
        "--tmpfs".into(),
        "/home".into(),
        "--tmpfs".into(),
        "/root".into(),
    ]);

    // Re-expose only the runtime paths selected from the host environment.
    // A `--dir` is needed for each missing parent below a masked home tree;
    // it does not grant read access by itself. Binding the original path (not
    // its canonical spelling) keeps PATH entries that are symlinks usable.
    for path in policy
        .toolchain_read_paths
        .iter()
        .chain(policy.toolchain_executable_paths.iter())
    {
        let path = path.to_str()?.to_string();
        let path_ref = Path::new(&path);
        if let Some(mask) = masked_home_root(path_ref) {
            let destination = if path_ref.is_dir() {
                path_ref
            } else {
                path_ref.parent()?
            };
            let relative = destination.strip_prefix(mask).ok()?;
            let mut parent = mask.to_path_buf();
            for component in relative.components() {
                if let std::path::Component::Normal(component) = component {
                    parent.push(component);
                    let parent = parent.to_str()?.to_string();
                    if !args
                        .windows(2)
                        .any(|window| window[0] == "--dir" && window[1] == parent)
                    {
                        args.extend(["--dir".into(), parent]);
                    }
                }
            }
        }
        args.extend(["--ro-bind-try".into(), path.clone(), path]);
    }

    // The one writable place, re-exposed over the mask above.
    args.extend(["--bind".into(), root.clone(), root.clone()]);

    if let Some(tmp) = &policy.session_tmp {
        let tmp = tmp.canonicalize().ok()?.to_str()?.to_string();
        args.extend(["--bind".into(), tmp.clone(), tmp]);
    }

    // The one route out, when egress is granted at all. Bound after the mask
    // for the same reason as the workspace: the socket may live under /tmp.
    let relay = match (&policy.egress_socket, socat_path()) {
        // A grant can outlive the proxy that backs it: the session's proxy dies
        // and takes its socket file with it, while the grant is still attached
        // to the next command. Binding a source that no longer exists makes
        // bwrap refuse to start, and the caller then sees the generic
        // message — which points at the wrong thing entirely. Drop the dead
        // grant instead and run with no route out, which is what a dead proxy
        // means; still fail closed, just legibly.
        (Some(socket), _) if !socket.exists() => None,
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
    for path in SandboxPolicy::readonly_subpaths_of(Path::new(&root), policy.git_writable) {
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

fn masked_home_root(path: &Path) -> Option<&'static Path> {
    if path.starts_with("/home") {
        Some(Path::new("/home"))
    } else if path.starts_with("/root") {
        Some(Path::new("/root"))
    } else {
        None
    }
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
        assert_eq!(
            p.clone().with_git_writable().readonly_subpaths(),
            vec![ws.path().join(".git/hooks"), ws.path().join(".forge")],
            "a publish spawn must be able to update refs, but never install a hook"
        );
    }

    #[test]
    fn profile_denies_by_default_and_denies_network() {
        let ws = workspace();
        let profile = seatbelt_profile(&SandboxPolicy::for_workspace(ws.path())).unwrap();
        assert!(profile.starts_with("(version 1)\n(deny default)"));
        assert!(profile.contains("(deny network*)"));
    }

    #[test]
    fn toolchain_allowlist_exposes_cargo_rustup_and_only_the_resolved_gh() {
        let ws = workspace();
        let toolchain = workspace();
        let cargo_bin = toolchain.path().join(".cargo/bin");
        let rustup_home = toolchain.path().join(".rustup");
        let gh = toolchain.path().join(".local/bin/gh");
        let unrelated = toolchain.path().join(".local/bin/unrelated");
        std::fs::create_dir_all(&cargo_bin).unwrap();
        std::fs::create_dir_all(&rustup_home).unwrap();
        std::fs::create_dir_all(gh.parent().unwrap()).unwrap();
        std::fs::write(cargo_bin.join("cargo"), b"").unwrap();
        std::fs::write(&gh, b"").unwrap();
        std::fs::write(&unrelated, b"").unwrap();

        let mut policy = SandboxPolicy::for_workspace(ws.path());
        policy.toolchain_read_paths = vec![cargo_bin.clone(), rustup_home.clone()];
        policy.toolchain_executable_paths = vec![gh.clone()];
        let profile = seatbelt_profile(&policy).unwrap();
        let read_rule = primary_file_read_rule(&profile);

        for path in [&cargo_bin, &rustup_home] {
            let path = path.canonicalize().unwrap();
            assert!(
                profile.contains(&format!("(subpath \"{}\")", path.display())),
                "toolchain path must be readable: {}",
                path.display()
            );
        }
        let gh = gh.canonicalize().unwrap();
        assert!(
            profile.contains(&format!("(literal \"{}\")", gh.display())),
            "gh must be allowed as an exact executable"
        );
        assert!(
            !profile.contains(&format!("(literal \"{}\")", unrelated.display())),
            "an unrelated binary must not be added to the executable allowlist"
        );
        assert!(
            !read_rule.contains(&format!("(subpath \"{}\")", toolchain.path().display())),
            "the user's home/toolchain parent must not be allowed wholesale"
        );
    }

    #[test]
    fn command_access_resolves_an_arbitrary_named_executable() {
        let ws = workspace();
        let commands = workspace();
        let selected = commands.path().join("custom-cli");
        let unrelated = commands.path().join("unrelated-cli");
        std::fs::write(&selected, b"#!/bin/sh\n").unwrap();
        std::fs::write(&unrelated, b"#!/bin/sh\n").unwrap();

        let policy = SandboxPolicy::for_workspace(ws.path())
            .with_command_access(&format!("{} --version", selected.display()));
        let profile = seatbelt_profile(&policy).unwrap();
        let selected = selected.canonicalize().unwrap();
        assert!(
            profile.contains(&format!("(literal \"{}\")", selected.display())),
            "the executable named by the command must be reachable"
        );
        assert!(
            !profile.contains(&format!("(literal \"{}\")", unrelated.display())),
            "an unrelated executable in the same directory must stay outside"
        );
    }

    #[test]
    fn relative_command_paths_cannot_escape_the_workspace_allowlist() {
        let ws = workspace();
        let outside = workspace();
        let selected = outside.path().join("outside-cli");
        std::fs::write(&selected, b"#!/bin/sh\n").unwrap();

        let policy = SandboxPolicy::for_workspace(ws.path()).with_command_access(&format!(
            "../{}/outside-cli",
            outside.path().file_name().unwrap().to_string_lossy()
        ));
        let profile = seatbelt_profile(&policy).unwrap();
        assert!(
            !profile.contains(&format!("(literal \"{}\")", selected.display())),
            "a relative path outside the workspace must not become a runtime exception"
        );
    }

    #[test]
    fn relative_path_entries_cannot_escape_the_workspace_allowlist() {
        let ws = workspace();
        let outside = workspace();
        let selected = outside.path().join("outside-cli");
        std::fs::write(&selected, b"#!/bin/sh\n").unwrap();
        let root = ws.path().canonicalize().unwrap();
        let path = std::ffi::OsString::from(format!(
            "../{}",
            outside.path().file_name().unwrap().to_string_lossy()
        ));

        assert!(
            find_executable_in_path("outside-cli", path.as_os_str(), Some(&root)).is_none(),
            "a relative PATH entry outside the workspace must not become a runtime exception"
        );
    }

    #[test]
    fn runtime_access_never_reopens_a_filesystem_root() {
        let mut paths = Vec::new();
        add_existing_path(&mut paths, PathBuf::from("/"));
        assert!(
            paths.is_empty(),
            "runtime discovery must not allow `/` wholesale"
        );
    }

    #[test]
    fn cargo_home_is_session_local_and_rustup_home_is_read_only_host_state() {
        let ws = workspace();
        let scratch = workspace();
        let rustup_home = workspace();
        let mut policy = SandboxPolicy::for_workspace(ws.path()).with_session_tmp(scratch.path());
        policy.rustup_home = Some(rustup_home.path().to_path_buf());
        let cargo_home = scratch.path().join("cargo-home");

        let env = policy.toolchain_env();
        assert_eq!(
            env.iter()
                .find(|(name, _)| name == "CARGO_HOME")
                .map(|(_, value)| value.as_str()),
            cargo_home.to_str()
        );
        assert_eq!(
            env.iter()
                .find(|(name, _)| name == "RUSTUP_HOME")
                .map(|(_, value)| value.as_str()),
            rustup_home.path().to_str()
        );
        assert!(cargo_home.is_dir());
    }

    /// `open <path>` talks to Launch Services, not the filesystem. Without
    /// these two rights it fails with `kLSApplicationNotFoundErr` / `-54`
    /// even though the file is readable.
    #[test]
    fn profile_allows_launch_services_handoff() {
        let ws = workspace();
        let profile = seatbelt_profile(&SandboxPolicy::for_workspace(ws.path())).unwrap();
        assert!(profile.contains("(allow appleevent-send)"));
        assert!(profile.contains("(allow user-preference-read)"));
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
        assert!(!profile.contains("(allow file-write* (subpath \"/private/var/folders\")"));
        assert!(!profile.contains("(allow file-write* (subpath \"/private/tmp\")"));

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

    #[test]
    fn startup_message_includes_the_install_line() {
        let linux = Unavailable::MissingDependency("bubblewrap").startup_message();
        assert!(
            linux.contains("sandbox unavailable: bubblewrap not found"),
            "{linux}"
        );
        assert!(linux.contains("sudo apt install bubblewrap"), "{linux}");
        assert!(linux.contains("brew install bubblewrap"), "{linux}");

        let windows = Unavailable::UnsupportedPlatform.startup_message();
        assert!(windows.contains("run under WSL2 on Windows"), "{windows}");
        assert!(
            windows.contains("Inside WSL2, install bubblewrap: sudo apt install bubblewrap"),
            "{windows}"
        );

        let macos = Unavailable::MissingDependency("sandbox-exec").startup_message();
        assert!(macos.contains("sandbox-exec not found"), "{macos}");
        assert!(macos.contains("/usr/bin/sandbox-exec"), "{macos}");
        assert!(!macos.contains("brew install bubblewrap"), "{macos}");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn wrap_shell_command_produces_a_sandbox_exec_invocation() {
        if availability().is_err() {
            eprintln!("skipping: this host cannot confine processes");
            return;
        }
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
    fn network_is_unshared_and_user_homes_are_masked() {
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
        assert!(ro_root, "system paths stay readable but not writable");

        // The trees where user data and credentials live are masked with a
        // private tmpfs, so `cat ~/.ssh/...` sees an empty directory rather
        // than a secret. Masked *before* the workspace bind, so a workspace
        // under /home or /root is re-exposed — the same ordering rule as a
        // workspace under /tmp.
        let workspace_bind = args.iter().position(|a| a == "--bind").unwrap();
        for home in ["/home", "/root"] {
            let mask = args
                .windows(2)
                .position(|w| w[0] == "--tmpfs" && w[1] == home)
                .expect("the home tree must be masked");
            assert!(
                mask < workspace_bind,
                "the {home} mask must precede the workspace bind, or a \
                 workspace inside {home} would be hidden"
            );
        }
    }

    #[test]
    fn masked_home_reexposes_only_selected_toolchain_paths() {
        let ws = workspace();
        let mut policy = SandboxPolicy::for_workspace(ws.path());
        policy.toolchain_read_paths = vec![
            PathBuf::from("/home/forge-user/.cargo/bin"),
            PathBuf::from("/home/forge-user/.rustup"),
        ];
        policy.toolchain_executable_paths = vec![PathBuf::from("/home/forge-user/.local/bin/gh")];
        let Some((_, args)) = bubblewrap_invocation("sh", "cargo build", &policy) else {
            return;
        };

        for path in [
            "/home/forge-user/.cargo/bin",
            "/home/forge-user/.rustup",
            "/home/forge-user/.local/bin/gh",
        ] {
            assert!(
                args.windows(3)
                    .any(|window| window[0] == "--ro-bind-try" && window[1] == path),
                "selected toolchain path must be rebound: {path}"
            );
        }
        assert!(
            args.iter().any(|arg| arg == "/home/forge-user/.cargo"),
            "masked home parents must be recreated before the bind"
        );
        assert!(
            !args.iter().any(|arg| arg == "/home/forge-user/.config"),
            "unrelated home configuration must remain masked"
        );
    }

    /// The read boundary, pinned at the profile level (the kernel-level
    /// counterpart lives in `tests/sandbox_enforcement.rs`). The old blanket
    /// `(allow file-read*)` granted every path on the host — the exact hole
    /// that let `cat ~/.ssh/...` beat the `read_file` boundary. Reads must now
    /// be the workspace/session-temp boundary plus OS-owned runtime paths.
    #[test]
    fn reads_are_confined_to_workspace_session_temp_and_system_paths() {
        let ws = workspace();
        let profile = seatbelt_profile(&SandboxPolicy::for_workspace(ws.path())).unwrap();
        let read_rule = primary_file_read_rule(&profile);

        assert!(
            !profile.contains("(allow file-read*)\n"),
            "a blanket file-read rule would re-open ~/.ssh and ~/.aws"
        );

        let canonical = ws.path().canonicalize().unwrap();
        let canonical = canonical.to_str().unwrap();
        assert!(
            profile.contains(&format!("(subpath \"{canonical}\")")),
            "the workspace must be readable"
        );

        // User-writable trees are not in the read allowlist, in any spelling.
        for tree in [
            "\"/Users\"",
            "\"/private/var/folders\"",
            "\"/Volumes\"",
            "\"/Network\"",
            "\"/home\"",
            "\"/root\"",
        ] {
            assert!(
                !read_rule.contains(&format!("(subpath {tree})")),
                "the read allowlist must not include {tree}"
            );
        }

        // The runtime paths a process needs to start stay readable.
        for runtime in [
            "(subpath \"/System\")",
            "(subpath \"/usr\")",
            "(subpath \"/bin\")",
            "(subpath \"/sbin\")",
            "(subpath \"/private/etc\")",
            "(literal \"/dev/null\")",
            "(literal \"/dev/urandom\")",
        ] {
            assert!(
                profile.contains(runtime),
                "a process needs to read {runtime}"
            );
        }
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
    /// A grant can outlive the proxy that backs it. Binding a source that no
    /// longer exists makes bwrap refuse to start, so the command never runs and
    /// the caller is told "blocked by the sandbox" — an
    /// explanation that has nothing to do with a dead egress proxy.
    #[test]
    fn a_grant_whose_socket_has_vanished_is_dropped_rather_than_bound() {
        let ws = workspace();
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("egress.sock");
        // Deliberately never created: this is the proxy-died-first case.

        let policy = SandboxPolicy::for_workspace(ws.path()).with_egress_socket(&sock);
        let Some((_, args)) = bubblewrap_invocation("sh", "true", &policy) else {
            return;
        };

        assert!(
            !args.iter().any(|a| a == sock.to_str().unwrap()),
            "a socket that does not exist must not be bound: {args:?}"
        );
        // Still fails closed: no relay, and the network stays unshared.
        assert!(args.iter().any(|a| a == "--unshare-net"), "{args:?}");
    }

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
    const CREDENTIAL: &[&str] = &[
        "The token in default is invalid",
        "Requires authentication (HTTP 401)",
    ];

    if output.contains(crate::egress::SANDBOX_DENIED_REASON) {
        return Some(
            "blocked by the sandbox: the destination host is not allowed by the personal \
             host(...) network permissions.",
        );
    }
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
    if CREDENTIAL.iter().any(|sig| output.contains(sig)) {
        return Some(CREDENTIAL_EXPLANATION);
    }

    // Linux reports a blocked write as a missing path, not as a permission
    // error: masked and unbound paths genuinely do not exist inside the
    // sandbox. bash says "No such file or directory"; dash (Debian/Ubuntu
    // `sh`, and therefore the `exec_command` default) says "Directory
    // nonexistent". Either string is also a common legitimate error, so
    // matching on it alone would blame the sandbox for every typo — which is
    // worse than saying nothing.
    //
    // The distinguishing fact is *which* path is missing. Inside the sandbox
    // an absolute path outside the workspace really is absent, and that is the
    // boundary. A missing file inside the workspace is an ordinary mistake and
    // is left alone.
    const MISSING: &[&str] = &["No such file or directory", "Directory nonexistent"];
    if MISSING.iter().any(|sig| output.contains(sig))
        && mentions_path_outside(output, workspace_root)
    {
        return Some(FILESYSTEM_EXPLANATION);
    }
    None
}

const FILESYSTEM_EXPLANATION: &str =
    "blocked by the sandbox: filesystem access is confined to the workspace \
     and the session temp directory, and .git/.forge are read-only inside the \
     workspace. On Linux a path outside the boundary does not exist inside \
     the sandbox, so it reports as missing rather than forbidden; on macOS it \
     reports as denied. This is not a file-permission problem on disk.";

const CREDENTIAL_EXPLANATION: &str =
    "blocked by the sandbox: credentials live in the host secret store, \
     which confined processes cannot read. This is not an invalid token — \
     a host(...) grant projects HTTPS identity for that host into the spawn.";

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
    fn a_proxy_policy_denial_is_not_reported_as_github_forbidden() {
        let out = "failed to authenticate via web browser: Post \"https://github.com/login/device/code\": Forge Sandbox Denied";
        let explained = explain_denial(out, ws().path()).expect("must be recognised as a denial");
        assert!(explained.contains("host(...) network permissions"));
    }

    #[test]
    fn a_secret_store_failure_is_not_reported_as_bad_credentials() {
        let out = "X Failed to log in\n  - The token in default is invalid.\n";
        let explained = explain_denial(out, ws().path()).expect("must be recognised as a denial");
        assert!(
            explained.contains("secret store"),
            "the 401 is the boundary wearing auth's clothes: {explained}"
        );
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
        assert!(explained.contains("does not exist inside the sandbox"));
    }

    /// dash, the default `sh` on Debian/Ubuntu and therefore `exec_command`'s
    /// default shell, uses a different ENOENT string than bash. Captured from
    /// the Linux CI failure of `exec_command_cannot_write_outside_the_workspace`.
    #[test]
    fn dash_missing_path_outside_the_workspace_is_named_as_the_boundary() {
        let ws = ws();
        let out = "sh: 1: cannot create /tmp/.tmpABCDEF/nope.txt: Directory nonexistent";
        let explained = explain_denial(out, ws.path()).expect("must be recognised");
        assert!(explained.contains("does not exist inside the sandbox"));
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

    #[test]
    fn dash_missing_path_inside_the_workspace_is_left_alone() {
        let ws = ws();
        let inside = ws.path().canonicalize().unwrap().join("typo.txt");
        let out = format!(
            "sh: 1: cannot create {}: Directory nonexistent",
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

    #[test]
    fn session_temp_sets_all_standard_temp_variables() {
        let ws = workspace();
        let scratch = workspace();
        let policy = SandboxPolicy::for_workspace(ws.path()).with_session_tmp(scratch.path());
        let env = temp_env(&policy);

        assert_eq!(env.len(), 3);
        for name in ["TMPDIR", "TMP", "TEMP"] {
            assert!(env
                .iter()
                .any(|(key, value)| { *key == name && *value == scratch.path() }));
        }
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
    /// Shared with the in-process proxy so a spawn can read hosts it refused.
    /// `None` only in tests that construct a grant without a live proxy.
    pub control: Option<crate::egress::EgressShared>,
}

impl EgressGrant {
    pub fn take_denied_host(&self) -> Option<String> {
        self.control.as_ref().and_then(|c| c.take_denied_host())
    }

    /// Whether the live proxy (or a test double) currently permits `host`.
    pub fn permits_host(&self, host: &str) -> bool {
        self.control
            .as_ref()
            .is_some_and(|control| control.permits_host(host))
    }

    pub fn allow_patterns(&self) -> Vec<String> {
        self.control
            .as_ref()
            .map(|control| control.allow_patterns())
            .unwrap_or_default()
    }
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
