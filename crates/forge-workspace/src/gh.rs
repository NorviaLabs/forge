//! Minimal `gh` CLI adapter. Commands are passed as argv (never through a shell).
use std::{path::Path, process::Command};

use serde::Deserialize;

use crate::github::{GithubError, Issue};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub state: String,
    #[serde(default)]
    pub is_draft: bool,
    #[serde(default)]
    pub review_decision: Option<String>,
    #[serde(rename = "baseRefName")]
    pub base_branch: String,
    #[serde(rename = "headRefName")]
    pub head_branch: String,
    #[serde(default, rename = "headRefOid")]
    pub head_sha: String,
    #[serde(default)]
    pub head_repository: Option<PrRepository>,
    #[serde(default)]
    pub head_repository_owner: Option<ReviewAuthor>,
    #[serde(default)]
    pub is_cross_repository: bool,
    #[serde(default)]
    pub closing_issues_references: Vec<ClosingIssue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ClosingIssue {
    pub number: u64,
    #[serde(default)]
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PrRepository {
    pub name: String,
}

pub fn branch_protection_required_approvals(
    workspace: &Path,
    base: &str,
) -> Result<u64, GithubError> {
    let repo = repository(workspace)?;
    let base = base.replace('/', "%2F");
    let endpoint = format!("repos/{repo}/branches/{base}/protection/required_pull_request_reviews");
    match run_gh(workspace, ["api", &endpoint]) {
        Ok(bytes) => {
            #[derive(Deserialize)]
            struct Reviews {
                required_approving_review_count: u64,
            }
            Ok(serde_json::from_slice::<Reviews>(&bytes)?.required_approving_review_count)
        }
        Err(GithubError::Command(error))
            if error.contains("404") || error.contains("Not Found") =>
        {
            Ok(0)
        }
        Err(error) => Err(error),
    }
}

pub fn branch_protection_configured(workspace: &Path, base: &str) -> Result<bool, GithubError> {
    let repo = repository(workspace)?;
    let base = base.replace('/', "%2F");
    let endpoint = format!("repos/{repo}/branches/{base}/protection");
    match run_gh(workspace, ["api", &endpoint]) {
        Ok(_) => Ok(true),
        Err(GithubError::Command(error))
            if error.contains("404") || error.contains("Not Found") =>
        {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

pub fn branch_protection_status_checks(
    workspace: &Path,
    base: &str,
) -> Result<Vec<String>, GithubError> {
    let repo = repository(workspace)?;
    let base = base.replace('/', "%2F");
    let endpoint = format!("repos/{repo}/branches/{base}/protection/required_status_checks");
    let output = run_gh(workspace, ["api", &endpoint]);
    match output {
        Ok(bytes) => {
            #[derive(Deserialize)]
            struct Checks {
                contexts: Vec<String>,
            }
            let value: Checks = serde_json::from_slice(&bytes)?;
            Ok(value.contexts)
        }
        Err(GithubError::Command(error))
            if error.contains("404") || error.contains("Not Found") =>
        {
            Ok(Vec::new())
        }
        Err(error) => Err(error),
    }
}

/// True when repository rules require another approval. If the API is unavailable,
/// fail closed so the caller does not treat unknown policy as permission to merge.
pub fn branch_protection_requires_review(
    workspace: &Path,
    base: &str,
) -> Result<bool, GithubError> {
    let repo = repository(workspace)?;
    let base = base.replace('/', "%2F");
    let endpoint = format!("repos/{repo}/branches/{base}/protection/required_pull_request_reviews");
    let output = run_gh(workspace, ["api", &endpoint]);
    match output {
        Ok(_) => Ok(true),
        Err(GithubError::Command(error))
            if error.contains("404") || error.contains("Not Found") =>
        {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

pub fn wait_for_checks(
    workspace: &Path,
    reference: &str,
    max_polls: usize,
) -> Result<Vec<String>, GithubError> {
    for _ in 0..max_polls.max(1) {
        let current = checks(workspace, reference)?;
        if !current.is_empty()
            && current
                .iter()
                .all(|row| !row.split_whitespace().any(|part| part == "pending"))
        {
            return Ok(current);
        }
        std::thread::sleep(std::time::Duration::from_secs(10));
    }
    checks(workspace, reference)
}

pub fn pull_request_for_issue(
    workspace: &Path,
    issue_number: u64,
) -> Result<Option<PullRequest>, GithubError> {
    let Some(pr) = find_issue_pull_request(workspace, issue_number)? else {
        return Ok(None);
    };
    if pr.state != "OPEN" {
        return Ok(None);
    }
    Ok(Some(pr))
}

pub fn resolve_reviews_and_checks(
    workspace: &Path,
    issue_number: u64,
) -> Result<Option<MergeGate>, GithubError> {
    let Some(pr) = pull_request_for_issue(workspace, issue_number)? else {
        return Ok(None);
    };
    let gate = merge_gate(workspace, &pr.number.to_string())?;
    Ok(Some(gate))
}

pub fn create_issue_pull_request(
    workspace: &Path,
    issue_number: u64,
) -> Result<String, GithubError> {
    let branch = current_branch(workspace)?;
    create_issue_pull_request_for_branch(workspace, issue_number, &branch)
}

pub fn create_issue_pull_request_for_branch(
    workspace: &Path,
    issue_number: u64,
    expected_branch: &str,
) -> Result<String, GithubError> {
    let issue = issue(workspace, &issue_number.to_string())?;
    let repo = repository(workspace)?;
    let base = run_gh(
        workspace,
        [
            "repo",
            "view",
            "--repo",
            &repo,
            "--json",
            "defaultBranchRef",
            "--jq",
            ".defaultBranchRef.name",
        ],
    )?;
    let base = String::from_utf8_lossy(&base).trim().to_string();
    if base.is_empty() {
        return Err(GithubError::Command(
            "could not determine repository default branch".into(),
        ));
    }
    validate_clean_worktree(workspace)?;
    let base_ref = std::process::Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args([
            "rev-parse",
            "--verify",
            &format!("origin/{base}^{{commit}}"),
        ])
        .output()
        .map_err(|error| GithubError::Command(error.to_string()))?;
    if !base_ref.status.success() {
        return Err(GithubError::Command(format!(
            "base branch `{base}` is not fetched locally"
        )));
    }
    let branch = std::process::Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["branch", "--show-current"])
        .output()
        .map_err(|error| GithubError::Command(error.to_string()))?;
    let branch = String::from_utf8_lossy(&branch.stdout).trim().to_string();
    if branch.is_empty() || branch != expected_branch {
        return Err(GithubError::Command(
            "PR creation branch does not match the issue task branch".into(),
        ));
    }
    if let Some(existing) = find_issue_pull_request(workspace, issue.number)? {
        validate_issue_pull_request(&existing, &repo, issue_number, expected_branch)?;
        return Ok(existing.url);
    }
    create_pull_request(
        workspace,
        &issue.title,
        &format!("Closes #{}\n\n{}", issue.number, issue.url),
        &base,
    )
}

pub fn validate_clean_worktree(workspace: &Path) -> Result<(), GithubError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["status", "--porcelain"])
        .output()
        .map_err(|error| GithubError::Command(error.to_string()))?;
    if !output.status.success() {
        return Err(GithubError::Command(
            "could not inspect local Git changes".into(),
        ));
    }
    if !output.stdout.is_empty() {
        return Err(GithubError::Command(
            "commit or discard working-tree changes before creating a PR".into(),
        ));
    }
    Ok(())
}

pub fn current_branch(workspace: &Path) -> Result<String, GithubError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["branch", "--show-current"])
        .output()
        .map_err(|e| GithubError::Command(e.to_string()))?;
    if !output.status.success() {
        return Err(GithubError::Command(
            "could not determine current branch".into(),
        ));
    }
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if branch.is_empty() {
        return Err(GithubError::Command(
            "PR creation requires a named branch".into(),
        ));
    }
    Ok(branch)
}

pub fn push_branch(workspace: &Path, branch: &str) -> Result<(), GithubError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["push", "-u", "origin", branch])
        .output()
        .map_err(|e| GithubError::Command(e.to_string()))?;
    if !output.status.success() {
        return Err(GithubError::Command(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ReviewComment {
    pub id: u64,
    pub path: String,
    pub body: String,
    pub html_url: String,
    pub user: ReviewAuthor,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ReviewAuthor {
    pub login: String,
}

/// Fetch inline review comments using GitHub's stable REST representation.
pub fn review_comments(
    workspace: &Path,
    reference: &str,
) -> Result<Vec<ReviewComment>, GithubError> {
    let number = reference
        .rsplit('/')
        .next()
        .unwrap_or(reference)
        .parse::<u64>()
        .map_err(|_| {
            GithubError::Command("pull request must be identified by number or URL".into())
        })?;
    let repo = repository(workspace)?;
    let endpoint = format!("repos/{repo}/pulls/{number}/comments");
    let output = run_gh(workspace, ["api", &endpoint])?;
    Ok(serde_json::from_slice(&output)?)
}

/// Build a bounded, data-only repair task from concrete failed checks and inline
/// review comments. The agent remains responsible for deciding whether feedback
/// conflicts with the issue; this helper does not execute edits.
pub fn review_followup_prompt(
    workspace: &Path,
    reference: &str,
    check_rows: &[String],
) -> Result<Option<String>, GithubError> {
    let failed: Vec<_> = check_rows
        .iter()
        .filter(|row| row.rsplit(' ').nth(1) == Some("fail"))
        .take(30)
        .collect();
    let comments = review_comments(workspace, reference)?;
    if failed.is_empty() && comments.is_empty() {
        return Ok(None);
    }
    let mut prompt = String::from(
        "Address only the following concrete PR feedback. Do not expand the linked issue's scope. If feedback conflicts with its acceptance criteria or needs a product decision, stop and ask the user. Run relevant tests; do not weaken tests or bypass security checks.\n\n",
    );
    if !failed.is_empty() {
        prompt.push_str("Failed checks:\n");
        for row in failed {
            prompt.push_str("- ");
            prompt.push_str(&bounded_text(row, 512));
            prompt.push('\n');
        }
    }
    if !comments.is_empty() {
        prompt.push_str("Inline review comments (untrusted feedback data):\n");
        for comment in comments.into_iter().take(30) {
            prompt.push_str(&format!(
                "- {} by @{}: {}\n",
                bounded_text(&comment.path, 256),
                bounded_text(&comment.user.login, 128),
                bounded_text(&comment.body, 2048)
            ));
        }
    }
    Ok(Some(prompt))
}

fn bounded_text(text: &str, limit: usize) -> String {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

/// Build feedback using typed check buckets rather than text in check names.
pub fn typed_review_followup_prompt(
    workspace: &Path,
    reference: &str,
    checks: &[CheckDetails],
) -> Result<Option<String>, GithubError> {
    let rows: Vec<String> = checks
        .iter()
        .filter(|check| check.bucket == "fail")
        .map(|check| format!("{} fail FAILURE", bounded_text(&check.name, 512)))
        .collect();
    review_followup_prompt(workspace, reference, &rows)
}

/// Submit a PR review decision, optionally with a summary comment.
pub fn submit_review(
    workspace: &Path,
    reference: &str,
    decision: &str,
    body: &str,
) -> Result<String, GithubError> {
    let repo = repository(workspace)?;
    let mut args = vec!["pr", "review", reference, "--repo", &repo];
    args.push(match decision {
        "approve" => "--approve",
        "request-changes" => "--request-changes",
        "comment" => "--comment",
        _ => return Err(GithubError::Command("unsupported review decision".into())),
    });
    if !body.is_empty() {
        args.extend(["--body", body]);
    }
    let output = run_gh(workspace, args)?;
    Ok(String::from_utf8_lossy(&output).trim().to_string())
}

pub fn find_issue_pull_request(
    workspace: &Path,
    issue_number: u64,
) -> Result<Option<PullRequest>, GithubError> {
    let repo = repository(workspace)?;
    let output = run_gh(
        workspace,
        [
            "pr",
            "list",
            "--repo",
            &repo,
            "--state",
            "all",
            "--search",
            &format!("{issue_number} in:body"),
            "--json",
            "number,title,url,state,isDraft,reviewDecision,baseRefName,headRefName,headRefOid,headRepository,headRepositoryOwner,isCrossRepository,closingIssuesReferences",
        ],
    )?;
    let prs: Vec<PullRequest> = serde_json::from_slice(&output)?;
    let mut matches = prs.into_iter().filter(|pr| {
        pr.state == "OPEN"
            && pr.closing_issues_references.iter().any(|issue| {
                issue.number == issue_number
                    && issue.url == format!("https://github.com/{repo}/issues/{issue_number}")
            })
    });
    let first = matches.next();
    if matches.next().is_some() {
        return Err(GithubError::Command(
            "multiple open PRs close this issue; select a PR explicitly".into(),
        ));
    }
    Ok(first)
}

pub fn issue(workspace: &Path, reference: &str) -> Result<Issue, GithubError> {
    let repo = repository(workspace)?;
    let output = run_gh(
        workspace,
        [
            "issue",
            "view",
            reference,
            "--repo",
            &repo,
            "--json",
            "number,title,url,state,labels",
        ],
    )?;
    Ok(serde_json::from_slice(&output)?)
}

pub fn create_pull_request(
    workspace: &Path,
    title: &str,
    body: &str,
    base: &str,
) -> Result<String, GithubError> {
    let repo = repository(workspace)?;
    let output = run_gh(
        workspace,
        [
            "pr", "create", "--repo", &repo, "--title", title, "--body", body, "--base", base,
        ],
    )?;
    Ok(String::from_utf8_lossy(&output).trim().to_string())
}

pub fn pull_request(workspace: &Path, reference: &str) -> Result<PullRequest, GithubError> {
    let repo = repository(workspace)?;
    let output = run_gh(
        workspace,
        [
            "pr",
            "view",
            reference,
            "--repo",
            &repo,
            "--json",
            "number,title,url,state,isDraft,reviewDecision,baseRefName,headRefName,headRefOid,headRepository,headRepositoryOwner,isCrossRepository,closingIssuesReferences",
        ],
    )?;
    Ok(serde_json::from_slice(&output)?)
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckDetails {
    pub bucket: String,
    pub name: String,
    pub state: String,
    #[serde(default)]
    pub link: String,
    #[serde(default)]
    pub workflow: String,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub completed_at: Option<String>,
    #[serde(default)]
    pub description: String,
}

pub fn check_details(workspace: &Path, reference: &str) -> Result<Vec<CheckDetails>, GithubError> {
    let repo = repository(workspace)?;
    // `gh pr checks` returns 1 for failed checks and 8 for pending checks,
    // even when it produced a valid JSON response.
    let mut command = Command::new("gh");
    command
        .args([
            "pr",
            "checks",
            reference,
            "--repo",
            &repo,
            "--json",
            "bucket,name,state,link,workflow,startedAt,completedAt,description",
        ])
        .current_dir(workspace);
    let output = bounded_command(&mut command, MAX_GH_OUTPUT_BYTES, GH_TIMEOUT)?;
    if output.truncated {
        return Err(GithubError::Command(
            "GitHub CLI output exceeded the safety limit".into(),
        ));
    }
    if !output.status.success() && !matches!(output.status.code(), Some(1 | 8)) {
        return Err(gh_command_error(&output.stderr));
    }
    if output.stdout.is_empty() && !output.status.success() {
        return Err(gh_command_error(&output.stderr));
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

pub fn checks(workspace: &Path, reference: &str) -> Result<Vec<String>, GithubError> {
    Ok(check_details(workspace, reference)?
        .into_iter()
        .map(|row| format!("{} {} {}", row.name, row.bucket, row.state))
        .collect())
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueDetails {
    #[serde(flatten)]
    pub issue: Issue,
    pub body: String,
    pub author: ReviewAuthor,
    #[serde(default)]
    pub assignees: Vec<ReviewAuthor>,
    pub created_at: String,
    pub updated_at: String,
}

pub fn issue_details(workspace: &Path, reference: &str) -> Result<IssueDetails, GithubError> {
    let repo = repository(workspace)?;
    let output = run_gh(
        workspace,
        [
            "issue",
            "view",
            reference,
            "--repo",
            &repo,
            "--json",
            "number,title,url,state,labels,body,author,assignees,createdAt,updatedAt",
        ],
    )?;
    Ok(serde_json::from_slice(&output)?)
}

pub const MAX_CI_LOG_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CiLogs {
    pub text: String,
    pub truncated: bool,
}

/// Read logs for a workflow run or one of its jobs. Drain stdout without retaining
/// more than the requested bound so large logs cannot exhaust memory or block gh.
pub fn ci_logs(
    workspace: &Path,
    run_id: u64,
    job_id: Option<u64>,
    max_bytes: usize,
) -> Result<CiLogs, GithubError> {
    if run_id == 0 || job_id == Some(0) {
        return Err(GithubError::Command(
            "run and job IDs must be positive".into(),
        ));
    }
    let repo = repository(workspace)?;
    let mut command = Command::new("gh");
    command.args(["run", "view", &run_id.to_string(), "--repo", &repo, "--log"]);
    if let Some(job_id) = job_id {
        command.args(["--job", &job_id.to_string()]);
    }
    command.current_dir(workspace);
    let output = bounded_command(
        &mut command,
        max_bytes.clamp(1, MAX_CI_LOG_BYTES),
        GH_TIMEOUT,
    )?;
    if !output.status.success() {
        return Err(gh_command_error(&output.stderr));
    }
    let bytes = output.stdout;
    let mut truncated = output.truncated;
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    let limit = max_bytes.clamp(1, MAX_CI_LOG_BYTES);
    if text.len() > limit {
        let mut end = limit;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        truncated = true;
    }
    Ok(CiLogs { text, truncated })
}

fn bounded_output(
    mut reader: impl std::io::Read,
    limit: usize,
) -> std::io::Result<(Vec<u8>, bool)> {
    let mut bytes = Vec::new();
    let mut truncated = false;
    let mut buffer = [0; 8192];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        let keep = count.min(limit.saturating_sub(bytes.len()));
        bytes.extend_from_slice(&buffer[..keep]);
        truncated |= keep < count;
    }
    Ok((bytes, truncated))
}

fn gh_spawn_error(error: std::io::Error) -> GithubError {
    if error.kind() == std::io::ErrorKind::NotFound {
        GithubError::MissingCli
    } else {
        GithubError::Command(error.to_string())
    }
}

fn gh_command_error(stderr: &[u8]) -> GithubError {
    let error = String::from_utf8_lossy(stderr);
    if error.contains("not logged") || error.contains("auth login") {
        GithubError::NotAuthenticated
    } else {
        GithubError::Command(error.trim().to_string())
    }
}

pub fn merge(workspace: &Path, reference: &str, method: &str) -> Result<String, GithubError> {
    if !matches!(method, "merge" | "squash" | "rebase") {
        return Err(GithubError::Command("unsupported merge method".into()));
    }
    let repo = repository(workspace)?;
    let output = run_gh(
        workspace,
        [
            "pr",
            "merge",
            reference,
            "--repo",
            &repo,
            &format!("--{method}"),
        ],
    )?;
    Ok(String::from_utf8_lossy(&output).trim().to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeGate {
    pub checks: Vec<String>,
    pub review_decision: Option<String>,
}

impl MergeGate {
    /// GitHub CLI `pr checks` reports each check's conclusion in its row.
    /// A `pass` substring is the only successful textual state; unknown states fail closed.
    pub fn github_ready(&self) -> bool {
        let checks_ready = !self.checks.is_empty()
            && self
                .checks
                .iter()
                .all(|line| line.rsplit(' ').nth(1) == Some("pass"));
        let review_ready = self
            .review_decision
            .as_deref()
            .is_some_and(|decision| matches!(decision, "APPROVED"));
        checks_ready && review_ready
    }
}

pub fn merge_gate(workspace: &Path, reference: &str) -> Result<MergeGate, GithubError> {
    let pr = pull_request(workspace, reference)?;
    let checks = checks(workspace, reference)?;
    Ok(MergeGate {
        checks,
        review_decision: pr.review_decision,
    })
}

/// Merge only after GitHub checks/reviews and Forge's own tests are successful.
pub fn merge_if_ready(
    workspace: &Path,
    reference: &str,
    method: &str,
    forge_tests_passed: bool,
) -> Result<String, GithubError> {
    if !forge_tests_passed {
        return Err(GithubError::Command("Forge tests have not passed".into()));
    }
    if !merge_gate(workspace, reference)?.github_ready() {
        return Err(GithubError::Command(
            "GitHub checks or required review are not ready".into(),
        ));
    }
    merge(workspace, reference, method)
}

pub fn repository(workspace: &Path) -> Result<String, GithubError> {
    let remote = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["remote", "get-url", "origin"])
        .output()
        .map_err(|e| GithubError::Command(e.to_string()))?;
    if !remote.status.success() {
        return Err(GithubError::MissingRemote);
    }
    super::github::repo_from_remote(&String::from_utf8_lossy(&remote.stdout))
        .ok_or(GithubError::MissingRemote)
}

pub(crate) const GH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const MAX_GH_OUTPUT_BYTES: usize = 2 * 1024 * 1024;

struct BoundedCommandOutput {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    truncated: bool,
}

fn bounded_command(
    command: &mut Command,
    limit: usize,
    timeout: std::time::Duration,
) -> Result<BoundedCommandOutput, GithubError> {
    use std::{
        process::Stdio,
        sync::mpsc,
        time::{Duration, Instant},
    };
    let deadline = Instant::now() + timeout;
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(gh_spawn_error)?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let (out_tx, out_rx) = mpsc::channel();
    let (err_tx, err_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = out_tx.send(bounded_output(stdout, limit));
    });
    std::thread::spawn(move || {
        let _ = err_tx.send(bounded_output(stderr, 4096));
    });
    let result = (|| {
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                result => {
                    return Err(GithubError::Command(match result {
                        Err(error) => error.to_string(),
                        _ => "GitHub CLI deadline exceeded".into(),
                    }))
                }
            }
        };
        let (stdout, truncated) = out_rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| GithubError::Command("GitHub CLI stdout deadline exceeded".into()))?
            .map_err(|error| GithubError::Command(error.to_string()))?;
        let (stderr, _) = err_rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| GithubError::Command("GitHub CLI stderr deadline exceeded".into()))?
            .map_err(|error| GithubError::Command(error.to_string()))?;
        Ok(BoundedCommandOutput {
            status,
            stdout,
            stderr,
            truncated,
        })
    })();
    if result.is_err() {
        terminate_command_tree(&mut child);
        // Group termination closes inherited pipes. Allow readers to report EOF
        // without an unbounded join (a descendant can deliberately escape a group).
        let cleanup_deadline = Instant::now() + Duration::from_secs(1);
        let _ = out_rx.recv_timeout(cleanup_deadline.saturating_duration_since(Instant::now()));
        let _ = err_rx.recv_timeout(cleanup_deadline.saturating_duration_since(Instant::now()));
    }
    result
}

fn terminate_command_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        // The child leads its own process group, even if it has already exited.
        // Use the platform utility to avoid a new dependency or unsafe FFI.
        let _ = Command::new("/bin/kill")
            .args(["-KILL", "--", &format!("-{}", child.id())])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    // Non-Unix fallback terminates only the direct child; std has no portable
    // process-tree termination API. Pipe cleanup remains time-bounded above.
    let _ = child.kill();
    let _ = child.wait();
}

pub(crate) fn run_gh(
    workspace: &Path,
    args: impl IntoIterator<Item = impl AsRef<std::ffi::OsStr>>,
) -> Result<Vec<u8>, GithubError> {
    let mut command = Command::new("gh");
    command.args(args).current_dir(workspace);
    let output = bounded_command(&mut command, MAX_GH_OUTPUT_BYTES, GH_TIMEOUT)?;
    if !output.status.success() {
        return Err(gh_command_error(&output.stderr));
    }
    if output.truncated {
        return Err(GithubError::Command(
            "GitHub CLI output exceeded the safety limit".into(),
        ));
    }
    Ok(output.stdout)
}

/// Resolve the recorded PR directly where possible; validate repository, issue,
/// and head identity before exposing it to session actions.
pub fn issue_session_pull_request(
    workspace: &Path,
    issue_number: u64,
    branch: &str,
    recorded_url: Option<&str>,
) -> Result<Option<PullRequest>, GithubError> {
    let repo = repository(workspace)?;
    let pr = match recorded_url {
        Some(url) => {
            let prefix = format!("https://github.com/{repo}/pull/");
            let number = url
                .strip_prefix(&prefix)
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|number| *number > 0)
                .ok_or_else(|| {
                    GithubError::Command(
                        "recorded PR belongs to another repository or has an invalid URL".into(),
                    )
                })?;
            Some(pull_request(workspace, &number.to_string())?)
        }
        None => find_issue_pull_request(workspace, issue_number)?,
    };
    if let Some(pr) = &pr {
        validate_issue_pull_request(pr, &repo, issue_number, branch)?;
    }
    Ok(pr)
}

pub fn validate_issue_pull_request(
    pr: &PullRequest,
    repo: &str,
    issue_number: u64,
    branch: &str,
) -> Result<(), GithubError> {
    validate_pull_request_branch(pr, branch)?;
    let head_repo = pr
        .head_repository
        .as_ref()
        .zip(pr.head_repository_owner.as_ref())
        .map(|(repository, owner)| format!("{}/{}", owner.login, repository.name));
    if pr.is_cross_repository
        || head_repo.as_deref() != Some(repo)
        || pr.url != format!("https://github.com/{repo}/pull/{}", pr.number)
        || !pr.closing_issues_references.iter().any(|issue| {
            issue.number == issue_number
                && issue.url == format!("https://github.com/{repo}/issues/{issue_number}")
        })
    {
        return Err(GithubError::Command(
            "PR repository or closing issue does not match the issue session".into(),
        ));
    }
    Ok(())
}

/// Do not attach feedback or status from a different issue worktree's PR.
pub fn validate_pull_request_branch(
    pr: &PullRequest,
    expected_branch: &str,
) -> Result<(), GithubError> {
    if expected_branch.is_empty() || pr.head_branch != expected_branch {
        return Err(GithubError::Command(
            "PR head does not match the issue session branch".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_pr_identity_rejects_cross_repo_and_fork_heads() {
        let mut pr: PullRequest = serde_json::from_str(r#"{"number":7,"title":"Fix","url":"https://github.com/a/b/pull/7","state":"OPEN","baseRefName":"main","headRefName":"fix","headRefOid":"abc","headRepository":{"name":"b"},"headRepositoryOwner":{"login":"a"},"closingIssuesReferences":[{"number":12,"url":"https://github.com/a/b/issues/12"}]}"#).unwrap();
        assert_eq!(pr.head_sha, "abc");
        assert!(validate_issue_pull_request(&pr, "a/b", 12, "fix").is_ok());
        pr.closing_issues_references[0].url = "https://github.com/other/b/issues/12".into();
        assert!(validate_issue_pull_request(&pr, "a/b", 12, "fix").is_err());
        pr.closing_issues_references[0].url = "https://github.com/a/b/issues/12".into();
        pr.head_repository_owner.as_mut().unwrap().login = "fork".into();
        assert!(validate_issue_pull_request(&pr, "a/b", 12, "fix").is_err());
    }

    #[test]
    fn named_pass_is_not_a_passing_check_and_feedback_is_byte_bounded() {
        let gate = MergeGate {
            checks: vec!["pass fail FAILURE".into()],
            review_decision: Some("APPROVED".into()),
        };
        assert!(!gate.github_ready());
        assert_eq!(bounded_text("ééé", 3), "é");
    }

    #[cfg(unix)]
    #[test]
    fn deadline_terminates_descendants_of_exited_parent() {
        let temp = tempfile::tempdir().unwrap();
        let marker = temp.path().join("survived");
        let mut command = Command::new("sh");
        command
            .args([
                "-c",
                r#"(sleep 1; printf survived > "$1") & exit 0"#,
                "test",
            ])
            .arg(&marker);
        let start = std::time::Instant::now();
        assert!(bounded_command(&mut command, 100, std::time::Duration::from_millis(50)).is_err());
        assert!(start.elapsed() < std::time::Duration::from_millis(900));
        std::thread::sleep(std::time::Duration::from_millis(1100));
        assert!(!marker.exists(), "descendant must not survive the deadline");
    }

    #[test]
    fn command_deadline_kills_sleeping_child() {
        let start = std::time::Instant::now();
        let mut command = Command::new("sleep");
        command.arg("5");
        let result = bounded_command(&mut command, 10, std::time::Duration::from_millis(50));
        assert!(matches!(result, Err(GithubError::Command(error)) if error.contains("deadline")));
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
    }

    #[test]
    fn command_drains_oversized_output_without_deadlock() {
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "i=0; while [ $i -lt 10000 ]; do printf '0123456789'; i=$((i+1)); done",
        ]);
        let result = bounded_command(&mut command, 100, std::time::Duration::from_secs(5)).unwrap();
        assert!(result.status.success());
        assert_eq!(result.stdout.len(), 100);
        assert!(result.truncated);
    }

    #[test]
    fn dirty_tree_is_rejected_before_pr_push() {
        let temp = tempfile::tempdir().unwrap();
        assert!(Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(temp.path())
            .status()
            .unwrap()
            .success());
        assert!(validate_clean_worktree(temp.path()).is_ok());
        std::fs::write(temp.path().join("untracked"), "work").unwrap();
        assert!(validate_clean_worktree(temp.path()).is_err());
    }

    #[test]
    fn github_pr_fields_decode_and_branch_is_validated() {
        let pr: PullRequest = serde_json::from_str(r#"{"number":7,"title":"Fix","url":"https://github.com/a/b/pull/7","state":"OPEN","isDraft":false,"reviewDecision":null,"baseRefName":"main","headRefName":"forge/fix","closingIssuesReferences":[{"number":12}]}"#).unwrap();
        assert_eq!(pr.base_branch, "main");
        assert_eq!(pr.closing_issues_references[0].number, 12);
        assert!(validate_pull_request_branch(&pr, "forge/fix").is_ok());
        assert!(validate_pull_request_branch(&pr, "wrong").is_err());
        assert!(validate_pull_request_branch(&pr, "").is_err());
    }

    #[test]
    fn typed_check_and_issue_details_decode() {
        let row: CheckDetails = serde_json::from_str(r#"{"bucket":"fail","name":"test","state":"FAILURE","link":"https://github.com/a/b/actions/runs/5/job/6","workflow":"CI","startedAt":null,"completedAt":null,"description":"failed"}"#).unwrap();
        assert_eq!(row.bucket, "fail");
        assert_eq!(row.workflow, "CI");
        let issue: IssueDetails = serde_json::from_str(r#"{"number":12,"title":"Fix","url":"url","state":"OPEN","body":"Acceptance criteria","author":{"login":"alice"},"assignees":[],"createdAt":"today","updatedAt":"today"}"#).unwrap();
        assert_eq!(issue.issue.number, 12);
        assert_eq!(issue.body, "Acceptance criteria");
    }

    #[test]
    fn logs_are_bounded_and_fully_drained() {
        let mut input = std::io::Cursor::new(vec![b'x'; MAX_CI_LOG_BYTES * 3]);
        let (bytes, truncated) = bounded_output(&mut input, 10).unwrap();
        assert_eq!(bytes.len(), 10);
        assert!(truncated);
        assert_eq!(input.position(), (MAX_CI_LOG_BYTES * 3) as u64);
        assert_eq!(
            bounded_output(&b"abc"[..], 3).unwrap(),
            (b"abc".to_vec(), false)
        );
    }
}
