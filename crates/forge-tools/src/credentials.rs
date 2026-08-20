//! Host-side identity for confined GitHub CLI / git-over-https spawns.
//!
//! The sandbox can reach `api.github.com` through the egress proxy once
//! `host(**.github.com)` is granted, but it cannot open the OS keychain
//! where `gh` stores the token. The command then prints HTTP 401 / "the
//! token in default is invalid", which looks like broken auth rather than
//! a boundary. Projecting the token from the host — which *can* read the
//! keychain — keeps the process confined without unsandboxing it.
//!
//! The token is only attached to a spawn whose leading executable is `gh`
//! (or a git publish subcommand). A host grant for GitHub is what
//! authorizes the projection: the same decision that lets the process
//! talk to GitHub also lets it speak *as* the user.

use crate::sandbox::EgressGrant;

/// Environment to inject into a confined spawn that needs GitHub identity.
///
/// `config_dir` is a writable directory inside the sandbox. Pointing
/// `GH_CONFIG_DIR` there stops `gh` from loading `~/.config/gh/hosts.yml`,
/// which names a keychain account the sandbox cannot open — that path
/// prints "Logged in (GH_TOKEN)" *and* "token in default is invalid" and
/// exits 1 even when projection worked.
pub fn github_identity_env(
    command: &str,
    grant: Option<&EgressGrant>,
    config_dir: &std::path::Path,
) -> Vec<(String, String)> {
    if !needs_github_identity(command) {
        return Vec::new();
    }
    if !grant.is_some_and(|g| g.permits_github()) {
        return Vec::new();
    }
    match host_gh_token() {
        Some(token) => vec![
            ("GH_TOKEN".into(), token.clone()),
            ("GH_ENTERPRISE_TOKEN".into(), token),
            ("GIT_TERMINAL_PROMPT".into(), "0".into()),
            (
                "GH_CONFIG_DIR".into(),
                config_dir.to_string_lossy().into_owned(),
            ),
        ],
        None => Vec::new(),
    }
}

/// `gh pr create` and `git push` update refs under `.git`, which the default
/// policy carves out as read-only.
pub fn needs_git_writes(command: &str) -> bool {
    let exe = leading_executable(command);
    if exe == "gh" {
        return true;
    }
    if exe != "git" {
        return false;
    }
    matches!(
        second_token(command),
        Some("push" | "fetch" | "pull" | "clone")
    )
}

pub fn needs_github_identity(command: &str) -> bool {
    let exe = leading_executable(command);
    if exe == "gh" {
        return true;
    }
    if exe != "git" {
        return false;
    }
    matches!(
        second_token(command),
        Some("push" | "fetch" | "pull" | "clone" | "ls-remote")
    )
}

fn leading_executable(command: &str) -> &str {
    let token = first_token(command).unwrap_or("");
    std::path::Path::new(token)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(token)
}

fn first_token(command: &str) -> Option<&str> {
    command
        .split_whitespace()
        .find(|token| !token.contains('='))
}

fn second_token(command: &str) -> Option<&str> {
    command
        .split_whitespace()
        .filter(|token| !token.contains('='))
        .nth(1)
}

fn host_gh_token() -> Option<String> {
    let output = std::process::Command::new("gh")
        .args(["auth", "token"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let token = String::from_utf8(output.stdout).ok()?;
    let token = token.trim();
    (!token.is_empty()).then(|| token.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gh_and_git_publish_need_identity_and_git_writes() {
        assert!(needs_github_identity("gh pr create --fill"));
        assert!(needs_github_identity("/opt/homebrew/bin/gh api user"));
        assert!(needs_github_identity("git push origin HEAD"));
        assert!(needs_git_writes("gh pr create"));
        assert!(needs_git_writes("git push"));
        assert!(!needs_github_identity("echo hello"));
        assert!(!needs_github_identity("cargo test"));
        assert!(!needs_git_writes("git status"));
        assert!(!needs_git_writes("echo gh pr create"));
    }

    #[test]
    fn env_assignments_are_skipped_when_finding_the_executable() {
        assert!(needs_github_identity("FOO=1 gh pr create"));
        assert_eq!(leading_executable("FOO=1 /usr/bin/gh pr create"), "gh");
    }

    #[test]
    fn identity_is_not_projected_without_a_github_grant() {
        assert!(
            github_identity_env("gh auth status", None, std::path::Path::new("/tmp")).is_empty()
        );
    }
}
