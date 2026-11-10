# forge-session source

## Overview

Frontend-independent session assembly and multi-session coordination. This
crate turns a `Config` into a ready `AgentSession` (route table, MCP servers,
journal directory, governance), and owns the repository-level roster: an
exclusive lease, a SQLite control database, per-session actors behind a
command/event API, and immutable per-frame snapshots for frontends.

## Interface

`lib.rs`:

- `SessionTarget::{New, Resume(SessionId), Fork(SessionId)}`.
- `OpenedSession { session: AgentSession, notices: Vec<String> }`.
- `resolve_journal_dir(&Config) -> (PathBuf, Vec<String>)`.
- `route_table() -> forge_model::RouteTable` — the routing snapshot: `forge_connect::resolve` over `loaded_registry()` and `Credentials::shared()`. Used by `open_session` and by the supervisor (including `ReloadProviders`).
- `open_session(&Config, SessionTarget) -> anyhow::Result<OpenedSession>`.
- `starting_model(&Config) -> (model, profile_id)`: `("mock", "")` when `cfg.model.mock`; otherwise the last selection saved in `preferences.toml`, or empty. A new session with no model starts on it; a resumed or forked session keeps its own. Supervisor model inheritance falls back to it.
- Re-export: `forge_storage::RepositoryRuntimeStorage`.

`control.rs`:

- `WorktreeOwnership::{Primary, Managed, Attached}`; `SessionLifecycle::{Active, Archived, Unavailable, Removed, Retained}`; `SupervisorTurnState::{Idle, Queued, Running, Waiting, Completed, Failed, Cancelled, Interrupted}` with `label()`.
- `RepositorySession { session_id, label, workspace, branch, base_sha, ownership, lifecycle, turn_state, slot, model_id, profile_id (SQL column still `route_id`; a stored legacy route id is mapped through `profile_id_for_stored_route` on read), reasoning_effort, created_at, updated_at, archived_at, github_* }`.
- `NewRepositorySession`, `PendingCreation`, `CompletedCreation`, `StaleCreation`, `LeaseOwner { pid, started_at, workspace }`.
- `RepositoryLease::{acquire(control_dir, workspace), owner(), probe(control_dir)}` (`probe` is read-only; `acquire` retries a lock held only briefly by a prober, up to 4 attempts 20 ms apart, and reads the owner record bounded) (releases on `Drop`).
- `RepositoryControl::{open, path, sessions, session, register_session, mark_unavailable, set_turn_state, archive, mark_retained, mark_worktree_removed, set_selected, selected, assign_slot, rename, set_session_model, set_base_sha, set_session_branch, reconcile_worktrees, begin_managed_creation, begin_managed_creation_for_issue, mark_worktree_created, complete_creation, cancel_creation, stale_creations, forget_unavailable_primary, enqueue_prompt, queued_prompts, interrupted_prompts, claim_next_prompt, finish_prompt, cancel_queued_prompt_at, cancel_queued_prompt_by_id, reconcile_running_prompt_claims, interrupt_prompt_claim, set_github_auto_pr, set_pr_url, set_github_issue, increment_github_retry, reset_github_retry, set_github_tests_passed}`.
- `RepositorySessionError` — `Database`, `Io`, `Json`, `AlreadyOwned`, `WorkspaceInUse`, `InvalidStoredValue`, `InvalidSessionId`, `NotFound`, `MustStopBeforeArchive`, `TrustRequired`, `Archived`, `Retained`, `SlotOccupied`.

`headless.rs`:

- `ApprovalPolicy::{Ask (default), DenyAll, ApproveAll}`.
- `ApprovalRequired { payload: HitlPayload }` implementing `Display`/`Error`.
- `pub const UNCONFINED_REFUSED_UNDER_APPROVE_ALL`.
- `run_headless(AgentSession, prompt, ApprovalPolicy) -> anyhow::Result<ModelResponse>`.

`launch.rs`:

- `LaunchPlan::{prepare(&Config).await, prepare_with_trust_store(&Config, Option<PathBuf>).await, workspace, needs_trust, launch(self, LaunchTarget) -> Result<Launched, LaunchError>}`.
- `LaunchTarget::{New, Continue, Latest, Resume(SessionId), Fork(SessionId)}`.
- `Launched { bootstrap: RepositoryBootstrap, opened: OpenedSession, fork_from: Option<SessionId>, notices }` (`opened.notices` is moved into `notices`).
- `LaunchError::{Untrusted { workspace }, Repository(RepositorySupervisorError), OwnershipUnavailable { control_dir, source }, Session(anyhow::Error)}`.
- `config_for_workspace(&Config, &Path) -> Config`.

`snapshot.rs`:

- `SessionSnapshot` (+`capture(&AgentSession)`, `is_awaiting_approval`, `is_awaiting_question`, `workspace_root`, `Default`).
- `SessionDetailsSnapshot { journal_dir, active_model, active_profile_id, reasoning_effort, thinking_enabled, image_input_supported, token_usage_report, tools, skills, skill_descriptions, context_state, compaction, queue, background, session_pattern_allow_count }`.
- `ShellExecutionSnapshot`, `BackgroundTaskSnapshot` (+`capture`).
- `TranscriptSnapshot` (+`refresh`, `capture`, `from_messages`, `messages`, `events`, `revision`).
- `replayed_transcript(journal_dir, session_id, revision) -> Option<TranscriptSnapshot>`.

`supervisor.rs`:

- `SessionRuntimeSnapshot { task, session, transcript, details, queued_prompts, interrupted_prompts }`.
- `SupervisorEvent` (`#[non_exhaustive]`): `Roster`, `SessionUpdated(Box<_>)`, `Stream`, `Attention`, `Selected`, `Error`, `TrustRequired`.
- `QueuedPromptId::{Durable(u64), Session(QueueItemId)}`.
- `SupervisorCommand` (`#[non_exhaustive]`): creation/attach/archive/rename/pin/removal, `TrustWorkspace`, prompt submission and queue control, goal commands, `ContinueTurn`, `ForkSession`, `ResumeSession`, `CloseSession`, `CloseAllSessions`, `StopTurn`, `ResolveApproval`, `DecideApproval`, `ResolveQuestion`, `SelectSession`, `SetModel`, `SetThinking`, `SetApproveAll`, `SetCapabilities`, `PollSession`, `CompactContext`, background-task cancel/approve commands, `GrantEgressHost`, `AllowSessionPattern`, `ClearSessionApprovals`, `ReloadProviders`, GitHub issue commands (`CreateIssuePullRequest`, `RefreshIssuePullRequest`, `ApplyIssuePullRequestFeedback`), `Refresh`, `Shutdown`.
- `SupervisorHandle::{command, submit, try_command, subscribe}`.
- `RepositorySupervisorError` — `Control`, `Loop`, `Assembly`, `Model`, `Storage`, `Worktree`, `NoActor`, `Closed`, `Contention`, `Retiring`, `Command`.
- `RepositoryBootstrap::{acquire, lease_owner, session, recover_interrupted_creations, open_with_primary}`.
- `RepositorySupervisor::{open(cfg), open_with_primary(cfg, Option<AgentSession>), spawn, spawn_with_trust_store, snapshots, snapshot, selected_session_id, lease_owner, shutdown(self)}` — owns the actor map, roster, and event broadcast. `open`/`open_with_primary` acquire the lease themselves; `spawn` takes an already-held `RepositoryLease` and an explicit `max_concurrency`; `spawn_with_trust_store` exists so tests can redirect the trust store. The CLI path goes through `RepositoryBootstrap::open_with_primary`.

## Invariants

- The 200 ms background poll republishes a session (full `refresh_actor`) only when `snapshot::background_signature` of its background list differs from the last published snapshot's (status, latest message, finish time, pending approval id, shell output sizes).

