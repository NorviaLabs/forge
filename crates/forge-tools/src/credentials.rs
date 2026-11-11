//! Host-side HTTPS identity for confined spawns, keyed off the egress grant.
//!
//! Two independent facts about confinement, neither of them tool-specific:
//!
//! 1. The CONNECT proxy can reach a host once `host(...)` allows it.
//! 2. The sandbox cannot open the OS secret store, so HTTPS clients look
//!    unauthenticated even after a grant.
//!
//! A host grant therefore **projects** HTTPS identity for that host into the
//! spawn: credentials filled on the host via `git credential`, a credential
//! helper that answers for those hosts, and `{first-label}_TOKEN` so HTTPS
//! CLIs that do not speak git-credential still authenticate without falling
//! through to the host secret store. The user's own git configuration stays
//! in effect; only the helper list is replaced, through the environment.
//! Callers isolate XDG dirs for every confined spawn, not just commands using
//! a credential. The same grant is what authorized talking to the host in
//! the first place.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use crate::sandbox::EgressGrant;

/// Keyed by workspace as well as host: `git credential fill` runs in the
/// workspace so repository-local helpers and `includeIf` identities apply,
/// and one project's identity must never be reused for another's.
type CredentialKey = (PathBuf, String);
static CREDENTIAL_CACHE: Mutex<Option<HashMap<CredentialKey, Option<HostCredential>>>> =
    Mutex::new(None);

/// Well-known credential variables used by external host clients. They are
/// stripped separately from Forge provider credentials so the public provider
/// list remains a contract with `forge-connect`; a confined command only gets
/// a host credential back through `host_identity_env` after a matching grant.
pub(crate) const HOST_CREDENTIAL_ENV: &[&str] = &[
    "GH_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
    "GITHUB_PERSONAL_ACCESS_TOKEN",
    "CARGO_REGISTRY_TOKEN",
    "CARGO_REGISTRIES_CRATES_IO_TOKEN",
];

#[derive(Debug, Clone, PartialEq, Eq)]
struct HostCredential {
    host: String,
    username: String,
    password: String,
}

/// Environment and spawn-local config for a confined command that may talk
/// to hosts the grant already permits. Credentials are resolved as git would
/// resolve them inside `workspace_root`.
pub fn host_identity_env(
    grant: Option<&EgressGrant>,
    config_dir: &Path,
    workspace_root: &Path,
) -> Vec<(String, String)> {
    let credentials = grant
        .map(|grant| credentials_for_grant(grant, workspace_root))
        .unwrap_or_default();
    let (credentials, mut env) = behind_the_gateway(grant, credentials, config_dir);
    env.extend(identity_env(&credentials, config_dir));
    env
}

/// Point the GitHub CLI at the gateway's Unix socket.
///
/// `gh` is a Go program, and on macOS Go verifies certificates with the
/// system's trust store only: no variable makes it accept the session
/// authority. It does have a setting to send its requests, in plain HTTP, to
/// a Unix socket; the gateway does the TLS from there.
fn gh_socket_env(grant: Option<&EgressGrant>, config_dir: &Path) -> Vec<(String, String)> {
    let socket = grant
        .map(|grant| grant.socket_path.as_path())
        .filter(|path| path.exists())
        .and_then(Path::to_str);
    let dir = config_dir.join("gh");
    let Some(socket) = socket.filter(|path| !path.contains(['\n', '"', '\\'])) else {
        return Vec::new();
    };
    let written = std::fs::create_dir_all(&dir).is_ok()
        && std::fs::write(
            dir.join("config.yml"),
            format!("http_unix_socket: \"{socket}\"\n"),
        )
        .is_ok();
    match dir.to_str().filter(|_| written) {
        Some(dir) => vec![("GH_CONFIG_DIR".into(), dir.to_string())],
        None => Vec::new(),
    }
}

/// The system's root certificates, which the bundle given to a confined
/// process must still contain: the variables below replace a client's trust
/// store, they do not add to it.
const SYSTEM_ROOTS: &str = "/etc/ssl/cert.pem";

