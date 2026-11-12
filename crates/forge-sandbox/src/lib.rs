//! OS-level confinement for agent-spawned processes.
//!
//! This is the *enforcement* half of the permission model. The other half,
//! whether the user is asked at all, lives in `forge-governance` and never
//! reaches a process.
//!
//! This crate depends on nothing else in the workspace, so every crate that
//! starts a process, including the ones at the bottom of the graph that run
//! git for Forge itself, can confine it with the same policy.
//!
//! Confinement is applied **at spawn**. A process that started unsandboxed
//! stays unsandboxed for its whole life.
//!
//! Scope (macOS Seatbelt):
//!
//! * reads: the whole disk except a deny-list (credentials, shell history,
//!   the user's own folders, application state, the per-user temp tree,
//!   mounted volumes). The session's own trees are reopened.
//! * writes: the workspace, the granted session scratch directory, and the
//!   shared tool cache.
//! * network: denied. Egress exists only via the proxy, and only when a
//!   grant created it.

pub mod git;

use std::env;
use std::ffi::{OsStr, OsString};
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

/// Environment additions shared by pipe and PTY shell spawns.
pub fn shell_env(policy: &SandboxPolicy) -> Vec<(String, OsString)> {
    #[allow(unused_mut)]
    let mut env = temp_env(policy)
        .into_iter()
        .map(|(name, value)| (name.to_string(), value.as_os_str().to_os_string()))
        .collect::<Vec<_>>();

    #[cfg(target_os = "macos")]
    if let Some(session_tmp) = policy.session_tmp.as_deref() {
        if let Some(bin_dir) = ensure_mktemp_wrapper(session_tmp) {
            let mut path = vec![bin_dir];
            if let Some(existing) = env::var_os("PATH") {
                path.extend(env::split_paths(&existing));
            }
            if let Ok(path) = env::join_paths(path) {
                env.push(("PATH".into(), path));
            }
        }
    }

    env
}

#[cfg(target_os = "macos")]
fn ensure_mktemp_wrapper(session_tmp: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;

    // macOS mktemp ignores TMPDIR when no template is supplied. Keep the
    // compatibility helper inside the already-granted scratch directory so
    // this does not turn the host temp tree into a writable root.
    let bin_dir = session_tmp.join(".forge-bin");
    std::fs::create_dir_all(&bin_dir).ok()?;
    let wrapper = bin_dir.join("mktemp");
    if !wrapper.exists() {
        std::fs::write(
            &wrapper,
            "#!/bin/sh\nif [ \"$#\" -eq 0 ]; then\n    exec /usr/bin/mktemp \"$TMPDIR/tmp.XXXXXXXXXX\"\nfi\nif [ \"$#\" -eq 1 ] && [ \"$1\" = \"-d\" ]; then\n    exec /usr/bin/mktemp -d \"$TMPDIR/tmp.XXXXXXXXXX\"\nfi\nexec /usr/bin/mktemp \"$@\"\n",
        )
        .ok()?;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).ok()?;
    }
    wrapper.is_file().then_some(bin_dir)
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
        return match bwrap_path() {
            None => Err(Unavailable::MissingDependency("bubblewrap")),
            Some(bwrap) if bwrap_applicable(bwrap) => Ok(()),
            Some(_) => Err(Unavailable::CannotApply),
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
/// Whether bubblewrap can actually create the sandbox's namespaces.
///
/// The binary existing is not enough: Ubuntu 24.04 restricts unprivileged
/// user namespaces through AppArmor, and WebKitGTK installs bwrap there as a
/// dependency. bwrap then fails at spawn ("loopback: Failed RTM_NEWADDR"), so
/// every confined command, Forge's own local git included, failed with an
/// unrelated error. Probed once with the namespaces the real wrapper uses and
/// cached, like [`seatbelt_applicable`].
fn bwrap_applicable(bwrap: &str) -> bool {
    static APPLICABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *APPLICABLE.get_or_init(|| {
        std::process::Command::new(bwrap)
            .args([
                "--ro-bind",
                "/",
                "/",
                "--dev",
                "/dev",
                "--proc",
                "/proc",
                "--unshare-net",
                "--",
                "true",
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    })
}

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
    /// When true, the whole git dir is writable, hooks and config included
    /// (see [`Self::with_git_open`]). Default is false: a bare policy keeps
    /// git read-only.
    git_writable: bool,
    /// Git administrative directories that live *outside* the workspace root
    /// because the workspace is a linked worktree (`git worktree add`) — the
    /// shape a managed session lives in. Resolved from the workspace's `.git`
    /// link file when a git spawn opts into writable `.git`. Without that
    /// grant they stay outside the sandbox entirely.
    linked_git_dirs: Vec<PathBuf>,
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
    /// Whether the workspace root is writable. True for every shell spawn;
    /// a long-lived MCP server can opt into a read-only workspace through
    /// [`Self::with_read_only_workspace`].
    workspace_writable: bool,
    /// The user's home directory, which the read deny-list is built from.
    /// `None` (no `HOME`) leaves no home-relative rule to write.
    home: Option<PathBuf>,
    /// Further loopback ports the process may connect to, besides the egress
    /// proxy: the SSH broker a confined git speaks to.
    extra_loopback_ports: Vec<u16>,
    /// Further directories the process may read and write: where a new
    /// worktree is created, or one being removed.
    extra_write_paths: Vec<PathBuf>,
    /// A Forge-owned directory for package-manager caches, writable and kept
    /// across sessions. See [`Self::with_tool_cache`].
    tool_cache: Option<PathBuf>,
}

/// Whether `dir` may be granted to sandboxed commands for writing (D5,
/// D8): `Err` says why not.
///
/// Never a directory whose contents something later runs outside the
/// sandbox, holds credentials or holds Forge's own configuration: not the
/// home directory or anything above it, no `PATH` directory, no launch
/// agents, no credential store, no system directory, and no directory
/// containing one of those.
pub fn grantable_write_dir(dir: &Path) -> Result<PathBuf, String> {
    let dir = dir
        .canonicalize()
        .map_err(|error| format!("{} cannot be resolved: {error}", dir.display()))?;
    if !dir.is_dir() {
        return Err(format!("{} is not a directory", dir.display()));
    }
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| resolved(&home));
    let mut protected: Vec<PathBuf> = [
        "/System",
        "/usr",
        "/bin",
        "/sbin",
        "/etc",
        "/private/etc",
        "/Library",
        "/Applications",
        "/opt",
    ]
    .iter()
    .map(|path| resolved(Path::new(path)))
    .collect();
    if let Some(path) = env::var_os("PATH") {
        protected.extend(
            env::split_paths(&path)
                .filter(|dir| dir.is_absolute())
                .map(|dir| resolved(&dir)),
        );
    }
    if let Some(home) = &home {
        if dir == *home || home.starts_with(&dir) {
            return Err("the home directory and the directories above it are never granted".into());
        }
        protected.extend(HOME_SECRETS.iter().map(|name| resolved(&home.join(name))));
        for name in [
            "Library/LaunchAgents",
            "Library/Application Support/forge",
            "Library/Preferences",
            ".config/forge",
            ".local/bin",
            ".cargo/bin",
            "bin",
        ] {
            protected.push(resolved(&home.join(name)));
        }
    }
    if let Some(hit) = protected
        .iter()
        .find(|protected| dir.starts_with(protected) || protected.starts_with(&dir))
    {
        return Err(format!(
            "{} is or contains {}, which is never granted to sandboxed commands",
            dir.display(),
            hit.display()
        ));
    }
    Ok(dir)
}

/// Locations under the home directory that hold credentials or shell
/// history. A confined process can never read them, whatever it was asked to
/// run: reading one and sending it out is the attack the sandbox exists to
/// stop, and no build needs them.
const HOME_SECRETS: &[&str] = &[
    ".ssh",
    ".aws",
    ".azure",
    ".gnupg",
    ".kube",
    ".oci",
    ".fly",
    ".wrangler",
    ".1password",
    ".password-store",
    ".age",
    ".docker/config.json",
    ".netrc",
    ".npmrc",
    ".pypirc",
    ".pgpass",
    ".my.cnf",
    ".mylogin.cnf",
    ".boto",
    ".s3cfg",
    ".vault-token",
    ".git-credentials",
    ".cargo/credentials",
    ".cargo/credentials.toml",
    ".gem/credentials",
    ".m2/settings-security.xml",
    ".terraform.d/credentials.tfrc.json",
    ".config/git/credentials",
    ".config/gh",
    ".config/gcloud",
    ".config/op",
    ".config/hub",
    ".config/sops",
    ".config/age",
    ".config/doctl",
    ".config/stripe",
    ".config/configstore",
    ".local/share/keyrings",
    ".zsh_history",
    ".zsh_sessions",
    ".bash_history",
    ".python_history",
    ".node_repl_history",
    ".psql_history",
    ".mysql_history",
    ".lesshst",
];

/// The user's own files. Nothing a toolchain needs lives here, and a
/// workspace inside one of them is reopened by the workspace rule.
const HOME_PERSONAL: &[&str] = &[
    "Documents",
    "Desktop",
    "Downloads",
    "Pictures",
    "Movies",
    "Music",
];

/// `~/Library` is application state: keychains, browser profiles, mail,
/// messages, cloud storage, and Forge's own credential store. It is denied
/// as a whole, except the parts toolchains install into or cache under.
const LIBRARY_READABLE: &[&str] = &[
    "Caches",
    "Developer",
    "Android",
    "Java",
    "Python",
    "pnpm",
    "org.swift.swiftpm",
    "Fonts",
];

/// One entry of the read deny-list: `path` and everything below it, except
/// the `except` subtrees.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReadDeny {
    path: PathBuf,
    except: Vec<PathBuf>,
}

