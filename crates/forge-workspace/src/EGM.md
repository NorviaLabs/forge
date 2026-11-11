# forge-workspace/src

## Overview

Implementation of workspace data services. `lib.rs` declares the public
modules; each module owns one concern:

- `file_context.rs`: active-file attachment context blocks.
- `git_status.rs`: parsing of `git status --porcelain=1 -z` into per-path
  staged/unstaged status.
- `git_service.rs`: scoped local Git operations (branches, staging, commits,
  merge state).
- `git_review.rs`: dirty-vs-HEAD diff parsing and hunks, discard/restore.
- `github.rs`: read-only issue discovery for the current remote.
- `gh.rs`: `gh` CLI adapter for pull requests, checks, review comments,
  issues, and CI logs.

## Interface

- `file_context`: `FileAttachment { rel_path, cursor_line }` with `new`,
  `label`; `build_attachment_text(...)`; `AttachmentError`.
- `git_status`: `GitStatusKind`, `PathStatus { staged, unstaged }` (+
  `primary`, `is_conflicted`), `GitStatusKind::is_more_severe`; crate-private
  `parse_null_terminated` (used by `LocalGit::status`).
- `git_service`: `GitServiceError`, `BranchStatus`, `ConflictState`,
  `LocalGit` (`new`, `root`, `status`, `branch_status`, `branches`,
  `current_branch`, `create_branch`, `switch_branch`, `delete_branch`,
  `rename_branch`, `abort_merge`, `conflict_state`, `stage`, `unstage`,
  `stage_all`, `commit`, `commit_all`, `pull`, `push`, `fetch`, `merge`).
- `git_review`: `DiffHunk`, `FileDiff`, `combined_diff`,
  `combined_diff_text`, `staged_diff_text`, `discard_hunk`, `restore_path`,
  `validated_relative_path`, `parse_file_diff`.
- `github`: `Issue`, `Label`, `GithubError`, `list_open_issues(&Path, u16)`,
  `repo_from_remote(&str)`.
- `gh`: `PullRequest`, `ClosingIssue`, `PrRepository`, `ReviewComment`,
  `ReviewAuthor`, `CheckDetails`, `IssueDetails`, `CiLogs`, `MAX_CI_LOG_BYTES`
  (64 KiB), `ci_logs(workspace, run_id, job_id: Option<u64>, max_bytes)`,
  plus check, PR/issue,
  review-comment, create-PR (`create_issue_pull_request_for_branch`,
  `create_pull_request`), PR validation, and `merge` functions.

## Invariants

- `LocalGit::new` requires the path to canonicalize to the worktree top level
  from `git rev-parse --show-toplevel`; otherwise `InvalidRoot`.
- Git subprocesses never run through a shell. Only `LocalGit`'s operation
  spawns set `GIT_TERMINAL_PROMPT=0`, `GIT_EDITOR=true`, and
  `GIT_MERGE_AUTOEDIT=no`; `LocalGit::new`'s `rev-parse` and `git_review`'s
  commands (which only scrub `GIT_DIR` and related variables) do not set
  them. `gh.rs`'s `git` calls set
  `GIT_TERMINAL_PROMPT=0`, close stdin, and run under a deadline (30 s;
  300 s for `push_branch`).
- Paths that `LocalGit` and `git_review` pass to Git are relative, contain only
  `Component::Normal`, never start with `.git`, and reject symlinked ancestors
  (including deleted paths).
- `git_review::discard_hunk` refuses untracked and binary files and
  out-of-range hunks; `restore_path` refuses untracked files.
- `github` is read-only (`MissingCli`, `MissingRemote`, `NotAuthenticated`
  errors) and `gh` commands are passed as argv, never a shell.
- `ci_logs` rejects a zero `run_id` or `job_id` and truncates logs at
  `max_bytes` clamped to `1..=MAX_CI_LOG_BYTES`, setting `truncated`.
- `gh::push_branch` passes `branch` to `git push -u origin` unvalidated.
  Precondition: `branch` equals `current_branch` (Git's own current branch),
  which its only caller checks first, so a leading `-` is not reachable.
- `gh::merge` accepts only `merge`, `squash`, or `rebase`.

## Side Effects

- Runs `git` and `gh` subprocesses; `push`, `pull`, `fetch`, PR creation,
  and issue queries reach the network and remote state.
- Mutates the Git index/worktree (`stage`, `commit`, `merge`, `restore`,
  `discard_hunk`, `abort_merge`).

## Verification

- Run: `cargo test --package forge-workspace --locked`