/// Keep the credentials out of the sandbox: the gateway supplies each one
/// itself, and the confined process gets a placeholder plus the session
/// authority's certificate so it accepts the gateway's TLS.
///
/// Returns the credentials to project (placeholders in place of passwords)
/// and the environment that makes clients trust the authority. When the
/// gateway cannot do this (no live endpoint, no authority, no readable
/// system roots) the credentials come back unchanged, as before.
fn behind_the_gateway(
    grant: Option<&EgressGrant>,
    credentials: Vec<HostCredential>,
    config_dir: &Path,
) -> (Vec<HostCredential>, Vec<(String, String)>) {
    let Some(endpoint) = grant.and_then(|grant| grant.control.as_ref()) else {
        return (credentials, Vec::new());
    };
    if credentials.is_empty() {
        return (credentials, Vec::new());
    }
    let bundle = config_dir.join("forge-ca-bundle.pem");
    let written = endpoint
        .session_ca_pem()
        .zip(std::fs::read_to_string(SYSTEM_ROOTS).ok())
        .is_some_and(|(authority, roots)| {
            std::fs::write(&bundle, format!("{roots}\n{authority}")).is_ok()
        });
    let Some(bundle) = bundle.to_str().filter(|_| written) else {
        return (credentials, Vec::new());
    };
    let shielded = credentials
        .iter()
        .map(|cred| {
            Some(HostCredential {
                password: endpoint.supply_credential(&cred.host, &cred.password)?,
                ..cred.clone()
            })
        })
        .collect::<Option<Vec<_>>>();
    let Some(shielded) = shielded else {
        return (credentials, Vec::new());
    };
    let mut env: Vec<(String, String)> = [
        "GIT_SSL_CAINFO",
        "SSL_CERT_FILE",
        "CURL_CA_BUNDLE",
        "REQUESTS_CA_BUNDLE",
        "NODE_EXTRA_CA_CERTS",
    ]
    .into_iter()
    .map(|name| (name.to_string(), bundle.to_string()))
    .collect();
    env.extend(gh_socket_env(grant, config_dir));
    (shielded, env)
}

fn identity_env(credentials: &[HostCredential], config_dir: &Path) -> Vec<(String, String)> {
    let mut env = helper_env(credentials, config_dir);
    for cred in credentials {
        if let Some(name) = token_env_name(&cred.host) {
            env.push((name, cred.password.clone()));
        }
    }
    env
}

/// Environment that keeps XDG-aware command-line clients out of the host's
/// mutable config, cache, and state trees. The caller applies this to every
/// confined spawn, rather than maintaining a list of supported CLIs.
///
/// Git is the exception: the user's own git settings (name, email, aliases,
/// signing) are read-only input a confined git should see. `~/.gitconfig` is
/// found as usual; `$XDG_CONFIG_HOME/git`, which the redirect would hide, is
/// linked into the isolated tree.
pub fn isolated_config_env(config_dir: &Path) -> Vec<(String, String)> {
    let mut env = Vec::new();
    for (var, dir) in [
        ("XDG_CONFIG_HOME", config_dir.join("xdg-config")),
        ("XDG_CACHE_HOME", config_dir.join("xdg-cache")),
        ("XDG_STATE_HOME", config_dir.join("xdg-state")),
    ] {
        let _ = std::fs::create_dir_all(&dir);
        env.push((var.into(), dir.to_string_lossy().into_owned()));
    }
    let user_git = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .map(|dir| dir.join("git"));
    let link = config_dir.join("xdg-config").join("git");
    if let Some(user_git) = user_git.filter(|dir| dir.is_dir()) {
        if link.symlink_metadata().is_err() {
            #[cfg(unix)]
            let _ = std::os::unix::fs::symlink(user_git, link);
        }
    }
    env
}

/// `.git` is read-only by default so a confined spawn cannot rewrite
/// history. A spawn opts in when any segment of the command line runs git
/// itself.
///
/// Every segment is checked, not just the leading one: `make release && git
/// push` writes refs from the second segment, and keying off the leading
/// executable denied it.
///
/// Frontends that shell out to git (`gh`, `glab`, ...) are deliberately
/// **not** enumerated. Tracking providers here would be an architecture
/// smell, and it buys nothing: such a tool needs `.git` writes only to push
/// a branch on the caller's behalf, which it cannot do without a TTY to
/// prompt on anyway. The working shape is an explicit `git push` — which
/// this rule already covers, including when the two are chained in one
/// command line.
pub fn needs_git_writes(command: &str) -> bool {
    command_segments(command).any(|segment| {
        let exe = leading_executable(segment);
        exe == "git" || exe.starts_with("git-")
    })
}