/// `path` as the kernel will see it: resolved when it exists (Seatbelt
/// matches the resolved path, so a rule on an unresolved one never applies),
/// as written when it does not.
fn resolved(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// As [`resolved`], for a path whose tail may not exist yet: the deepest
/// existing ancestor is resolved and the rest appended.
fn resolved_lenient(path: &Path) -> PathBuf {
    let mut missing = Vec::new();
    let mut existing = path;
    loop {
        if let Ok(canonical) = existing.canonicalize() {
            return missing
                .iter()
                .rev()
                .fold(canonical, |path, part| path.join(part));
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                missing.push(name.to_os_string());
                existing = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

impl SandboxPolicy {
    /// The default policy for an agent-spawned process in `workspace_root`.
    pub fn for_workspace(workspace_root: impl AsRef<Path>) -> Self {
        let workspace_root = workspace_root.as_ref().to_path_buf();
        let linked_git_dirs = linked_worktree_git_dirs(&workspace_root);
        Self {
            workspace_root,
            session_tmp: None,
            egress_proxy_port: None,
            egress_socket: None,
            git_writable: false,
            linked_git_dirs,
            toolchain_read_paths: Vec::new(),
            toolchain_executable_paths: Vec::new(),
            rustup_home: None,
            workspace_writable: true,
            home: env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|home| home.is_absolute()),
            tool_cache: None,
            extra_write_paths: Vec::new(),
            extra_loopback_ports: Vec::new(),
        }
    }

    /// Open the whole git dir for writing, hooks and config included.
    ///
    /// Safe only because nothing unconfined runs git on this repository:
    /// a hook or a config key naming a program runs inside the same sandbox
    /// as whatever wrote it.
    pub fn with_git_open(mut self) -> Self {
        self.git_writable = true;
        self
    }

    /// Also let the process connect to `port` on loopback. For a service
    /// Forge itself runs for this process; never a way onto the network.
    pub fn with_loopback_port(mut self, port: u16) -> Self {
        if port != 0 && !self.extra_loopback_ports.contains(&port) {
            self.extra_loopback_ports.push(port);
        }
        self
    }

    /// Also let the process read and write `path`, which need not exist yet.
    pub fn with_write_path(mut self, path: impl AsRef<Path>) -> Self {
        let path = resolved_lenient(path.as_ref());
        if !is_unscoped_runtime_root(&path) && !self.extra_write_paths.contains(&path) {
            self.extra_write_paths.push(path);
        }
        self
    }

    /// Build the read deny-list against `home` instead of `$HOME`.
    pub fn with_home(mut self, home: impl AsRef<Path>) -> Self {
        self.home = Some(home.as_ref().to_path_buf());
        self
    }

    /// Make `dir` writable and point package managers' caches at it (see
    /// [`Self::toolchain_env`]).
    ///
    /// The user's own caches (`~/.cargo/registry`, `~/.npm`) stay read-only:
    /// what is unpacked there is compiled and run later, outside the
    /// sandbox, so a confined process that could write them could plant code
    /// that runs unconfined. A cache only confined processes use has no such
    /// reader, and keeping it across sessions means a dependency is fetched
    /// once, not once per session.
    pub fn with_tool_cache(mut self, dir: impl AsRef<Path>) -> Self {
        self.tool_cache = Some(dir.as_ref().to_path_buf());
        self
    }

    pub fn tool_cache(&self) -> Option<&Path> {
        self.tool_cache.as_deref()
    }

    /// What a confined process may not read: credentials, the user's own
    /// files, application state, other processes' temp files, mounted
    /// volumes. Everything else on the disk is readable, so any installed
    /// toolchain works without being named.
    fn read_denies(&self) -> Vec<ReadDeny> {
        let mut denies = Vec::new();
        let mut push = |path: PathBuf, except: Vec<PathBuf>| {
            let entry = ReadDeny {
                path: resolved(&path),
                except: except.iter().map(|path| resolved(path)).collect(),
            };
            if !denies.contains(&entry) {
                denies.push(entry);
            }
        };
        if let Some(home) = &self.home {
            for name in HOME_SECRETS.iter().chain(HOME_PERSONAL) {
                push(home.join(name), Vec::new());
            }
            let library = home.join("Library");
            push(
                library.clone(),
                LIBRARY_READABLE
                    .iter()
                    .map(|name| library.join(name))
                    .collect(),
            );
        }
        // The per-user temp tree is shared by every process the user runs.
        // Only the session's own scratch directory inside it is reopened.
        let temp = env::temp_dir();
        if temp.is_absolute() && !is_unscoped_runtime_root(&resolved(&temp)) {
            push(temp, Vec::new());
        }
        push(PathBuf::from("/Volumes"), Vec::new());
        denies
    }

    /// The default location of the shared tool cache:
    /// `~/Library/Caches/forge/sandbox`. `None` without a home directory or
    /// when it cannot be created.
    pub fn default_tool_cache() -> Option<PathBuf> {
        let home = env::var_os("HOME").map(PathBuf::from)?;
        if !home.is_absolute() {
            return None;
        }
        let dir = home.join("Library/Caches/forge/sandbox");
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir)
    }

    /// Mount the workspace read-only: the process can read it but cannot
    /// create, modify or delete anything under it. A granted session scratch
    /// directory stays writable.
    pub fn with_read_only_workspace(mut self) -> Self {
        self.workspace_writable = false;
        self
    }

    pub fn workspace_writable(&self) -> bool {
        self.workspace_writable
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

    /// Also let the process read and run one executable file, such as
    /// Forge's own binary, whose helper subcommands (the git proxy, the SSH
    /// and signing brokers) run inside the sandbox. On Linux `/home` is
    /// masked, so a Forge installed under it is otherwise not there at all.
    /// Call after [`Self::with_command_access`], which replaces this list.
    pub fn with_executable(mut self, path: impl AsRef<Path>) -> Self {
        add_existing_path(
            &mut self.toolchain_executable_paths,
            path.as_ref().to_path_buf(),
        );
        self
    }

    /// Child environment that sends package managers' writes to the tool
    /// cache (or, without one, to the session scratch directory). No
    /// environment is changed for an unconfined retry, which should use the
    /// user's normal configuration.
    pub fn toolchain_env(&self) -> Vec<(String, String)> {
        let mut env = Vec::new();
        let Some(base) = self
            .tool_cache
            .clone()
            .or_else(|| self.session_tmp.as_ref().map(|tmp| tmp.join("tool-cache")))
        else {
            return env;
        };
        for (name, relative) in [
            ("CARGO_HOME", "cargo"),
            ("npm_config_cache", "npm"),
            ("npm_config_store_dir", "pnpm-store"),
            ("YARN_CACHE_FOLDER", "yarn"),
            ("BUN_INSTALL_CACHE_DIR", "bun"),
            ("PIP_CACHE_DIR", "pip"),
            ("UV_CACHE_DIR", "uv"),
            ("UV_TOOL_DIR", "uv-tools"),
            ("UV_PYTHON_INSTALL_DIR", "uv-python"),
            ("GOMODCACHE", "go/mod"),
            ("GOCACHE", "go/build"),
            ("XDG_CACHE_HOME", "xdg"),
        ] {
            let dir = base.join(relative);
            let _ = std::fs::create_dir_all(&dir);
            env.push((name.to_string(), dir.to_string_lossy().into_owned()));
        }
        // Cargo reads its user configuration from `CARGO_HOME`, which no
        // longer is `~/.cargo`. Link the user's config in so registries,
        // aliases and build settings still apply. Credentials are separate
        // files and stay behind.
        if let Some(home) = &self.home {
            let link = base.join("cargo/config.toml");
            let config = home.join(".cargo/config.toml");
            if config.is_file() && std::fs::symlink_metadata(&link).is_err() {
                #[cfg(unix)]
                let _ = std::os::unix::fs::symlink(&config, &link);
            }
        }
        if let Some(npmrc) = self.sanitized_npmrc() {
            env.push((
                "NPM_CONFIG_USERCONFIG".to_string(),
                npmrc.to_string_lossy().into_owned(),
            ));
        }
        if let Some(rustup_home) = &self.rustup_home {
            env.push((
                "RUSTUP_HOME".to_string(),
                rustup_home.to_string_lossy().into_owned(),
            ));
        }
        env
    }

    /// `~/.npmrc` usually holds a registry token, so it is on the deny-list,
    /// and npm fails outright when it cannot read its user config. Give npm a
    /// copy in the session scratch directory with the credential lines
    /// removed: registry and proxy settings survive, tokens do not enter the
    /// sandbox.
    fn sanitized_npmrc(&self) -> Option<PathBuf> {
        let source = self.home.as_ref()?.join(".npmrc");
        let body = std::fs::read_to_string(source).ok()?;
        let kept: String = body
            .lines()
            .filter(|line| {
                let key = line.split('=').next().unwrap_or("").to_ascii_lowercase();
                !(key.contains("auth") || key.contains("token") || key.contains("password"))
            })
            .flat_map(|line| [line, "\n"])
            .collect();
        let target = self.session_tmp.as_ref()?.join("npmrc");
        std::fs::write(&target, kept).ok()?;
        Some(target)
    }

    /// Permit reading (and mapping executables from) the directory tree at
    /// `path`, for a caller-chosen location outside the default profile — an
    /// MCP server's install directory or a package-manager cache.
    ///
    /// Refused (`Err` with the reason) when `path` is relative, is a broad
    /// root (`/`, `/home`, `/Users`, `/root`), or is the home directory or
    /// one of its ancestors: those would expose `~/.ssh` and every other
    /// secret under `$HOME`, which must stay unreadable. A path that does
    /// not exist is accepted and contributes nothing.
    pub fn with_read_path(self, path: &Path) -> Result<Self, String> {
        let home = env::var_os("HOME").map(PathBuf::from);
        self.with_read_path_under_home(path, home.as_deref())
    }

    fn with_read_path_under_home(
        mut self,
        path: &Path,
        home: Option<&Path>,
    ) -> Result<Self, String> {
        if !path.is_absolute() {
            return Err(format!("`{}` is not an absolute path", path.display()));
        }
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if is_unscoped_runtime_root(path) || is_unscoped_runtime_root(&canonical) {
            return Err(format!("`{}` is too broad to expose", path.display()));
        }
        if let Some(home) = home.filter(|home| home.is_absolute()) {
            let canonical_home = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
            if home.starts_with(path) || canonical_home.starts_with(&canonical) {
                return Err(format!(
                    "`{}` would expose the whole home directory",
                    path.display()
                ));
            }
        }
        add_existing_path(&mut self.toolchain_read_paths, path.to_path_buf());
        Ok(self)
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
        if !git_writable {
            paths.push(root.join(".git"));
        }
        paths.push(root.join(".forge"));
        paths
    }
}

/// Resolve the git administrative directories of a workspace whose `.git` is a
/// link file pointing elsewhere — a linked worktree, the shape a managed
/// session lives in. The object database and refs then sit in a gitdir outside
/// the sandbox root, so without this a confined `git status` cannot read them.
///
/// Returns the resolved, canonical directories that exist and lie outside the
/// workspace: the worktree's own gitdir and, when different, the shared common
/// dir. Empty for a normal repository (`.git` is a directory) or a
/// non-repository.
fn linked_worktree_git_dirs(workspace: &Path) -> Vec<PathBuf> {
    let gitlink = workspace.join(".git");
    let Ok(metadata) = std::fs::symlink_metadata(&gitlink) else {
        return Vec::new();
    };
    if metadata.file_type().is_dir() {
        return Vec::new();
    }
    let Ok(body) = std::fs::read_to_string(&gitlink) else {
        return Vec::new();
    };
    let Some(target) = body
        .lines()
        .find_map(|line| line.strip_prefix("gitdir:"))
        .map(str::trim)
        .filter(|line| !line.is_empty())
    else {
        return Vec::new();
    };
    let gitdir = if Path::new(target).is_absolute() {
        PathBuf::from(target)
    } else {
        workspace.join(target)
    };
    // The pointer is a file the confined process can rewrite, and what it
    // names becomes writable, so it is not believed on its own. Git lays a
    // linked worktree out as `<common>/worktrees/<name>`, holding a `gitdir`
    // file that names this worktree's `.git` back. A directory the process
    // cannot write cannot be made to carry that back-reference, so a pointer
    // redirected at one is refused; the common dir is taken from the layout,
    // never from the `commondir` file, which the process can also rewrite.
    let Ok(gitdir) = gitdir.canonicalize() else {
        return Vec::new();
    };
    let Ok(workspace) = workspace.canonicalize() else {
        return Vec::new();
    };
    let names_this_worktree = std::fs::read_to_string(gitdir.join("gitdir"))
        .ok()
        .and_then(|back| PathBuf::from(back.trim()).canonicalize().ok())
        .is_some_and(|back| back == workspace.join(".git"));
    let common = gitdir
        .parent()
        .filter(|parent| parent.file_name() == Some(OsStr::new("worktrees")))
        .and_then(Path::parent)
        .map(Path::to_path_buf);
    let granted = match (names_this_worktree, common) {
        (true, Some(common)) => vec![gitdir, common],
        // A submodule's git dir (`<super>/.git/modules/<name>`) names its
        // checkout through `core.worktree` instead. The same argument holds:
        // a directory the process cannot write cannot be made to carry it.
        _ if submodule_gitdir_names(&gitdir, &workspace) => vec![gitdir],
        _ => return Vec::new(),
    };
    let mut dirs = Vec::new();
    for canonical in granted {
        if canonical != workspace
            && !path_is_under(&canonical, &workspace)
            && !dirs.contains(&canonical)
        {
            dirs.push(canonical);
        }
    }
    dirs
}

/// Whether `gitdir`'s own config says its checkout is `workspace`
/// (`core.worktree`, written by git when it sets a submodule up).
fn submodule_gitdir_names(gitdir: &Path, workspace: &Path) -> bool {
    let Ok(config) = std::fs::read_to_string(gitdir.join("config")) else {
        return false;
    };
    config
        .lines()
        .filter_map(|line| line.trim().strip_prefix("worktree"))
        .filter_map(|rest| rest.trim_start().strip_prefix('='))
        .map(|value| PathBuf::from(value.trim().trim_matches('"')))
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                gitdir.join(path)
            }
        })
        .any(|path| path.canonicalize().is_ok_and(|path| path == workspace))
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

#[cfg(target_os = "macos")]
fn xcrun_cache_regex() -> Option<String> {
    let temp_dir = std::env::temp_dir().canonicalize().ok()?;
    let raw = temp_dir.to_str()?;
    let mut pattern = String::from("^");
    for ch in raw.chars() {
        if matches!(
            ch,
            '\\' | '.' | '^' | '$' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|'
        ) {
            pattern.push('\\');
        }
        pattern.push(ch);
    }
    pattern.push_str(r"/xcrun_db(-[A-Za-z0-9]+)?$");
    sbpl_literal(Path::new(&pattern))
}

/// One `(deny file-read* ...)` rule for a deny-list entry.
fn deny_read_rule(deny: &ReadDeny) -> Option<String> {
    let path = sbpl_literal(&deny.path)?;
    if deny.except.is_empty() {
        return Some(format!("(deny file-read* (subpath \"{path}\"))\n"));
    }
    let mut rule = format!("(deny file-read* (require-all (subpath \"{path}\")");
    for except in &deny.except {
        let except = sbpl_literal(except)?;
        rule.push_str(&format!(" (require-not (subpath \"{except}\"))"));
    }
    rule.push_str("))\n");
    Some(rule)
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
/// Apple Event sending remains denied so confined commands cannot drive
/// unconfined GUI apps.
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

    // Reads. The whole disk is readable except the deny-list: credentials,
    // the user's own files, application state, the shared temp tree, mounted
    // volumes. A toolchain installed anywhere therefore works without being
    // named, and nothing about the command being run decides what is
    // readable. Seatbelt matches the resolved path, so a symlink or a case
    // variant that leads into a denied tree is denied with it.
    //
    // In SBPL the last matching rule wins, so the order below is the policy:
    // allow everything, deny the list, reopen what this session owns, then
    // deny again whatever on the list sits inside a reopened tree.
    profile.push_str("(allow file-read* file-test-existence file-map-executable)\n");
    let denies = policy.read_denies();
    for deny in &denies {
        profile.push_str(&deny_read_rule(deny)?);
    }
    // Other projects' Forge state (session journals) is not this session's
    // to read; its own is reopened with the workspace below.
    profile.push_str("(deny file-read* (regex #\"/\\.forge/\"))\n");

    let mut reopened = vec![canonical_root.clone()];
    if let Some(tmp) = &policy.session_tmp {
        reopened.push(tmp.canonicalize().ok()?);
    }
    if let Some(cache) = &policy.tool_cache {
        reopened.push(cache.canonicalize().ok()?);
    }
    for path in &policy.toolchain_read_paths {
        reopened.push(path.canonicalize().ok()?);
    }
    reopened.extend(policy.linked_git_dirs.iter().cloned());
    reopened.extend(policy.extra_write_paths.iter().cloned());

    // A reopened tree inside a denied one (a workspace under `~/Documents`,
    // the scratch directory under the temp tree) is reached through denied
    // parents. Resolving a path stats each component, so the parents'
    // metadata, and only that, has to be visible.
    let mut ancestors: Vec<PathBuf> = Vec::new();
    for path in reopened
        .iter()
        .chain(denies.iter().flat_map(|deny| deny.except.iter()))
    {
        let mut parent = path.parent();
        while let Some(path) = parent {
            if ancestors.iter().any(|existing| existing == path) {
                break;
            }
            ancestors.push(path.to_path_buf());
            parent = path.parent();
        }
    }
    profile.push_str("(allow file-read-metadata");
    for path in &ancestors {
        let path = sbpl_literal(path)?;
        profile.push_str(&format!("\n  (literal \"{path}\")"));
    }
    profile.push_str(")\n");

    profile.push_str("(allow file-read* file-test-existence file-map-executable");
    for path in &reopened {
        let path = sbpl_literal(path)?;
        profile.push_str(&format!("\n  (subpath \"{path}\")"));
    }
    profile.push_str(")\n");

    #[cfg(target_os = "macos")]
    if policy.session_tmp.is_some() {
        if let Some(pattern) = xcrun_cache_regex() {
            profile.push_str(&format!(
                "(allow file-read* file-test-existence (regex #\"{pattern}\"))\n"
            ));
        }
    }

    // A workspace that contains a denied location (the home directory opened
    // as a project) must not reopen it.
    for deny in &denies {
        if reopened
            .iter()
            .any(|root| deny.path != *root && deny.path.starts_with(root))
        {
            profile.push_str(&deny_read_rule(deny)?);
        }
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
    for port in &policy.extra_loopback_ports {
        profile.push_str(&format!(
            "(allow network-outbound (remote ip \"localhost:{port}\"))\n"
        ));
    }
    // The gateway's Unix socket, for a client that is told to send plain
    // requests there instead of doing TLS itself. Named exactly; no other
    // socket on the host becomes reachable.
    if let Some(socket) = policy.egress_socket.as_deref().filter(|path| path.exists()) {
        let mut paths = vec![socket.to_path_buf()];
        if let Ok(resolved) = socket.canonicalize() {
            if resolved != socket {
                paths.push(resolved);
            }
        }
        for path in paths {
            let literal = sbpl_literal(&path)?;
            profile.push_str(&format!(
                "(allow network-outbound (remote unix-socket (path-literal \"{literal}\")))\n"
            ));
        }
    }

    // Writable: the workspace (unless the policy mounts it read-only), and a
    // session scratch directory if one was granted. Nothing else — in
    // particular not all of `$TMPDIR`.
    let mut writable = Vec::new();
    if policy.workspace_writable {
        writable.push(format!(" (subpath \"{root}\")"));
    }
    if let Some(tmp) = &policy.session_tmp {
        let tmp = sbpl_literal(&tmp.canonicalize().ok()?)?;
        writable.push(format!(" (subpath \"{tmp}\")"));
    }
    if let Some(cache) = &policy.tool_cache {
        let cache = sbpl_literal(&cache.canonicalize().ok()?)?;
        writable.push(format!(" (subpath \"{cache}\")"));
    }
    // Linked-worktree metadata is always readable for repository discovery,
    // but only writable for commands that explicitly opt into Git writes.
    if policy.git_writable && policy.workspace_writable {
        for path in &policy.linked_git_dirs {
            let path = sbpl_literal(path)?;
            writable.push(format!(" (subpath \"{path}\")"));
        }
    }
    // An `(allow file-write*)` with no filter would allow *every* write, so a
    // policy with nothing writable must emit no allow rule at all.
    if !writable.is_empty() {
        profile.push_str("(allow file-write*");
        for filter in writable {
            profile.push_str(&filter);
        }
        profile.push_str(")\n");
    }

    #[cfg(target_os = "macos")]
    if policy.session_tmp.is_some() {
        if let Some(pattern) = xcrun_cache_regex() {
            // macOS developer tools create a small lookup cache in the host temp
            // root, ignoring TMPDIR. Permit only that generated namespace; the
            // rest of the shared temp tree remains outside the write boundary.
            profile.push_str(&format!("(allow file-write* (regex #\"{pattern}\"))\n"));
        }
    }

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

    // Paths the caller named explicitly come last, so they hold even inside
    // a carve-out above: Forge keeps managed worktrees under `.forge/`, and
    // removing one has to delete it.
    if !policy.extra_write_paths.is_empty() {
        profile.push_str("(allow file-write*");
        for path in &policy.extra_write_paths {
            let path = sbpl_literal(path)?;
            profile.push_str(&format!(" (subpath \"{path}\")"));
        }
        profile.push_str(")\n");
    }

    Some(profile)
}

#[cfg(test)]
fn primary_file_read_rule(profile: &str) -> &str {
    let (_, rule) = profile
        .split_once("(allow file-read* file-test-existence file-map-executable\n")
        .expect("profile must contain a rule reopening the session's own trees");
    match rule.find("))") {
        Some(idx) => &rule[..idx + 1],
        None => rule,
    }
}

#[cfg(test)]
fn primary_file_write_rule(profile: &str) -> &str {
    let (_, rule) = profile
        .split_once("(allow file-write*")
        .expect("profile must contain a file-write rule");
    match rule.find("))") {
        Some(idx) => &rule[..idx + 1],
        None => rule,
    }
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

/// Rewrite an arbitrary argv so it runs confined — the non-shell counterpart
/// of [`wrap_shell_command`], used for long-lived processes such as MCP
/// servers that are launched by program and arguments rather than by a shell
/// command line.
///
/// The same profile applies: filesystem per `policy`, network denied except
/// the egress proxy the policy names. `program` is resolved through `PATH`
/// by the sandbox launcher (`sandbox-exec` / `bwrap` both `execvp`).
///
/// Returns `None` exactly when [`wrap_shell_command`] would: callers must not
/// fall back to spawning unconfined.
pub fn wrap_command(
    program: &str,
    args: &[String],
    policy: &SandboxPolicy,
) -> Option<(String, Vec<String>)> {
    availability().ok()?;
    if cfg!(target_os = "linux") {
        return bubblewrap_invocation_for(Payload::Argv { program, args }, policy);
    }
    let profile = seatbelt_profile(policy)?;
    let mut argv = vec!["-p".to_string(), profile, program.to_string()];
    argv.extend(args.iter().cloned());
    Some(("/usr/bin/sandbox-exec".to_string(), argv))
}

/// What a bubblewrap invocation finally runs inside the namespace.
enum Payload<'a> {
    /// `shell -c command`, the shell tool's shape.
    Shell { shell: &'a str, command: &'a str },
    /// A program and its arguments, passed through without a shell.
    Argv {
        program: &'a str,
        args: &'a [String],
    },
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
    bubblewrap_invocation_for(Payload::Shell { shell, command }, policy)
}

fn bubblewrap_invocation_for(
    payload: Payload<'_>,
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
        // Everything outside the masks is already visible through the
        // read-only root. Rebinding it is not just redundant: binding a path
        // that is a symlink (`/usr/bin/go` -> the tool cache, alternatives)
        // makes bwrap fail with "Can't create file at ...", and the whole
        // command with it.
        .filter(|path| is_under_bwrap_mask(path))
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

    // The one writable place, re-exposed over the mask above — or, for a
    // read-only policy, re-exposed without write access.
    let root_bind = if policy.workspace_writable {
        "--bind"
    } else {
        "--ro-bind"
    };
    args.extend([root_bind.into(), root.clone(), root.clone()]);

    if let Some(tmp) = &policy.session_tmp {
        let tmp = tmp.canonicalize().ok()?.to_str()?.to_string();
        args.extend(["--bind".into(), tmp.clone(), tmp]);
    }

    // A linked worktree's git metadata lives outside the workspace root.
    // It is mounted read-only for repository discovery; git mutation spawns
    // opt into writable mounts.
    for dir in &policy.linked_git_dirs {
        let dir = dir.to_str()?.to_string();
        let bind = if policy.git_writable && policy.workspace_writable {
            "--bind"
        } else {
            "--ro-bind"
        };
        args.extend([bind.into(), dir.clone(), dir]);
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

    // Paths the caller named explicitly come last, so they hold even inside
    // a carve-out above (Forge keeps managed worktrees under `.forge/`), as
    // they do in the Seatbelt profile. Each is bound exactly: a grant opens
    // what was granted, not its parent. A path that does not exist yet is
    // skipped rather than widened to its parent, so creating it fails
    // closed; callers name the directory something is created in.
    for path in &policy.extra_write_paths {
        if path.is_dir() {
            let path = path.to_str()?.to_string();
            args.extend(["--bind".into(), path.clone(), path]);
        }
    }

    // A long-lived argv payload (an MCP server) must not outlive Forge: if
    // bwrap is killed, the sandboxed child is killed with it.
    if matches!(payload, Payload::Argv { .. }) {
        args.push("--die-with-parent".into());
    }

    args.extend(["--chdir".into(), root, "--".into()]);

    let (shell, command) = match payload {
        Payload::Shell { shell, command } => (shell, command),
        Payload::Argv {
            program,
            args: program_args,
        } => {
            if let Some((socat, socket)) = relay {
                // Same relay as the shell shape below, then `exec "$@"`
                // so the program replaces the shell and is never
                // re-parsed by it — its arguments stay argv elements.
                let script = format!(
                    "{socat} TCP-LISTEN:{port},bind=127.0.0.1,reuseaddr,fork \
                     UNIX-CONNECT:{socket} >/dev/null 2>&1 & \
                     for _ in 1 2 3 4 5 6 7 8 9 10; do \
                     {socat} -u OPEN:/dev/null TCP:127.0.0.1:{port} >/dev/null 2>&1 && break; \
                     sleep 0.1; done; exec \"$@\"",
                    port = SANDBOX_PROXY_PORT,
                    socket = shell_quote(&socket),
                );
                args.extend(["/bin/sh".into(), "-c".into(), script, "sh".into()]);
            }
            args.push(program.to_string());
            args.extend(program_args.iter().cloned());
            return Some((bwrap.to_string(), args));
        }
    };

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

/// Whether `path` lies under one of the trees the bubblewrap invocation
/// masks with a tmpfs, and so must be bound back to be visible.
fn is_under_bwrap_mask(path: &Path) -> bool {
    ["/run", "/tmp", "/home", "/root"]
        .iter()
        .any(|mask| path.starts_with(mask))
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

    #[test]
    fn read_paths_refuse_home_and_its_ancestors_but_accept_subdirectories() {
        let home = tempfile::tempdir().unwrap();
        let cache = home.path().join(".npm");
        std::fs::create_dir_all(&cache).unwrap();
        let policy = || SandboxPolicy::for_workspace(home.path().join("ws"));

        let accepted = policy()
            .with_read_path_under_home(&cache, Some(home.path()))
            .unwrap();
        assert!(accepted.toolchain_read_paths.contains(&cache));

        for refused in [
            home.path().to_path_buf(),
            home.path().parent().unwrap().to_path_buf(),
            PathBuf::from("/"),
            PathBuf::from("relative/dir"),
        ] {
            assert!(
                policy()
                    .with_read_path_under_home(&refused, Some(home.path()))
                    .is_err(),
                "{} must be refused",
                refused.display()
            );
        }

        let missing = policy()
            .with_read_path_under_home(&home.path().join("missing"), Some(home.path()))
            .unwrap();
        assert!(missing.toolchain_read_paths.is_empty());
    }

    #[test]
    fn a_read_path_is_readable_in_the_seatbelt_profile() {
        let dir = tempfile::tempdir().unwrap();
        let extra = dir.path().join("extra");
        let workspace = dir.path().join("ws");
        std::fs::create_dir_all(&extra).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        let profile = seatbelt_profile(
            &SandboxPolicy::for_workspace(&workspace)
                .with_read_path_under_home(&extra, None)
                .unwrap(),
        )
        .unwrap();
        let canonical = extra.canonicalize().unwrap();
        assert!(
            primary_file_read_rule(&profile)
                .contains(&format!("(subpath \"{}\")", canonical.display())),
            "{profile}"
        );
        assert!(
            !primary_file_write_rule(&profile).contains(&canonical.display().to_string()),
            "a read path must not become writable: {profile}"
        );
    }

    /// A real directory, because the profile canonicalises and a path that
    /// does not exist cannot be resolved.
    fn workspace() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    /// A read-only workspace must drop the workspace from the write allow —
    /// and, with nothing else writable, emit no `(allow file-write*)` at all,
    /// because an unfiltered allow would permit every write on the host.
    #[test]
    fn read_only_workspace_profile_grants_no_workspace_writes() {
        let ws = workspace();
        let root = ws.path().canonicalize().unwrap();
        let root = root.to_str().unwrap();
        let policy = SandboxPolicy::for_workspace(ws.path()).with_read_only_workspace();
        assert!(!policy.workspace_writable());
        let profile = seatbelt_profile(&policy).unwrap();
        assert!(
            !profile.contains("(allow file-write*"),
            "nothing is writable, so no write allow may be emitted: {profile}"
        );
        assert!(
            primary_file_read_rule(&profile).contains(&format!("(subpath \"{root}\")")),
            "the workspace must stay readable"
        );
        assert!(profile.contains("(deny network*)"));

        let tmp = tempfile::tempdir().unwrap();
        let tmp_root = tmp.path().canonicalize().unwrap();
        let profile = seatbelt_profile(&policy.clone().with_session_tmp(tmp.path())).unwrap();
        let write = primary_file_write_rule(&profile);
        assert!(write.contains(tmp_root.to_str().unwrap()), "{write}");
        assert!(
            !write.contains(&format!("(subpath \"{root}\")")),
            "the workspace must not be writable: {write}"
        );
    }

    #[test]
    fn writable_workspace_is_the_default() {
        let ws = workspace();
        let root = ws.path().canonicalize().unwrap();
        let profile = seatbelt_profile(&SandboxPolicy::for_workspace(ws.path())).unwrap();
        assert!(primary_file_write_rule(&profile)
            .contains(&format!("(subpath \"{}\")", root.to_str().unwrap())));
    }

    /// The argv wrapper passes the program and its arguments through as
    /// separate elements after the profile — never joined into a shell line —
    /// and only the egress proxy port gets a network hole.
    #[test]
    #[cfg(target_os = "macos")]
    fn wrap_command_passes_argv_through_under_the_profile() {
        if availability().is_err() {
            return; // cannot confine on this host; nothing to assert
        }
        let ws = workspace();
        let policy = SandboxPolicy::for_workspace(ws.path())
            .with_read_only_workspace()
            .with_egress_proxy(4242);
        let args = vec!["--flag".to_string(), "a b; rm -rf /".to_string()];
        let (program, argv) = wrap_command("node", &args, &policy).unwrap();
        assert_eq!(program, "/usr/bin/sandbox-exec");
        assert_eq!(argv[0], "-p");
        assert_eq!(&argv[2..], ["node", "--flag", "a b; rm -rf /"]);
        let profile = &argv[1];
        assert!(profile.contains("(deny network*)"));
        assert!(profile.contains("(allow network-outbound (remote ip \"localhost:4242\"))"));
        assert!(!profile.contains("(allow file-write*"), "{profile}");
    }

    #[test]
    fn chained_segments_all_contribute_to_command_access() {
        // Every `;`-separated segment runs, so every segment's executable
        // must be discovered. Missing the second segment would leave the
        // sandbox profile blind to half the command.
        let ws = workspace();
        let first = ws.path().join("first-tool");
        let second = ws.path().join("second-tool");
        std::fs::write(&first, b"#!/bin/sh\n").unwrap();
        std::fs::write(&second, b"#!/bin/sh\n").unwrap();
        let found = discover_command_executables(
            &format!("{}; {}", first.display(), second.display()),
            ws.path(),
        );
        assert!(found.iter().any(|p| p == &first), "first segment missing");
        assert!(found.iter().any(|p| p == &second), "second segment missing");
    }

    #[test]
    fn policy_marks_git_and_forge_readonly() {
        let ws = workspace();
        let p = SandboxPolicy::for_workspace(ws.path());
        let readonly = |p: &SandboxPolicy| {
            SandboxPolicy::readonly_subpaths_of(&p.workspace_root, p.git_writable)
        };
        assert_eq!(
            readonly(&p),
            vec![ws.path().join(".git"), ws.path().join(".forge")]
        );
        // Opened, the whole git dir is writable, hooks and config included.
        std::fs::create_dir_all(ws.path().join(".git")).unwrap();
        assert_eq!(
            readonly(&p.clone().with_git_open()),
            vec![ws.path().join(".forge")]
        );
    }

    /// A linked worktree — the shape a managed session lives in — keeps its
    /// object database in a gitdir outside the workspace root. Repository
    /// metadata is readable by default; only a Git spawn may write it.
    #[test]
    fn linked_worktree_metadata_is_readable_by_default_and_writable_for_git_spawns() {
        let repo = workspace();
        let gitdir = repo.path().join(".git/worktrees/session-1");
        std::fs::create_dir_all(&gitdir).unwrap();
        std::fs::write(gitdir.join("commondir"), "../..\n").unwrap();
        let ws = tempfile::tempdir().unwrap();
        std::fs::write(
            ws.path().join(".git"),
            format!("gitdir: {}\n", gitdir.display()),
        )
        .unwrap();
        std::fs::write(
            gitdir.join("gitdir"),
            format!("{}\n", ws.path().join(".git").display()),
        )
        .unwrap();

        let plain = SandboxPolicy::for_workspace(ws.path());
        assert!(
            plain
                .linked_git_dirs
                .contains(&gitdir.canonicalize().unwrap()),
            "repository discovery should read the linked worktree gitdir"
        );

        let policy = plain.clone().with_git_open();
        let gitdir = gitdir.canonicalize().unwrap();
        let common = repo.path().join(".git").canonicalize().unwrap();
        assert!(
            policy.linked_git_dirs.contains(&gitdir),
            "the worktree's own gitdir must be reached: {:?}",
            policy.linked_git_dirs
        );
        assert!(
            policy.linked_git_dirs.contains(&common),
            "the shared common dir must be reached: {:?}",
            policy.linked_git_dirs
        );
        let read_only = seatbelt_profile(&plain).unwrap();
        let read_rule = primary_file_read_rule(&read_only);
        let write_rule = primary_file_write_rule(&read_only);
        for dir in [&gitdir, &common] {
            let dir = dir.to_str().unwrap();
            assert!(read_rule.contains(&format!("(subpath \"{dir}\")")));
            assert!(!write_rule.contains(&format!("(subpath \"{dir}\")")));
        }
    }

    /// A normal repository keeps `.git` inside the workspace, so metadata
    /// outside the workspace is not added to its sandbox.
    #[test]
    fn a_normal_repository_adds_no_linked_git_dirs() {
        let ws = workspace();
        assert!(SandboxPolicy::for_workspace(ws.path())
            .with_git_open()
            .linked_git_dirs
            .is_empty());
    }

    #[test]
    fn seatbelt_profile_opens_linked_metadata_hooks_included() {
        let repo = workspace();
        let gitdir = repo.path().join(".git/worktrees/session-1");
        std::fs::create_dir_all(&gitdir).unwrap();
        std::fs::write(gitdir.join("commondir"), "../..\n").unwrap();
        let ws = tempfile::tempdir().unwrap();
        std::fs::write(
            ws.path().join(".git"),
            format!("gitdir: {}\n", gitdir.display()),
        )
        .unwrap();
        std::fs::write(
            gitdir.join("gitdir"),
            format!("{}\n", ws.path().join(".git").display()),
        )
        .unwrap();

        let policy = SandboxPolicy::for_workspace(ws.path()).with_git_open();
        let profile = seatbelt_profile(&policy).unwrap();
        let gitdir = gitdir.canonicalize().unwrap().to_str().unwrap().to_string();
        let common = repo
            .path()
            .join(".git")
            .canonicalize()
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        let read_rule = primary_file_read_rule(&profile);
        let write_rule = primary_file_write_rule(&profile);
        for dir in [&gitdir, &common] {
            assert!(
                read_rule.contains(&format!("(subpath \"{dir}\")")),
                "git metadata must be readable: {dir}"
            );
            assert!(
                write_rule.contains(&format!("(subpath \"{dir}\")")),
                "git metadata must be writable for git spawns: {dir}"
            );
            let hooks = format!("(deny file-write* (subpath \"{dir}/hooks\"))");
            assert!(
                !profile.contains(&hooks),
                "an opened git dir has no hooks carve-out: {dir}"
            );
        }

        let plain_policy = SandboxPolicy::for_workspace(ws.path());
        let plain = seatbelt_profile(&plain_policy).unwrap();
        assert!(
            primary_file_read_rule(&plain).contains(&format!("(subpath \"{gitdir}\")")),
            "repository discovery needs the linked worktree gitdir readable"
        );
        assert!(
            !primary_file_write_rule(&plain).contains(&format!("(subpath \"{gitdir}\")")),
            "repository discovery must not make the linked worktree gitdir writable"
        );
    }

    #[test]
    fn profile_denies_by_default_and_denies_network() {
        let ws = workspace();
        let profile = seatbelt_profile(&SandboxPolicy::for_workspace(ws.path())).unwrap();
        assert!(profile.starts_with("(version 1)\n(deny default)"));
        assert!(profile.contains("(deny network*)"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn profile_allows_only_the_xcrun_cache_namespace() {
        let ws = workspace();
        let scratch = workspace();
        let policy = SandboxPolicy::for_workspace(ws.path()).with_session_tmp(scratch.path());
        let profile = seatbelt_profile(&policy).unwrap();
        let pattern = xcrun_cache_regex().unwrap();
        let read_rule = format!("(allow file-read* file-test-existence (regex #\"{pattern}\"))");
        let write_rule = format!("(allow file-write* (regex #\"{pattern}\"))");

        assert!(profile.contains(&read_rule));
        assert!(profile.contains(&write_rule));
        let shared_temp = std::env::temp_dir().canonicalize().unwrap();
        assert!(!primary_file_write_rule(&profile)
            .contains(&format!("(subpath \"{}\")", shared_temp.display())));

        let plain = seatbelt_profile(&SandboxPolicy::for_workspace(ws.path())).unwrap();
        assert!(!plain.contains(&write_rule));
    }

    /// A home directory with the usual secret and personal locations, and a
    /// policy whose deny-list is built from it.
    fn home_fixture() -> (tempfile::TempDir, PathBuf) {
        let home = workspace();
        for dir in [
            ".ssh",
            ".aws",
            "Documents/proj",
            "Library/Caches",
            "Library/Keychains",
            ".cargo",
        ] {
            std::fs::create_dir_all(home.path().join(dir)).unwrap();
        }
        let canonical = home.path().canonicalize().unwrap();
        (home, canonical)
    }

    /// The whole disk is readable except the deny-list, so no toolchain has
    /// to be named. Credentials, the user's own files and application state
    /// are on the list; the parts of `~/Library` toolchains use are not.
    #[test]
    fn everything_is_readable_except_the_deny_list() {
        let ws = workspace();
        let (_home, home) = home_fixture();
        let policy = SandboxPolicy::for_workspace(ws.path()).with_home(&home);
        let profile = seatbelt_profile(&policy).unwrap();

        assert!(profile.contains("(allow file-read* file-test-existence file-map-executable)\n"));
        for denied in [
            ".ssh",
            ".aws",
            ".netrc",
            ".npmrc",
            ".zsh_history",
            "Documents",
            "Desktop",
        ] {
            assert!(
                profile.contains(&format!(
                    "(deny file-read* (subpath \"{}\"))",
                    home.join(denied).display()
                )),
                "{denied} must be unreadable: {profile}"
            );
        }
        let library = home.join("Library");
        assert!(
            profile.contains(&format!(
                "(deny file-read* (require-all (subpath \"{}\") (require-not (subpath \"{}\"))",
                library.display(),
                library.join("Caches").display()
            )),
            "~/Library is denied apart from the toolchain parts: {profile}"
        );
        assert!(
            !profile.contains(&format!("(subpath \"{}\")", home.join(".cargo").display())),
            "a toolchain directory is neither denied nor named"
        );
        assert!(profile.contains("(deny file-read* (subpath \"/Volumes\"))"));
    }

    /// A workspace inside a denied tree is readable; its denied parent is
    /// not, beyond the metadata needed to walk to it.
    #[test]
    fn a_workspace_inside_a_denied_tree_is_reopened() {
        let (_home, home) = home_fixture();
        let project = home.join("Documents/proj");
        let profile =
            seatbelt_profile(&SandboxPolicy::for_workspace(&project).with_home(&home)).unwrap();

        let deny = profile
            .find(&format!(
                "(deny file-read* (subpath \"{}\"))",
                home.join("Documents").display()
            ))
            .expect("Documents is denied");
        let reopen = profile
            .find(&format!("(subpath \"{}\")", project.display()))
            .expect("the workspace is reopened");
        assert!(reopen > deny, "the reopening rule must follow the deny");
        assert!(profile.contains(&format!(
            "(literal \"{}\")",
            home.join("Documents").display()
        )));
    }

    /// Opening the home directory as a project must not reopen what is on
    /// the deny-list inside it.
    #[test]
    fn a_workspace_containing_denied_locations_does_not_reopen_them() {
        let (_home, home) = home_fixture();
        let profile =
            seatbelt_profile(&SandboxPolicy::for_workspace(&home).with_home(&home)).unwrap();
        let reopen = profile
            .find("(allow file-read* file-test-existence file-map-executable\n")
            .unwrap();
        let ssh = format!(
            "(deny file-read* (subpath \"{}\"))",
            home.join(".ssh").display()
        );
        assert!(
            profile[reopen..].contains(&ssh),
            "~/.ssh must be denied again after the workspace allow: {profile}"
        );
    }

    /// What a command names decides nothing about what is readable. Naming
    /// a file as the "program" (`/path/secret; cat /path/secret`) used to
    /// add it to the read allow-list.
    #[test]
    fn a_file_named_by_the_command_is_not_added_to_the_profile() {
        let ws = workspace();
        let (_home, home) = home_fixture();
        let secret = home.join(".ssh/id_ed25519");
        std::fs::write(&secret, b"key").unwrap();

        let plain = SandboxPolicy::for_workspace(ws.path()).with_home(&home);
        let named = plain
            .clone()
            .with_command_access(&format!("{0}; cat {0}", secret.display()));
        assert_eq!(
            seatbelt_profile(&named).unwrap(),
            seatbelt_profile(&plain).unwrap()
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
    fn package_caches_go_to_the_tool_cache_and_rustup_home_stays_host_state() {
        let ws = workspace();
        let scratch = workspace();
        let cache = workspace();
        let rustup_home = workspace();
        let (_home, home) = home_fixture();
        std::fs::write(home.join(".cargo/config.toml"), "[net]\n").unwrap();
        std::fs::write(
            home.join(".npmrc"),
            "registry=https://npm.example.test/\n//npm.example.test/:_authToken=secret\n_auth=abc\n",
        )
        .unwrap();
        let mut policy = SandboxPolicy::for_workspace(ws.path())
            .with_home(&home)
            .with_session_tmp(scratch.path())
            .with_tool_cache(cache.path());
        policy.rustup_home = Some(rustup_home.path().to_path_buf());

        let env = policy.toolchain_env();
        let value = |name: &str| {
            env.iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| PathBuf::from(value))
        };
        assert_eq!(value("CARGO_HOME"), Some(cache.path().join("cargo")));
        assert_eq!(value("npm_config_cache"), Some(cache.path().join("npm")));
        assert_eq!(value("RUSTUP_HOME"), Some(rustup_home.path().to_path_buf()));
        assert!(cache.path().join("cargo").is_dir());
        assert_eq!(
            std::fs::read_to_string(cache.path().join("cargo/config.toml")).unwrap(),
            "[net]\n",
            "the user's cargo config is linked into the new CARGO_HOME"
        );

        let npmrc = std::fs::read_to_string(value("NPM_CONFIG_USERCONFIG").unwrap()).unwrap();
        assert_eq!(npmrc, "registry=https://npm.example.test/\n");

        // The cache is writable and readable; the user's own is not named.
        let profile = seatbelt_profile(&policy).unwrap();
        let cache = cache.path().canonicalize().unwrap();
        assert!(primary_file_write_rule(&profile)
            .contains(&format!("(subpath \"{}\")", cache.display())));

        // Without a tool cache the session scratch directory is used.
        let fallback = SandboxPolicy::for_workspace(ws.path())
            .with_home(&home)
            .with_session_tmp(scratch.path());
        assert!(fallback.toolchain_env().contains(&(
            "CARGO_HOME".to_string(),
            scratch
                .path()
                .join("tool-cache/cargo")
                .to_string_lossy()
                .into_owned()
        )));
    }

    #[test]
    fn profile_does_not_allow_apple_event_sending() {
        let ws = workspace();
        let profile = seatbelt_profile(&SandboxPolicy::for_workspace(ws.path())).unwrap();
        assert!(!profile.contains("(allow appleevent-send)"));
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

    /// A linked worktree's gitdir is bound writable (like the workspace) and
    /// its hooks are bound back read-only *after* that writable bind — the
    /// same ordering property the `.git` carve-out asserts.
    #[test]
    fn linked_worktree_metadata_is_bound_writable_and_hooks_stay_readonly() {
        let repo = workspace();
        let gitdir = repo.path().join(".git/worktrees/session-1");
        std::fs::create_dir_all(&gitdir).unwrap();
        std::fs::write(gitdir.join("commondir"), "../..\n").unwrap();
        let ws = tempfile::tempdir().unwrap();
        std::fs::write(
            ws.path().join(".git"),
            format!("gitdir: {}\n", gitdir.display()),
        )
        .unwrap();
        std::fs::write(
            gitdir.join("gitdir"),
            format!("{}\n", ws.path().join(".git").display()),
        )
        .unwrap();

        let policy = SandboxPolicy::for_workspace(ws.path()).with_git_open();
        let Some((_, args)) = bubblewrap_invocation("sh", "git status", &policy) else {
            return; // bwrap not installed on this host; nothing to assert
        };
        let gitdir = gitdir.canonicalize().unwrap();
        let gitdir_str = gitdir.to_str().unwrap();
        let common = repo.path().join(".git").canonicalize().unwrap();
        let common_str = common.to_str().unwrap();

        for dir in [gitdir_str, common_str] {
            assert!(
                args.windows(3)
                    .any(|w| w[0] == "--bind" && w[1] == dir && w[2] == dir),
                "{dir} must be bound writable"
            );
        }
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

    /// Paths named with `with_write_path` hold inside the read-only
    /// `.forge` carve-out, as they do in the Seatbelt profile, and are bound
    /// exactly. A path that does not exist yet is not widened to its parent.
    #[test]
    fn extra_write_paths_are_bound_after_the_readonly_carve_out() {
        let ws = workspace();
        let worktrees = ws.path().join(".forge/local/worktrees");
        std::fs::create_dir_all(&worktrees).unwrap();
        let missing = ws.path().join(".forge/local/not-yet");
        let policy = SandboxPolicy::for_workspace(ws.path())
            .with_write_path(&worktrees)
            .with_write_path(&missing);
        let Some((_, args)) = bubblewrap_invocation("sh", "true", &policy) else {
            return;
        };
        let worktrees = worktrees.canonicalize().unwrap();
        let worktrees = worktrees.to_str().unwrap();
        let forge = Path::new(worktrees).parent().unwrap().parent().unwrap();
        let carve = args
            .windows(2)
            .position(|w| w[0] == "--ro-bind-try" && Path::new(&w[1]) == forge)
            .expect(".forge is carved back to read-only");
        let bind = args
            .windows(3)
            .position(|w| w[0] == "--bind" && w[1] == worktrees && w[2] == worktrees)
            .expect("the named write path is bound writable");
        assert!(bind > carve, "the write path must come after the carve-out");
        assert!(
            !args.iter().any(|arg| arg.ends_with("not-yet")),
            "a missing write path is skipped, not bound: {args:?}"
        );
        assert!(
            !args
                .windows(2)
                .any(|w| w[0] == "--bind" && Path::new(&w[1]) == forge.join("local")),
            "a missing write path is not widened to its parent"
        );
    }

    #[test]
    fn an_executable_under_home_is_reexposed_after_command_access() {
        let ws = workspace();
        let home = tempfile::tempdir().unwrap();
        let exe = home.path().join("forge");
        std::fs::write(&exe, "").unwrap();
        let policy = SandboxPolicy::for_workspace(ws.path())
            .with_command_access("true")
            .with_executable(&exe);
        let Some((_, args)) = bubblewrap_invocation("sh", "true", &policy) else {
            return;
        };
        let exe = exe.to_str().unwrap();
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--ro-bind-try" && w[1] == exe),
            "the executable must be rebound read-only: {args:?}"
        );
    }

    /// A toolchain outside the masks is already visible, and is not bound
    /// again: `/usr/bin/go` is a symlink on GitHub's runners, and binding a
    /// symlink made bwrap refuse to start the command at all.
    #[test]
    fn toolchains_outside_the_masks_are_not_rebound() {
        let ws = workspace();
        let mut policy = SandboxPolicy::for_workspace(ws.path());
        policy.toolchain_executable_paths = vec![
            PathBuf::from("/usr/bin/go"),
            PathBuf::from("/home/forge-user/.local/bin/gh"),
        ];
        let Some((_, args)) = bubblewrap_invocation("sh", "go version", &policy) else {
            return;
        };
        assert!(
            !args.iter().any(|arg| arg == "/usr/bin/go"),
            "a visible toolchain must not be rebound: {args:?}"
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--ro-bind-try" && w[1] == "/home/forge-user/.local/bin/gh"),
            "a toolchain under a mask is still rebound"
        );
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
    /// counterpart lives in `tests/sandbox_enforcement.rs`): the per-user
    /// temp tree and mounted volumes are denied, and the session's own trees
    /// are reopened after the denies.
    #[test]
    fn shared_temp_and_volumes_are_denied_and_the_workspace_reopened() {
        let ws = workspace();
        let profile = seatbelt_profile(&SandboxPolicy::for_workspace(ws.path())).unwrap();
        let temp = std::env::temp_dir().canonicalize().unwrap();
        let deny = profile
            .find(&format!(
                "(deny file-read* (subpath \"{}\"))",
                temp.display()
            ))
            .expect("the shared temp tree is denied");
        assert!(profile.contains("(deny file-read* (subpath \"/Volumes\"))"));

        let canonical = ws.path().canonicalize().unwrap();
        let reopen = profile
            .rfind(&format!("(subpath \"{}\")", canonical.display()))
            .expect("the workspace must be readable");
        assert!(reopen > deny);
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

    /// A read-only workspace is bound with `--ro-bind`, never `--bind`.
    #[test]
    fn read_only_workspace_is_bound_read_only() {
        let ws = workspace();
        let root = ws.path().canonicalize().unwrap();
        let root = root.to_str().unwrap();
        let policy = SandboxPolicy::for_workspace(ws.path()).with_read_only_workspace();
        let Some((_, args)) = bubblewrap_invocation("sh", "true", &policy) else {
            return; // bwrap not installed on this host; nothing to assert
        };
        assert!(
            args.windows(3)
                .any(|w| w[0] == "--ro-bind" && w[1] == root && w[2] == root),
            "{args:?}"
        );
        assert!(
            !args.windows(2).any(|w| w[0] == "--bind" && w[1] == root),
            "the workspace must not be bound writable: {args:?}"
        );
    }

    /// An argv payload runs the program directly after `--`, with its
    /// arguments as separate elements, and dies with bwrap.
    #[test]
    fn argv_payload_runs_the_program_without_a_shell() {
        let ws = workspace();
        let args = vec!["serve".to_string(), "a b; rm -rf /".to_string()];
        let Some((_, argv)) = bubblewrap_invocation_for(
            Payload::Argv {
                program: "mcp-server",
                args: &args,
            },
            &SandboxPolicy::for_workspace(ws.path()),
        ) else {
            return; // bwrap not installed on this host; nothing to assert
        };
        let dashdash = argv.iter().position(|a| a == "--").unwrap();
        assert_eq!(
            &argv[dashdash + 1..],
            ["mcp-server", "serve", "a b; rm -rf /"]
        );
        assert!(argv[..dashdash].iter().any(|a| a == "--die-with-parent"));
        assert!(argv.iter().any(|a| a == "--unshare-net"));
    }

    /// With egress, the relay starts first and then `exec "$@"` hands over to
    /// the program, whose argv is passed positionally rather than re-parsed.
    #[test]
    fn argv_payload_with_egress_execs_the_program_after_the_relay() {
        let ws = workspace();
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("egress.sock");
        std::fs::write(&sock, b"").unwrap();
        let args = vec!["--x".to_string()];
        let policy = SandboxPolicy::for_workspace(ws.path()).with_egress_socket(&sock);
        let Some((_, argv)) = bubblewrap_invocation_for(
            Payload::Argv {
                program: "mcp-server",
                args: &args,
            },
            &policy,
        ) else {
            return; // bwrap or socat not installed on this host
        };
        let dashdash = argv.iter().position(|a| a == "--").unwrap();
        let tail = &argv[dashdash + 1..];
        assert_eq!(tail[0], "/bin/sh");
        assert_eq!(tail[1], "-c");
        assert!(tail[2].ends_with("exec \"$@\""), "{}", tail[2]);
        assert_eq!(&tail[3..], ["sh", "mcp-server", "--x"]);
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

#[cfg(test)]
mod grant_tests {
    use super::grantable_write_dir;
    use std::path::Path;

    #[test]
    fn only_ordinary_directories_can_be_granted_for_writing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(grantable_write_dir(dir.path()).is_ok());
        assert!(grantable_write_dir(&dir.path().join("missing")).is_err());
        let file = dir.path().join("file");
        std::fs::write(&file, "x").unwrap();
        assert!(grantable_write_dir(&file).is_err());

        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap();
        for refused in [
            Path::new("/"),
            Path::new("/usr/local"),
            Path::new("/etc"),
            home.as_path(),
            &home.join(".ssh"),
            &home.join("Library"),
            &home.join("Library/LaunchAgents"),
        ] {
            if refused.exists() {
                assert!(
                    grantable_write_dir(refused).is_err(),
                    "{} must be refused",
                    refused.display()
                );
            }
        }
        if let Some(path) = std::env::var_os("PATH") {
            for entry in std::env::split_paths(&path).filter(|entry| entry.is_dir()) {
                assert!(grantable_write_dir(&entry).is_err(), "{}", entry.display());
            }
        }
    }
}
