//! Host-side HTTPS identity for confined spawns, keyed off the egress grant.
//!
//! Three independent facts about confinement, none of them tool-specific:
//!
//! 1. The CONNECT proxy can reach a host once `host(...)` allows it.
//! 2. The sandbox cannot open the OS secret store, so HTTPS clients look
//!    unauthenticated even after a grant.
//! 3. SSH cannot use an HTTP CONNECT proxy, so `git@host:` remotes have no
//!    route out.
//!
//! A host grant therefore **projects** HTTPS identity for that host into the
//! spawn: credentials filled on the host via `git credential`, a spawn-local
//! gitconfig that rewrites SSH remotes to HTTPS, and a credential helper
//! that answers for those hosts. The same grant is what authorized talking
//! to the host in the first place.

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Mutex;

use crate::sandbox::EgressGrant;

static CREDENTIAL_CACHE: Mutex<Option<HashMap<String, Option<HostCredential>>>> = Mutex::new(None);

#[derive(Debug, Clone, PartialEq, Eq)]
struct HostCredential {
    host: String,
    username: String,
    password: String,
}

/// Environment and spawn-local config for a confined command that may talk
/// to hosts the grant already permits.
pub fn host_identity_env(grant: Option<&EgressGrant>, config_dir: &Path) -> Vec<(String, String)> {
    let Some(grant) = grant else {
        return Vec::new();
    };
    let credentials = credentials_for_grant(grant);
    if credentials.is_empty() {
        return Vec::new();
    }
    let mut env = vec![("GIT_TERMINAL_PROMPT".into(), "0".into())];
    let store = config_dir.join("https-credentials");
    if write_credential_store(&store, &credentials).is_ok() {
        env.push((
            "FORGE_HOST_CREDENTIALS".into(),
            store.to_string_lossy().into_owned(),
        ));
    }
    let helper = config_dir.join("git-credential-forge");
    if write_credential_helper(&helper).is_ok() {
        if let Some(gitconfig) = write_gitconfig(config_dir, &helper, &credentials) {
            env.push(("GIT_CONFIG_GLOBAL".into(), gitconfig));
            env.push(("GIT_CONFIG_NOSYSTEM".into(), "1".into()));
        }
    }
    for cred in &credentials {
        env.extend(https_cli_env(&cred.host, &cred.password));
    }
    if env.iter().any(|(name, _)| name == "GH_TOKEN") {
        env.push((
            "GH_CONFIG_DIR".into(),
            config_dir.to_string_lossy().into_owned(),
        ));
    }
    env
}

/// Git (and git-frontend CLIs) update refs under `.git`, which the default
/// policy carves out as read-only. This is about the git directory, not a
/// particular forge.
pub fn needs_git_writes(command: &str) -> bool {
    matches!(
        leading_executable(command),
        "git" | "gh" | "glab" | "tea" | "hut"
    )
}

fn credentials_for_grant(grant: &EgressGrant) -> Vec<HostCredential> {
    let mut out = Vec::new();
    for pattern in grant.allow_patterns() {
        let Some(host) = apex_host(&pattern) else {
            continue;
        };
        if out.iter().any(|c: &HostCredential| c.host == host) {
            continue;
        }
        if let Some(cred) = fill_https_credential(&host) {
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

fn fill_https_credential(host: &str) -> Option<HostCredential> {
    {
        let mut cache = CREDENTIAL_CACHE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let map = cache.get_or_insert_with(HashMap::new);
        if let Some(existing) = map.get(host) {
            return existing.clone();
        }
    }
    let filled = fill_https_credential_uncached(host);
    let mut cache = CREDENTIAL_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let map = cache.get_or_insert_with(HashMap::new);
    map.insert(host.to_string(), filled.clone());
    filled
}

fn fill_https_credential_uncached(host: &str) -> Option<HostCredential> {
    let mut child = Command::new("git")
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
            .unwrap_or_else(|| "x-access-token".into()),
        password,
    })
}

/// HTTPS CLIs that do not speak git-credential still need the password in
/// an env var they already document. Unknown hosts get gitconfig only.
/// This is an adapter, not spawn policy: identity is filled per granted host.
fn https_cli_env(host: &str, password: &str) -> Vec<(String, String)> {
    if host == "github.com" || host.ends_with(".github.com") {
        vec![
            ("GH_TOKEN".into(), password.to_string()),
            ("GITHUB_TOKEN".into(), password.to_string()),
        ]
    } else if host == "gitlab.com" || host.ends_with(".gitlab.com") {
        vec![("GITLAB_TOKEN".into(), password.to_string())]
    } else {
        Vec::new()
    }
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

fn write_gitconfig(
    config_dir: &Path,
    helper: &Path,
    credentials: &[HostCredential],
) -> Option<String> {
    let path = config_dir.join("gitconfig");
    let helper = helper.to_str()?;
    let mut body = String::from(
        "# Written by Forge for a confined spawn with a host(...) grant.\n\
         # SSH cannot use the CONNECT proxy; rewrite to HTTPS.\n\
         [credential]\n\
         \thelper =\n\
         [credential]\n",
    );
    body.push_str(&format!("\thelper = !sh '{helper}'\n"));
    for cred in credentials {
        let host = &cred.host;
        body.push_str(&format!(
            "[url \"https://{host}/\"]\n\
             \tinsteadOf = git@{host}:\n\
             \tinsteadOf = ssh://git@{host}/\n"
        ));
    }
    std::fs::write(&path, body).ok()?;
    Some(path.to_string_lossy().into_owned())
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
    fn git_frontends_need_git_dir_writes() {
        assert!(needs_git_writes("git push origin HEAD"));
        assert!(needs_git_writes("git status"));
        assert!(needs_git_writes("gh pr create"));
        assert!(needs_git_writes("glab mr create"));
        assert!(!needs_git_writes("cargo test"));
        assert!(!needs_git_writes("echo git push"));
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
    fn gitconfig_rewrites_ssh_for_each_projected_host() {
        let dir = tempfile::tempdir().unwrap();
        let helper = dir.path().join("helper");
        std::fs::write(&helper, "true").unwrap();
        let creds = vec![HostCredential {
            host: "example.com".into(),
            username: "me".into(),
            password: "x".into(),
        }];
        let gitconfig = write_gitconfig(dir.path(), &helper, &creds).unwrap();
        let body = std::fs::read_to_string(gitconfig).unwrap();
        assert!(body.contains("insteadOf = git@example.com:"));
        assert!(body.contains("insteadOf = ssh://git@example.com/"));
        assert!(body.contains("https://example.com/"));
        assert!(!body.contains("github.com"));
    }

    #[test]
    fn identity_is_not_projected_without_a_grant() {
        assert!(host_identity_env(None, Path::new("/tmp")).is_empty());
    }

    #[test]
    fn extra_cli_env_is_only_for_documented_https_clis() {
        assert!(https_cli_env("example.com", "x").is_empty());
        let github = https_cli_env("github.com", "x");
        assert!(github.iter().any(|(k, v)| k == "GH_TOKEN" && v == "x"));
        let gitlab = https_cli_env("gitlab.com", "x");
        assert!(gitlab.iter().any(|(k, v)| k == "GITLAB_TOKEN" && v == "x"));
    }
}
