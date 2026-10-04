//! Read-only GitHub CLI integration for the current workspace remote.
//!
//! Write operations belong to the task workflow, not this discovery client.
use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Issue {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub state: String,
    #[serde(default)]
    pub labels: Vec<Label>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Label {
    pub name: String,
}

#[derive(Debug, thiserror::Error)]
pub enum GithubError {
    #[error("GitHub CLI (`gh`) is not installed")]
    MissingCli,
    #[error("workspace has no GitHub remote")]
    MissingRemote,
    #[error("GitHub CLI is not authenticated; run `gh auth login`")]
    NotAuthenticated,
    #[error("GitHub CLI failed: {0}")]
    Command(String),
    #[error("could not parse GitHub CLI response: {0}")]
    Parse(#[from] serde_json::Error),
}

/// List open issues for the repository identified by this workspace's GitHub remote.
/// Executes `gh` directly (never through a shell) and requests a fixed field set.
pub fn list_open_issues(workspace: &Path, limit: u16) -> Result<Vec<Issue>, GithubError> {
    let repo = super::gh::repository(workspace)?;
    let output = super::gh::run_gh(
        workspace,
        [
            "issue",
            "list",
            "--repo",
            &repo,
            "--state",
            "open",
            "--limit",
            &limit.clamp(1, 100).to_string(),
            "--json",
            "number,title,url,state,labels",
        ],
    )?;
    Ok(serde_json::from_slice(&output)?)
}

pub fn repo_from_remote(remote: &str) -> Option<String> {
    let value = remote
        .trim()
        .strip_prefix("https://")
        .or_else(|| remote.trim().strip_prefix("http://"))
        .or_else(|| remote.trim().strip_prefix("git@"))?;
    let value = value
        .strip_prefix("github.com:")
        .or_else(|| value.strip_prefix("github.com/"))?;
    let value = value.strip_suffix(".git").unwrap_or(value);
    let mut parts = value.split('/');
    let owner = parts.next()?;
    let repo = parts.next()?;
    if owner.is_empty() || repo.is_empty() || parts.next().is_some() {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

#[cfg(test)]
mod tests {
    use super::repo_from_remote;

    #[test]
    fn parses_common_github_remote_forms() {
        assert_eq!(
            repo_from_remote("https://github.com/acme/forge.git"),
            Some("acme/forge".into())
        );
        assert_eq!(
            repo_from_remote("git@github.com:acme/forge.git"),
            Some("acme/forge".into())
        );
        assert_eq!(
            repo_from_remote("https://github.com/acme/forge"),
            Some("acme/forge".into())
        );
    }

    #[test]
    fn rejects_non_github_and_malformed_remotes() {
        assert_eq!(repo_from_remote("https://gitlab.com/acme/forge.git"), None);
        assert_eq!(repo_from_remote("https://github.com/acme"), None);
    }
}