/// Split a command line on the shell operators that start a new command.
///
/// This is a policy *widening* heuristic, not a security boundary: the
/// sandbox is what enforces the result, and over-splitting can only ever
/// propose git writability for a spawn, never grant it filesystem reach.
/// Quoting is not honoured, so `echo "git push"` also proposes the
/// carve-out — accepted, because the alternative is denying real pushes.
fn command_segments(command: &str) -> impl Iterator<Item = &str> {
    command
        .split([';', '\n', '|', '&', '(', ')'])
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
}

fn credentials_for_grant(grant: &EgressGrant, workspace_root: &Path) -> Vec<HostCredential> {
    let mut out = Vec::new();
    for pattern in grant.allow_patterns() {
        let Some(host) = apex_host(&pattern) else {
            continue;
        };
        if out.iter().any(|c: &HostCredential| c.host == host) {
            continue;
        }
        if let Some(cred) = fill_https_credential(&host, workspace_root) {
            out.push(cred);
        }
    }
    out
}

/// Apex hostname a `host(...)` allow pattern stands for.
///
/// `**.example.com` and `*.example.com` and `example.com` all project
/// identity for `example.com`. `*` (unrestricted network) does not dump
/// every secret the host process can see.
fn apex_host(pattern: &str) -> Option<String> {
    let pattern = pattern.trim().trim_end_matches('.').to_ascii_lowercase();
    if pattern.is_empty() || pattern == "*" {
        return None;
    }
    let host = pattern
        .strip_prefix("**.")
        .or_else(|| pattern.strip_prefix("*."))
        .unwrap_or(&pattern);
    if host.is_empty() || host.starts_with('.') || host.contains("..") {
        return None;
    }
    if host.parse::<std::net::IpAddr>().is_ok() || !host.contains('.') {
        return None;
    }
    Some(host.to_string())
}

fn fill_https_credential(host: &str, workspace_root: &Path) -> Option<HostCredential> {
    // Canonicalize so two spellings of one workspace share an entry; fall back
    // to the raw path so a lookup never borrows another workspace's entry.
    let workspace = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    let key = (workspace, host.to_string());
    {
        let mut cache = CREDENTIAL_CACHE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let map = cache.get_or_insert_with(HashMap::new);
        if let Some(existing) = map.get(&key) {
            return existing.clone();
        }
    }
    let filled = fill_https_credential_uncached(host, &key.0);
    let mut cache = CREDENTIAL_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let map = cache.get_or_insert_with(HashMap::new);
    map.insert(key, filled.clone());
    filled
}

/// Ask the user's credential helper for `host`.
///
/// Run outside any repository, so only the user's own (global and system)
/// configuration applies. `credential.helper` names a program, and this
/// call is not confined: read from the workspace's config, which a confined
/// process can write, it would run whatever that config named.
fn fill_https_credential_uncached(host: &str, _workspace_root: &Path) -> Option<HostCredential> {
    let mut command = Command::new("git");
    crate::builtins::strip_credentials(&mut command, &[]);
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_CONFIG",
        "GIT_CONFIG_GLOBAL",
    ] {
        command.env_remove(name);
    }
    let mut child = command
        .current_dir(std::env::temp_dir())
        .args(["credential", "fill"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    {
        let mut stdin = child.stdin.take()?;
        write!(stdin, "protocol=https\nhost={host}\n\n").ok()?;
    }
    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        return None;
    }
    parse_credential_fill(host, &String::from_utf8(output.stdout).ok()?)
}