- `LaunchPlan::prepare` writes nothing. It probes ownership read-only (`RepositoryLease::probe`: never creates or writes; ignores an `owner.lock` that is not a regular file, so a symlink or FIFO can neither redirect nor block it; reads at most 4 KiB of the owner record; takes only a shared lock, so concurrent probes never see each other) and refuses with `AlreadyOwned` when another process holds the lease, so a doomed launch is refused before any setup screen or trust/theme write. It also reads `needs_trust`. `launch` re-reads trust first (`Untrusted` otherwise, with nothing written: no `.forge`, no exclude edit, no control database opened, so crafted or symlinked repository content is never acted on before consent), then takes the lease: `AlreadyOwned` → `Repository`; any other bootstrap failure → `OwnershipUnavailable { control_dir, .. }`, naming `<project root>/.forge/local`. There is no lease-less path: Git repositories and directory projects alike run under the lease. For a directory project, a `Resume`/`Fork` target with no control-database row (a journal written by a lease-less Forge and migrated) opens with the launch config; the supervisor registers it as a `Directory` session on adoption. Then it runs `recover_interrupted_creations`, resolves the target and opens the session; the caller opens the supervisor. The probe is advisory: a process that takes the lease between `prepare` and `launch` is still refused by `launch`.
- `LaunchTarget::Continue` opens the control DB's selected session when it is `Active` and its journal exists; otherwise the most recent journal with a "selected session is no longer available" notice; with no journal at all, a new session with a "No previous session found" notice. Without a repository it behaves like `Latest`.
- `RepositorySupervisor::shutdown(self)` sends `Shutdown` (harmless if a handle already did), joins the command loop (which joins its detached operations), waits for every turn driver (`DriverTracker`), and only then drops the lease, which lives in `Option<RepositoryLease>` inside the shared state. When it returns `Ok`, the lease can be re-acquired immediately, in this process or another. The supervisor keeps its own command sender, so the command loop no longer ends merely because every `SupervisorHandle` was dropped while the supervisor is alive.
- Exactly one Forge process owns a repository group: `RepositoryLease::acquire` uses a non-truncating `try_lock` on `owner.lock`, and a losing process reports the incumbent `LeaseOwner` parsed from that file.
- The exclusive lease is taken before any session is created; the CLI acquires it, opens the primary session, then hands both to `open_with_primary`.
- Governance pattern rules: a repo-scope `allow` rule is never honored (only `deny` from the workspace file is merged); deny always wins.
- `resolve_journal_dir` treats `cfg.journal.path == default_journal_path()` as "unconfigured" and routes through `RepositoryRuntimeStorage` (`.forge/local/sessions` in a repo, app-data otherwise, emitting fallback notices). An explicit non-default path is honored as-is; relative paths anchor to the main worktree.
- Credentials are never exported into `std::env`: they are resolved per request through `forge_connect::Credentials::shared()` inside the route table, so a new key needs no rebuild; a provider definition change needs `ReloadProviders`.
- `open_session` composes: refused-key notices, one shared egress state created before MCP servers start, MCP connect and `register_into` failures as `"mcp: {e}"` notices, journal resolution notices, permission-file notices, the route table, and — only for a session with no model yet and a non-empty `starting_model` — image-input capability and context-window metadata from `ModelCatalogCache` (an unknown model keeps the conservative default). Resumed or forked sessions keep their own model and get no metadata here.
- Archive is refused while a session is `Queued`, `Running`, or `Waiting` (`archive_allowed`); untrusted sessions refuse to run (`TrustRequired`); `Retained` is a distinct, operator-visible end state from `Archived`.
- `RepositoryBootstrap::recover_interrupted_creations` (which the caller must invoke explicitly; `RepositorySupervisor` does not) cancels stale managed creations and only removes a worktree when it is clean.
- Headless approval semantics: `Ask` stops the turn and returns `ApprovalRequired`; `DenyAll` denies; `ApproveAll` approves governance prompts, grants a denied network host once per turn (a repeated host is denied with a note), and never approves sandbox escalation (`UNCONFINED_REFUSED_UNDER_APPROVE_ALL`). A turn is bounded at 64 approval rounds.
- `SessionSnapshot::capture` is the authoritative per-frame view (including `lifecycle`); the transcript is deliberately excluded and must come from `TranscriptSnapshot`.
- `TranscriptSnapshot::refresh` copies only when the fingerprint (`storage_id`, lengths, tail sizes) changes, and bumps `revision` at most once per real change. `from_messages` sets `events` empty and requires a caller-supplied `revision` distinct from the snapshot it replaces.
- `SupervisorEvent::SessionUpdated` is boxed because it dwarfs other variants and is broadcast on every turn transition.
- `QueuedPromptId::Durable` and `Session` namespaces are independent and must never be used to address each other.
- Commands carrying `expected_call_id`/`expected_queue_id` must still match the pending/visible item or the command is refused.
- `SupervisorHandle::try_command`/`submit` never block the terminal owner; execution failures are published as `SupervisorEvent::Error`.

