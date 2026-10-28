# forge-session source

## Overview

Frontend-independent session assembly and multi-session coordination. This
crate turns a `Config` into a ready `AgentSession` (credentials, MCP servers,
journal directory, governance), and owns the repository-level roster: an
exclusive lease, a SQLite control database, per-session actors behind a
command/event API, and immutable per-frame snapshots for frontends.

## Interface

`lib.rs`:

- `SessionTarget::{New, Resume(SessionId), Fork(SessionId)}`.
- `OpenedSession { session: AgentSession, notices: Vec<String> }`.
- `resolve_journal_dir(&Config) -> (PathBuf, Vec<String>)`.
- `connect_credentials() -> Vec<(String, String)>`.
- `open_session(&Config, SessionTarget) -> anyhow::Result<OpenedSession>`.
- `starting_model(&Config) -> (model, profile_id)`: `("mock", "")` when `cfg.model.mock`; otherwise the last selection saved in `preferences.toml`, or empty. A new session with no model starts on it; a resumed or forked session keeps its own. Supervisor model inheritance falls back to it.
- Re-export: `forge_storage::RepositoryRuntimeStorage`.

`control.rs`:

- `WorktreeOwnership::{Primary, Managed, Attached}`; `SessionLifecycle::{Active, Archived, Unavailable, Removed, Retained}`; `SupervisorTurnState::{Idle, Queued, Running, Waiting, Completed, Failed, Cancelled, Interrupted}` with `label()`.
- `RepositorySession { session_id, label, workspace, branch, base_sha, ownership, lifecycle, turn_state, slot, model_id, profile_id (SQL column still `route_id`; a stored legacy route id is mapped through `profile_id_for_stored_route` on read), reasoning_effort, created_at, updated_at, archived_at, github_* }`.
- `NewRepositorySession`, `PendingCreation`, `CompletedCreation`, `StaleCreation`, `LeaseOwner { pid, started_at, workspace }`.
- `RepositoryLease::{acquire(control_dir, workspace), owner()}` (releases on `Drop`).
- `RepositoryControl::{open, path, sessions, session, register_session, mark_unavailable, set_turn_state, archive, mark_retained, mark_worktree_removed, set_selected, selected, assign_slot, rename, set_session_model, set_base_sha, set_session_branch, reconcile_worktrees, begin_managed_creation, begin_managed_creation_for_issue, mark_worktree_created, complete_creation, cancel_creation, stale_creations, forget_unavailable_primary, enqueue_prompt, queued_prompts, interrupted_prompts, claim_next_prompt, finish_prompt, cancel_queued_prompt_at, cancel_queued_prompt_by_id, reconcile_running_prompt_claims, interrupt_prompt_claim, set_github_auto_pr, set_pr_url, set_github_issue, increment_github_retry, reset_github_retry, set_github_tests_passed}`.
- `RepositorySessionError` — `Database`, `Io`, `Json`, `AlreadyOwned`, `WorkspaceInUse`, `InvalidStoredValue`, `InvalidSessionId`, `NotFound`, `MustStopBeforeArchive`, `TrustRequired`, `Archived`, `Retained`, `SlotOccupied`.

`headless.rs`:

- `ApprovalPolicy::{Ask (default), DenyAll, ApproveAll}`.
- `ApprovalRequired { payload: HitlPayload }` implementing `Display`/`Error`.
- `pub const UNCONFINED_REFUSED_UNDER_APPROVE_ALL`.
- `run_headless(AgentSession, prompt, ApprovalPolicy) -> anyhow::Result<ModelResponse>`.

`snapshot.rs`:

- `SessionSnapshot` (+`capture(&AgentSession)`, `is_awaiting_approval`, `is_awaiting_question`, `workspace_root`, `Default`).
- `SessionDetailsSnapshot { journal_dir, active_model, active_profile_id, reasoning_effort, thinking_enabled, image_input_supported, token_usage_report, tools, skills, skill_descriptions, context_state, compaction, queue, background, session_pattern_allow_count }`.
- `ShellExecutionSnapshot`, `BackgroundTaskSnapshot` (+`capture`).
- `TranscriptSnapshot` (+`refresh`, `capture`, `from_messages`, `messages`, `events`, `revision`).
- `replayed_transcript(journal_dir, session_id, revision) -> Option<TranscriptSnapshot>`.

`supervisor.rs`:

- `SessionRuntimeSnapshot { task, session, transcript, details, queued_prompts, interrupted_prompts }`.
- `SupervisorEvent` (`#[non_exhaustive]`): `Roster`, `SessionUpdated(Box<_>)`, `Stream`, `Attention`, `GoalStatus`, `GoalNotice`, `Selected`, `Error`, `TrustRequired`.
- `QueuedPromptId::{Durable(u64), Session(QueueItemId)}`.
- `SupervisorCommand` (`#[non_exhaustive]`): creation/attach/archive/rename/pin/removal, `TrustWorkspace`, prompt submission and queue control, goal commands, `ContinueTurn`, `ForkSession`, `ResumeSession`, `CloseSession`, `CloseAllSessions`, `StopTurn`, `ResolveApproval`, `DecideApproval`, `ResolveQuestion`, `SelectSession`, `SetModel`, `SetThinking`, `SetApproveAll`, `SetCapabilities`, `PollSession`, `CompactContext`, background-task cancel/approve commands, `GrantEgressHost`, `AllowSessionPattern`, `ClearSessionApprovals`, `ApplyProviderEnv`, `ClearProviderEnv`, `Refresh`, `Shutdown`.
- `SupervisorHandle::{command, submit, try_command, subscribe}`.
- `RepositorySupervisorError` — `Control`, `Loop`, `Assembly`, `Model`, `Storage`, `Worktree`, `NoActor`, `Closed`, `Contention`, `Retiring`, `Command`.
- `RepositoryBootstrap::{acquire, lease_owner, session, recover_interrupted_creations, open_with_primary}`.
- `RepositorySupervisor::{open(cfg), open_with_primary(cfg, Option<AgentSession>), spawn, spawn_with_trust_store, snapshots, snapshot, selected_session_id, lease_owner}` — owns the actor map, roster, and event broadcast. `open`/`open_with_primary` acquire the lease themselves; `spawn` takes an already-held `RepositoryLease` and an explicit `max_concurrency`; `spawn_with_trust_store` exists so tests can redirect the trust store. The CLI path goes through `RepositoryBootstrap::open_with_primary`.

## Invariants

- Exactly one Forge process owns a repository group: `RepositoryLease::acquire` uses a non-truncating `try_lock` on `owner.lock`, and a losing process reports the incumbent `LeaseOwner` parsed from that file.
- The exclusive lease is taken before any session is created; the CLI acquires it, opens the primary session, then hands both to `open_with_primary`.
- Governance pattern rules: a repo-scope `allow` rule is never honored (only `deny` from the workspace file is merged); deny always wins.
- `resolve_journal_dir` treats `cfg.journal.path == default_journal_path()` as "unconfigured" and routes through `RepositoryRuntimeStorage` (`.forge/local/sessions` in a repo, app-data otherwise, emitting fallback notices). An explicit non-default path is honored as-is; relative paths anchor to the main worktree.
- Credentials are injected into the model client, never exported into `std::env`; variables already exported in the environment win over the stored value.
- `open_session` composes: refused-key notices, one shared egress state created before MCP servers start, MCP connect failures as `"mcp: {e}"` notices, journal resolution notices, permission-file notices, and model capability/context-window metadata from `ModelCatalogCache` (an unknown model keeps the conservative default).
- Archive is refused while a session is `Queued`, `Running`, or `Waiting` (`archive_allowed`); untrusted sessions refuse to run (`TrustRequired`); `Retained` is a distinct, operator-visible end state from `Archived`.
- `RepositorySupervisor` recovers interrupted managed creations on startup, cancels stale ones, and only removes a worktree when it is clean.
- Headless approval semantics: `Ask` stops the turn and returns `ApprovalRequired`; `DenyAll` denies; `ApproveAll` approves governance prompts, grants a denied network host once per turn (a repeated host is denied with a note), and never approves sandbox escalation (`UNCONFINED_REFUSED_UNDER_APPROVE_ALL`). A turn is bounded at 64 approval rounds.
- `SessionSnapshot::capture` is the authoritative per-frame view (including `lifecycle`); the transcript is deliberately excluded and must come from `TranscriptSnapshot`.
- `TranscriptSnapshot::refresh` copies only when the fingerprint (`storage_id`, lengths, tail sizes) changes, and bumps `revision` at most once per real change. `from_messages` sets `events` empty and requires a caller-supplied `revision` distinct from the snapshot it replaces.
- `SupervisorEvent::SessionUpdated` is boxed because it dwarfs other variants and is broadcast on every turn transition.
- `QueuedPromptId::Durable` and `Session` namespaces are independent and must never be used to address each other.
- Commands carrying `expected_call_id`/`expected_queue_id` must still match the pending/visible item or the command is refused.
- `SupervisorHandle::try_command`/`submit` never block the terminal owner; execution failures are published as `SupervisorEvent::Error`.

## Side Effects

- Creates directories and files: control dir, `sessions.db` (SQLite, WAL, `synchronous=NORMAL`, 1 connection), `owner.lock`, journal dirs, attachments.
- Mutates the `sessions`, `repository_state`, `pending_operations`, and queue tables; adopts a legacy `tasks` table by renaming it.
- Spawns a background supervisor task per session with up to `DEFAULT_MAX_CONCURRENCY` (4) concurrent turns, plus per-turn cancellation tokens; broadcasts events on a Tokio broadcast channel.
- Starts MCP server processes (`connect_all_in_workspace`) and installs search tools.
- Runs git operations through `forge_storage` (worktree create/remove/list) and `git` subprocesses.
- Writes trust state and session model/route selections; writes goal state files.
- Reads the user config, permissions files, credential and preference stores.
- Network egress is possible through MCP servers and the model client.

## Verification

- Run: `cargo test --package forge-session --locked`