fn parse_credential_fill(host: &str, body: &str) -> Option<HostCredential> {
    let mut username = None;
    let mut password = None;
    for line in body.lines() {
        if let Some(value) = line.strip_prefix("username=") {
            username = Some(value.to_string());
        } else if let Some(value) = line.strip_prefix("password=") {
            password = Some(value.to_string());
        }
    }
    let password = password.filter(|p| !p.is_empty())?;
    Some(HostCredential {
        host: host.to_string(),
        username: username
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| "git".into()),
        password,
    })
}

/// The credential helper for a confined git that keeps the user's own
/// global configuration (name, email, aliases): the helper list is replaced
/// through the environment, which outranks every config file.
///
/// The user's own helpers are dropped either way. They cannot work in the
/// sandbox (the keychain is out of reach), and one that fails slowly or
/// loudly would only get in the way. Credentials are offered for hosts the
/// grant already permits, looked up on the host.
pub(crate) fn credential_helper_env(
    grant: Option<&EgressGrant>,
    config_dir: &Path,
    workspace_root: &Path,
) -> Vec<(String, String)> {
    let credentials = grant
        .map(|grant| credentials_for_grant(grant, workspace_root))
        .unwrap_or_default();
    let (credentials, mut env) = behind_the_gateway(grant, credentials, config_dir);
    env.extend(helper_env(&credentials, config_dir));
    env
}

fn helper_env(credentials: &[HostCredential], config_dir: &Path) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = vec![
        ("GIT_TERMINAL_PROMPT".into(), "0".into()),
        ("GIT_CONFIG_KEY_0".into(), "credential.helper".into()),
        ("GIT_CONFIG_VALUE_0".into(), String::new()),
    ];
    let mut count = 1;
    let store = config_dir.join("https-credentials");
    let helper = config_dir.join("git-credential-forge");
    if !credentials.is_empty()
        && write_credential_store(&store, credentials).is_ok()
        && write_credential_helper(&helper).is_ok()
    {
        env.push((
            "FORGE_HOST_CREDENTIALS".into(),
            store.to_string_lossy().into_owned(),
        ));
        env.push(("GIT_CONFIG_KEY_1".into(), "credential.helper".into()));
        env.push((
            "GIT_CONFIG_VALUE_1".into(),
            format!("!sh '{}'", helper.to_string_lossy()),
        ));
        count = 2;
    }
    env.push(("GIT_CONFIG_COUNT".into(), count.to_string()));
    env
}

/// `{first-label}_TOKEN` for `host`. `example.com` → `EXAMPLE_TOKEN`.
///
/// HTTPS CLIs that do not speak git-credential commonly document this
/// shape. It is derived from the granted host, not a product table.
fn token_env_name(host: &str) -> Option<String> {
    let label = host.split('.').next().filter(|label| !label.is_empty())?;
    let mut chars = label.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    let mut name = String::with_capacity(label.len() + 6);
    for c in std::iter::once(first).chain(chars) {
        if c.is_ascii_alphanumeric() {
            name.push(c.to_ascii_uppercase());
        } else {
            name.push('_');
        }
    }
    name.push_str("_TOKEN");
    Some(name)
}

fn write_credential_store(path: &Path, credentials: &[HostCredential]) -> std::io::Result<()> {
    let mut body = String::new();
    for cred in credentials {
        body.push_str(&format!(
            "{}\t{}\t{}\n",
            cred.host, cred.username, cred.password
        ));
    }
    std::fs::write(path, body)
}

fn write_credential_helper(path: &Path) -> std::io::Result<()> {
    const SCRIPT: &str = r#"#!/bin/sh
[ "$1" = get ] || exit 0
host=
while IFS= read -r line || [ -n "$line" ]; do
  [ -z "$line" ] && break
  case "$line" in
    host=*) host=${line#host=} ;;
  esac
done
host=${host%%:*}
file=$FORGE_HOST_CREDENTIALS
[ -n "$host" ] && [ -f "$file" ] || exit 0
while IFS=$(printf '\t') read -r h user pass; do
  case "$host" in
    "$h"|*."$h")
      printf 'username=%s\npassword=%s\n' "$user" "$pass"
      exit 0
      ;;
  esac
done < "$file"
"#;
    std::fs::write(path, SCRIPT)
}