- `ProjectTopology` (`topology.rs`) is plain data `{kind, root}` (`Git` → main worktree, `Directory` → the folder). Every runtime-storage construction and worktree listing in this crate goes through it; storage is built per call because the storage types are `!Sync`. The kind is detected on every launch, never persisted.
- Directory projects: storage is `LocalRuntimeStorage::directory_local(root)` (`<root>/.forge/local`). After the lease, `RepositoryBootstrap::acquire` runs `forge_storage::migrate_legacy_runtime_files(root)` (idempotent; notices via `migration_notices`, prepended to launch notices). Sessions use `WorktreeOwnership::Directory` (`'directory'`), `branch = ''`, `slot = None`, and all share the root: the unique index `sessions_live_workspace` is `WHERE lifecycle = 'active' AND ownership != 'directory'` (recreated on every open) and `register_session` skips `WorkspaceInUse` for them. Git sessions keep one live session per worktree path regardless of branch. `CreateSession` creates no worktree; `AttachWorktree`, `RemoveManagedWorktree` and the issue/pull-request commands return `RepositorySupervisorError::Unsupported`. `SupervisorHandle::supports_git()` and `RepositoryBootstrap::supports_git()` expose the kind.

- `SupervisorHandle::project_state() -> watch::Receiver<ProjectState>` (`project_state.rs`): `ProjectState { version, sessions: Vec<SessionSummary>, attention: Vec<AttentionItem>, running, limit }`. `AttentionItem { session_id, kind: Approval | Question | ChildApproval, call_id, since }`. A task started with the supervisor (aborted when it drops) derives the value from the actors' snapshots in roster order, never from event payloads: any non-`Stream` event or a `Lagged` error triggers one recomputation, and queued events are drained first so a burst costs one. A lost event can delay an update but never lose one. `since` is when the task first saw `(session_id, kind, call_id)` and carries over; `version` increases only when another field changes. `running` counts `Running` turns; `limit` is the supervisor's run slots.

