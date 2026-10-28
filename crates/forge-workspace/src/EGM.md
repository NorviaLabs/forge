# forge-workspace/src

## Overview

Implementation of workspace data services. `lib.rs` declares the public
modules; each module owns one concern:

- `file_context.rs`: active-file attachment context blocks.
- `file_ops.rs`: workspace-confined create/rename/delete operations.
- `git_status.rs`: polled status/diff cache for the file explorer.
- `git_service.rs`: scoped local Git operations (branches, staging, commits,
  merge state).
- `git_review.rs`: dirty-vs-HEAD diff parsing and hunks, discard/restore.
- `git_sync.rs`: polled branch refresh plus `pull`/`push`/`fetch`/`merge`/
  `switch`/`create`/branch delete and rename/merge-abort operations.
- `github.rs`: read-only issue discovery for the current remote.
- `gh.rs`: `gh` CLI adapter for pull requests, checks, reviews, issues, and CI
  logs.

## Interface

- `file_context`: `FileAttachment { rel_path, cursor_line }` with `new`,
  `label`; `build_attachment_text(...)`; `AttachmentError`.
- `file_ops`: `FileOperationKind`, `DeleteMode` (`Trash`, `Permanent`),
  `EntryKind`, `FileOperationResult`, `FileOperationError` (+ `actionable`),
  `WorkspaceFileOps::new(validate_name, plan_create, plan_rename, create_file,
  create_directory, rename_entry, delete_entry, entry_kind,
  is_non_empty_directory)`.
- `git_status`: `GitStatusKind`, `PathStatus` (+ `primary`, `is_untracked`,
  `is_conflicted`), `GitStatusKind::marker`/`is_more_severe`, `ChangedFile`,
  `DiffKind` (`Unstaged`, `CombinedVsHead`, `Staged`), `GitStatusCache` (`new`,
  `start_refresh`, `poll`, `get`, `changed_files`, `path_status`, `revision`,
  `request_unstaged_diff`/`request_combined_diff`/`request_staged_diff`,
  `poll_diff`, `get_*_diff`).
- `git_service`: `GitServiceError`, `BranchStatus`, `ConflictState`,
  `LocalGit` (`new`, `root`, `status`, `branch_status`, `branches`,
  `current_branch`, `create_branch`, `switch_branch`, `delete_branch`,
  `rename_branch`, `abort_merge`, `conflict_state`, `stage`, `unstage`,
  `stage_all`, `commit`, `commit_all`, `pull`, `push`, `fetch`, `merge`).
- `git_review`: `DiffHunk`, `FileDiff`, `Reviewability`, `reviewability`,
  `combined_diff`, `combined_diff_text`, `staged_diff_text`, `discard_hunk`,
  `restore_path`, `delete_untracked`, `parse_file_diff`, `stage_path`,
  `unstage_path`.
- `git_sync`: `SyncOperation` (`Pull`, `Push`, `Merge`, `Switch`, `Create`,
  `Fetch`, `DeleteBranch { branch, force }`, `RenameBranch { from, to }`,
  `AbortMerge`; with `label`/`active`/`finished`), `SyncOutcome`, `GitSyncCache`
  (`start_branch_refresh`, `poll`, `upstream`, `start_operation`,
  `poll_operation`).
- `github`: `Issue`, `Label`, `GithubError`, `list_open_issues(&Path, u16)`,
  `repo_from_remote(&str)`.
- `gh`: `PullRequest`, `ClosingIssue`, `PrRepository`, `ReviewComment`,
  `ReviewAuthor`, `CheckDetails`, `IssueDetails`, `CiLogs`, `MAX_CI_LOG_BYTES`
  (64 KiB), `ci_logs(workspace, run_id, job_id: Option<u64>, max_bytes)`,
  `MergeGate { checks, review_decision }` (+ `github_ready`), plus
  branch-protection, check-wait, PR/issue, review-comment/submission,
  create-PR, PR validation, `merge`, `merge_gate`, and `merge_if_ready`
  functions.

## Invariants

- `WorkspaceFileOps::new` canonicalizes the root once; every later path is
  resolved against it and re-checked (`ensure_inside`), with symlink metadata
  inspected rather than followed for existing entries.
- Names are validated before any filesystem call: empty, `.`/`..`, absolute, or
  platform-invalid names fail with a typed `FileOperationError`; creating an
  existing entry fails with `AlreadyExists`.
- `WorkspaceFileOps` deletion is no-follow: a symlink is removed as a link,
  never traversed. `DeleteMode::Trash` reports `TrashUnavailable` instead of
  silently falling back to permanent deletion.
- `git_review::delete_untracked` is a separate, permanent delete path (no
  Trash mode): it validates the path like other `git_review` calls, refuses
  tracked paths, and uses `symlink_metadata` so a symlink is removed rather
  than followed; directories are removed with `remove_dir_all`.
- `LocalGit::new` requires the path to canonicalize to the worktree top level
  from `git rev-parse --show-toplevel`; otherwise `InvalidRoot`.
- Git subprocesses never run through a shell. Only `LocalGit`'s operation
  spawns set `GIT_TERMINAL_PROMPT=0`, `GIT_EDITOR=true`, and
  `GIT_MERGE_AUTOEDIT=no`; `LocalGit::new`'s `rev-parse`, `git_review`'s
  commands (which only scrub `GIT_DIR` and related variables), `git_status`'s
  status/unstaged-diff reads, and `gh.rs`'s `git` calls do not set them.
- Paths that `LocalGit` and `git_review` pass to Git are relative, contain only
  `Component::Normal`, never start with `.git`, and reject symlinked ancestors
  (including deleted paths). `git_status`'s unstaged diff passes the caller's
  `path` to `git diff --` unvalidated.
- `git_status`/`git_sync` caches are refreshed by a worker thread and observed
  only through `poll`; `start_refresh` increments `revision` and clears the
  diff cache on every call, and diff requests are answered into the cache,
  not returned directly.
- `git_sync` operations are serialized; `poll_operation` yields at most one
  `SyncOutcome` per started operation. A merge conflict is a normal outcome
  left in `conflicts`, not a refusal.
- `git_review::discard_hunk` refuses untracked and binary files and
  out-of-range hunks; `restore_path` refuses untracked files.
- `github` is read-only (`MissingCli`, `MissingRemote`, `NotAuthenticated`
  errors) and `gh` commands are passed as argv, never a shell.
- `ci_logs` rejects a zero `run_id` or `job_id` and truncates logs at
  `max_bytes` clamped to `1..=MAX_CI_LOG_BYTES`, setting `truncated`.
- `gh::push_branch` passes `branch` to `git push -u origin` unvalidated.
  Precondition: `branch` equals `current_branch` (Git's own current branch),
  which its only caller checks first, so a leading `-` is not reachable.
- `gh::merge` accepts only `merge`, `squash`, or `rebase`. `MergeGate::github_ready`
  fails closed: it needs a non-empty check list where every row reports
  `pass`, plus a review decision of exactly `APPROVED`; `merge_if_ready`
  refuses unless `forge_tests_passed` is true and that gate is ready.

## Side Effects

- Creates, renames, deletes, and trashes files/directories inside the workspace.
- Runs `git` and `gh` subprocesses; `push`, `pull`, `fetch`, PR creation,
  review submission, and issue queries reach the network and remote state.
- Mutates the Git index/worktree (`stage`, `commit`, `merge`, `restore`,
  `discard_hunk`, `abort_merge`).
- Spawns worker threads for status/diff/sync polling; the caller must keep
  polling to drain results.

## Verification

- Run: `cargo test --package forge-workspace --locked`