fn leading_executable(command: &str) -> &str {
    let token = command
        .split_whitespace()
        .find(|token| !token.contains('='))
        .unwrap_or("");
    std::path::Path::new(token)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apex_host_strips_wildcard_prefixes() {
        assert_eq!(apex_host("**.example.com").as_deref(), Some("example.com"));
        assert_eq!(apex_host("*.example.com").as_deref(), Some("example.com"));
        assert_eq!(apex_host("example.com").as_deref(), Some("example.com"));
        assert_eq!(apex_host("*"), None);
        assert_eq!(apex_host("127.0.0.1"), None);
        assert_eq!(apex_host("localhost"), None);
    }

    #[test]
    fn only_git_itself_needs_git_dir_writes() {
        assert!(needs_git_writes("git push origin HEAD"));
        assert!(needs_git_writes("git status"));
        assert!(needs_git_writes("git-lfs push origin main"));
        assert!(!needs_git_writes("gh pr create"));
        assert!(!needs_git_writes("glab mr create"));
        assert!(!needs_git_writes("cargo test"));
        assert!(!needs_git_writes("curl -I https://example.com"));
    }

    #[test]
    fn isolated_config_is_not_tied_to_a_product_cli() {
        let dir = tempfile::tempdir().unwrap();
        let env = isolated_config_env(dir.path());
        for name in ["XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_STATE_HOME"] {
            let value = env
                .iter()
                .find(|(candidate, _)| candidate == name)
                .map(|(_, value)| value)
                .expect("all XDG trees must be isolated");
            assert!(value.starts_with(dir.path().to_str().unwrap()));
        }
        assert!(dir.path().join("xdg-config").is_dir());
    }

    /// The user's own git settings stay in effect in a confined spawn: the
    /// global config is not replaced.
    #[test]
    fn isolated_config_leaves_the_users_git_config_in_effect() {
        let dir = tempfile::tempdir().unwrap();
        let env = isolated_config_env(dir.path());
        assert!(!env
            .iter()
            .any(|(name, _)| name == "GIT_CONFIG_GLOBAL" || name == "GIT_CONFIG_NOSYSTEM"));
    }

    #[test]
    fn github_credentials_use_the_generic_host_projection() {
        assert_eq!(token_env_name("github.com"), Some("GITHUB_TOKEN".into()));
        assert_eq!(token_env_name("crates.io"), Some("CRATES_TOKEN".into()));
        let dir = tempfile::tempdir().unwrap();
        let env = helper_env(
            &[HostCredential {
                host: "github.com".into(),
                username: "oauth2".into(),
                password: "token".into(),
            }],
            dir.path(),
        );
        let value = |name: &str| {
            env.iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        };
        // The user's helpers are dropped, then Forge's answers.
        assert_eq!(value("GIT_CONFIG_COUNT"), Some("2"));
        assert_eq!(value("GIT_CONFIG_VALUE_0"), Some(""));
        assert!(value("GIT_CONFIG_VALUE_1").unwrap().starts_with("!sh '"));
        assert!(value("FORGE_HOST_CREDENTIALS").is_some());
        assert!(!env.iter().any(|(name, _)| name == "GIT_CONFIG_GLOBAL"));
    }

    #[test]
    fn host_client_credentials_are_separate_from_provider_credentials() {
        assert!(HOST_CREDENTIAL_ENV.contains(&"GH_TOKEN"));
        assert!(HOST_CREDENTIAL_ENV.contains(&"CARGO_REGISTRY_TOKEN"));
        assert!(!HOST_CREDENTIAL_ENV.contains(&"OPENAI_API_KEY"));
    }

    /// A git write can sit in any segment of a compound command. Checking
    /// only the leading executable denied all of these.
    #[test]
    fn git_writes_are_detected_in_any_command_segment() {
        assert!(needs_git_writes("make release && git push"));
        assert!(needs_git_writes("cargo build; git tag -a v1 -m v1"));
        assert!(needs_git_writes(
            "gh pr create --head x && git update-ref HEAD x"
        ));
        assert!(needs_git_writes("ls | grep -q x || git commit -am wip"));
        assert!(!needs_git_writes("cargo build && cargo test"));
    }

    #[test]
    fn parse_fill_reads_username_and_password() {
        let cred = parse_credential_fill(
            "example.com",
            "protocol=https\nhost=example.com\nusername=me\npassword=s3cret\n",
        )
        .unwrap();
        assert_eq!(cred.host, "example.com");
        assert_eq!(cred.username, "me");
        assert_eq!(cred.password, "s3cret");
    }

    #[test]
    fn identity_is_not_projected_without_a_grant() {
        let dir = tempfile::tempdir().unwrap();
        let env = host_identity_env(None, dir.path(), dir.path());
        assert!(!env
            .iter()
            .any(|(name, _)| name.ends_with("_TOKEN") || name == "FORGE_HOST_CREDENTIALS"));
    }

    #[test]
    fn token_env_name_is_the_first_dns_label() {
        assert_eq!(
            token_env_name("example.com").as_deref(),
            Some("EXAMPLE_TOKEN")
        );
        assert_eq!(
            token_env_name("my-git.internal").as_deref(),
            Some("MY_GIT_TOKEN")
        );
        assert_eq!(token_env_name("10.0.0.1"), None);
    }

    #[test]
    fn projected_env_uses_host_label_tokens_not_a_product_table() {
        let dir = tempfile::tempdir().unwrap();
        let env = identity_env(
            &[HostCredential {
                host: "example.com".into(),
                username: "me".into(),
                password: "s3cret".into(),
            }],
            dir.path(),
        );
        assert!(env
            .iter()
            .any(|(k, v)| k == "EXAMPLE_TOKEN" && v == "s3cret"));
        assert_eq!(env.iter().filter(|(k, _)| k.ends_with("_TOKEN")).count(), 1);
    }

    #[test]
    fn parse_fill_defaults_username_to_git() {
        let cred = parse_credential_fill("example.com", "password=s3cret\n").unwrap();
        assert_eq!(cred.username, "git");
        assert_eq!(cred.password, "s3cret");
    }

    /// A repository's own `credential.helper` names a program, and the
    /// lookup runs on the host. A confined process can write that config, so
    /// the lookup must neither run it nor believe its answer. The host is
    /// under the reserved `.invalid` TLD so no real store answers for it.
    #[test]
    fn a_repository_credential_helper_is_neither_run_nor_believed() {
        let host = "forge-credential-test.invalid";
        let repo = repo_with_helper("planted-secret");
        let marker = repo.path().join("helper-ran");
        let helper = format!(
            "!f() {{ : > {}; echo username=tester; echo password=planted-secret; }}; f",
            marker.display()
        );
        let status = Command::new("git")
            .args(["config", "--local", "credential.helper", &helper])
            .current_dir(repo.path())
            .status()
            .unwrap();
        assert!(status.success());

        let filled = fill_https_credential(host, repo.path());
        assert!(
            filled.is_none_or(|credential| credential.password != "planted-secret"),
            "the repository's helper must not supply the credential"
        );
        assert!(!marker.exists(), "the repository's helper must not run");
    }

    /// The helper list a confined git gets replaces the user's own, and
    /// names Forge's helper only when there is a credential to offer.
    #[test]
    fn the_confined_helper_env_resets_the_users_helpers() {
        let dir = tempfile::tempdir().unwrap();
        let env = credential_helper_env(None, dir.path(), dir.path());
        let value = |name: &str| {
            env.iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(value("GIT_CONFIG_COUNT"), Some("1"));
        assert_eq!(value("GIT_CONFIG_KEY_0"), Some("credential.helper"));
        assert_eq!(value("GIT_CONFIG_VALUE_0"), Some(""));
        assert_eq!(value("GIT_TERMINAL_PROMPT"), Some("0"));
        assert!(value("FORGE_HOST_CREDENTIALS").is_none());
    }

    fn repo_with_helper(password: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let status = Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir.path())
            .status()
            .unwrap();
        assert!(status.success());
        let helper = format!("!f() {{ echo username=tester; echo password={password}; }}; f");
        let status = Command::new("git")
            .args(["config", "--local", "credential.helper", &helper])
            .current_dir(dir.path())
            .status()
            .unwrap();
        assert!(status.success());
        dir
    }
}