- `ProjectRegistry` (`project.rs`): `projects.toml` (`version = 1`, `[[project]] { id, root, name, added_at }`) in `<config dir>/forge/`, or `ProjectRegistry::at(path)` in tests. A missing file is empty; another `version` is `UnknownVersion` and the file is never overwritten. Writes take `projects.lock`, re-read, validate, then write a temporary file and rename it. Roots are canonical (Git: main worktree via `ProjectTopology`); adding a registered root returns the existing project; nested roots are `Overlaps`. `resolve_launch(cwd)`: a registered project containing `cwd`, else a Git repository (registered without asking), else `NeedsConfirmation { root }` (writes nothing). `relocate` keeps the id; `remove` never touches the folder.
- `ProjectService` (`projects.rs`), reached through a cloneable `ProjectServiceHandle`: owns the registry and one `RepositorySupervisor` per open project; `open` loads config through `ProjectServiceOptions.load_config` (default `Config::load` with `project_root`), then `LaunchPlan::prepare_with_trust_store` → `NeedsTrust` (nothing written) or `AlreadyOwned { owner }` → `launch(Continue)` → `open_with_primary`. `close` refuses with `Busy { sessions }` while a session is queued, running or waiting, unless `stop_running`; it then calls `RepositorySupervisor::shutdown` bounded at 10 s (queued prompts are kept). `remove`/`relocate` refuse an open project (`Open`). `workspace()` is a `watch<WorkspaceState { version, projects: Vec<ProjectSummary { project, open, state }> }>` kept current by one forwarding task per open project, outside the command loop. `new_attention(prev, next)` returns items new by `(project, session, kind, call_id)`. Dropping every handle closes every project. Each project's forwarding task also records its supervisor's `Error` events and `Failed` attention as `ProjectSummary.last_error: Option<ProjectNotice { seq, session_id, message }>` (`seq` increases per error). `WorkspaceState::has_busy_sessions()` is true when any open project has a queued, running or waiting session. Open and close notices start with the project name.

## Isolation audit

Rule: after startup, no runtime path reads `current_dir` or a process-wide static, except user- or host-level state. Produced with `rg "current_dir\(\)|static [A-Z_]+: .*(Mutex|OnceLock|RwLock|LazyLock)" crates --type rust` on 2026-11-10; test-only hits omitted.

| Site | Outcome |
|---|---|
| `forge-tools/src/credentials.rs` `CREDENTIAL_CACHE` | Fixed: `git credential fill` runs in the workspace; keyed `(workspace, host)` |
| `forge-mcp/src/lib.rs` sandboxed spawn | Fixed: no workspace → `McpError::Protocol`, no `current_dir` fallback |
| `forge-config/src/lib.rs` `Config::load` | Fixed: `ConfigOverrides.project_root` replaces `current_dir` for discovery and workspace |
| `forge-config/src/lib.rs` `Config::default()` | Allowed: never used to build a project config |
| `forge-desktop/src/startup.rs` | Allowed: launch-time only, to pick the workspace |
| `forge-core/src/subagent.rs` `WRITER_GATES` | Allowed: keyed by root |
| `forge-connect` `Credentials::shared`, `store.rs` `WRITE_LOCK`, `builtin.rs` `SPECS` | Allowed: user-level |
| `forge-model` `FALLBACK_OPENCODE_SESSION` | Allowed: user-level |
| `forge-tools` `sandbox.rs` `APPLICABLE`, `git_network.rs` `EGRESS_CONNECT_EXE` | Allowed: host-level |

## Side Effects

- Creates directories and files: control dir, `sessions.db` (SQLite, WAL, `synchronous=NORMAL`, 1 connection), `owner.lock`, journal dirs, attachments.
- Mutates the `sessions`, `repository_state`, `pending_operations`, and queue tables; adopts a legacy `tasks` table by renaming it, and renames a legacy `tasks.db` (plus `-wal`/`-shm`) to `sessions.db` before opening.
- Spawns one command-loop task per supervisor and a driver task per running turn, bounded by a shared `Semaphore` of `max_concurrency` permits (`DEFAULT_MAX_CONCURRENCY` = 4), plus per-turn cancellation tokens; broadcasts events on a Tokio broadcast channel.
- Starts MCP server processes (`connect_all_in_workspace`) and installs search tools.
- Runs git operations through `forge_storage` (worktree create/remove/list); spawns no `git` subprocess itself outside tests.
- GitHub issue commands push branches and create or query pull requests and checks through `forge_workspace::gh` on `spawn_blocking`.
- Writes trust state and session model/route selections; writes goal state files.
- Reads the user config, permissions files, credential and preference stores.
- Network egress is possible through MCP servers and the model client.

## Verification

- Run: `cargo test --package forge-session --locked`
