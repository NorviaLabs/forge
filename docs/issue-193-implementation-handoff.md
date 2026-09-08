# Issue #193 Implementation Handoff

## Objective

Implement Git worktrees and concurrent multi-session task support for Forge, based on the approved high-fidelity UX mockups.

Current branch: `feat/issue-193-ux-mockups`

No commit has been created. All changes are currently in the working tree.

The `grill-me` skill was used during planning and UX exploration.

## Authoritative Product Decisions

These decisions are settled and should not be revisited unless the user explicitly changes scope:

- One active agent session per worktree.
- Forge manages created worktrees or attaches existing worktrees from the same repository.
- A repository supervisor owns concurrent session actors.
- Default runnable concurrency is four actors; waiting actors release capacity and requeue FIFO.
- Repository-wide SQLite control DB plus centralized session journals.
- Repository group anchors to Git’s main worktree and protects `.forge/local`.
- Exclusive repository-group lease; competing Forge processes fail with owner metadata.
- Managed worktrees branch from the initiating worktree’s committed `HEAD`.
- Session/worktree bindings are immutable; branch/path drift makes a session unavailable.
- Stop cancels the current turn but keeps the session active.
- Archive is final; only stopped sessions may archive.
- Only clean archived managed worktrees may be removed; branches are retained.
- Attached worktrees are never deleted by Forge.
- No automatic merge/rebase/cherry-pick integration in v1.
- Model settings are per-session; provider authentication/transport is global.
- Non-Git workspaces retain single-session behavior.
- UX uses a persistent task strip, searchable task switcher, task-local UI state, attention badges/toasts, and explicit New/Attach/Archive/Cleanup flows.

## Implemented Areas

### Mockups

Tracked interactive mockups:

- `docs/mockups/issue-193-multi-task/index.html`
- `docs/mockups/issue-193-multi-task/styles.css`
- `docs/mockups/issue-193-multi-task/app.js`

`.gitignore` was updated so this mockup directory is not hidden by the repository-wide `docs` rule.

### Storage and Worktrees

Files:

- `crates/forge-storage/src/resolver.rs`
- `crates/forge-storage/src/worktree.rs`
- `crates/forge-storage/src/lib.rs`

Implemented symbols:

- `RepositoryRuntimeStorage`
- `RuntimeDataKind::Control`
- `RuntimeDataKind::Worktree`
- `main_worktree`
- `create_task_worktree`
- `remove_clean_worktree`
- `WorktreeRecord`

Runtime data resolves through the Git main worktree and `.forge/local`. Managed worktree removal checks `git status --porcelain` and refuses dirty worktrees. Branches are retained on removal.

### Repository Control Plane

Files:

- `crates/forge-session/src/control.rs`
- `crates/forge-session/src/lib.rs`

Implemented symbols:

- `RepositoryControl`
- `RepositoryLease`
- `RepositoryTask`
- `NewRepositoryTask`
- `PendingCreation`
- `WorktreeOwnership`
- `SessionLifecycle`
- `SupervisorTurnState`

The SQLite schema includes:

- repository selection state
- task roster
- unique live workspace binding
- unique live pinned slot
- pending managed-worktree operations
- durable prompt queue

Lifecycle protections currently include:

- archive refuses queued/running/waiting tasks
- archived tasks reject new prompts
- managed tasks in `awaiting_trust` reject new prompts
- pin conflicts require explicit swap
- reconciliation marks missing/path-drift/branch-drift tasks unavailable
- creation cancellation records an error and removes the provisional task row

### Concurrent Supervisor

File:

- `crates/forge-session/src/supervisor.rs`

Implemented symbols:

- `RepositorySupervisor`
- `SupervisorHandle`
- `SupervisorCommand`
- `SupervisorEvent`
- `SessionRuntimeSnapshot`
- `RepositorySupervisor::open`
- `RepositoryBootstrap::open_with_primary`

Supported commands include:

- submit prompt
- continue/stop turn
- resolve approval/question
- select task
- set model
- create managed task
- attach worktree
- rename/archive/pin task
- remove managed worktree
- finalize/cancel pending creation
- refresh/shutdown

The supervisor owns actor sessions behind `tokio::Mutex`, broadcasts roster/task/stream/attention/error events, and schedules turns with a shared semaphore.

### Core Cancellation

Files:

- `crates/forge-core/src/lib.rs`
- `crates/forge-core/src/session/create.rs`
- `crates/forge-core/src/session/inspect.rs`
- `crates/forge-core/src/session/turn_ops.rs`
- `crates/forge-core/src/stream.rs`

Implemented symbols:

- `AgentSession::request_turn_cancel`
- `AgentSession::begin_turn_cancellation_scope`

Root-turn cancellation is separate from subagent identity/cancellation.

### CLI Startup

File:

- `crates/forge-cli/src/main.rs`

Repository workspaces acquire a bootstrap lease before opening the primary
session, then pass that already-open runtime to
`RepositoryBootstrap::open_with_primary`. The supervisor adopts it as the
primary `SessionActor` and opens the remaining active Sessions from their
journals. Non-Git workspaces continue with legacy single-session mode. The
repository TUI receives an initial `SessionRuntimeSnapshot` and supervisor
handle; the direct launcher has no supervisor compatibility field.

### TUI Task Chrome and Switcher

Files:

- `crates/forge-tui/src/layout.rs`
- `crates/forge-tui/src/widgets/task_strip.rs`
- `crates/forge-tui/src/widgets/mod.rs`
- `crates/forge-tui/src/app/types.rs`
- `crates/forge-tui/src/app/new.rs`
- `crates/forge-tui/src/app/render.rs`
- `crates/forge-tui/src/app/input.rs`
- `crates/forge-tui/src/app/chrome.rs`
- `crates/forge-tui/src/app/commands.rs`
- `crates/forge-tui/src/app/overlays.rs`
- `crates/forge-tui/src/commands.rs`
- `crates/forge-tui/src/app/tests/focus.rs`

Implemented:

- `FocusBlock::TaskStrip`
- task strip layout row in task mode
- semantic lifecycle coloring
- attention badge rendering
- task-strip left/right/Home/End navigation
- task-strip stop/continue/pin/archive shortcuts
- `/sessions`
- task switcher overlay backed by supervisor roster snapshots
- switcher search by label/branch/workspace/session ID
- `n` New and `a` Attach entry points
- task input overlay for New/Attach
- trust confirmation overlay for managed creation
- trust cancellation rollback
- roster/task/stream/attention/error event polling
- sibling prompt routing through `SupervisorCommand::SubmitPrompt`
- task-local view-state structure for draft, workspace navigation, source viewer, conversation view, focus, overlay, and attachments

Current shortcut implementation preserves the existing last-turn expansion binding:

- `Ctrl+T` remains last-turn expansion.
- `F3` opens the task switcher.
- `/sessions` is the discoverable command.

The help copy still says `Ctrl+T` opens the task switcher and must be corrected or the binding must be remapped deliberately.

## Current Validation

All green as of this pass:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
```

Clippy now runs clean workspace-wide (it had never completed on this branch).
Three pre-existing lints in `forge-session` were fixed along the way: the lease
lock file opened with `create` but no `truncate` policy, a redundant closure in
`cancel_creation`, and `SupervisorEvent::TaskUpdated` carrying an unboxed
640-byte snapshot through a broadcast channel.

## Closed in This Pass

### 1. Managed creation queues first prompt before trust — fixed

`pending_operations` gained a `first_prompt` column (added by an idempotent
`PRAGMA table_info` migration, so an existing control database upgrades in
place). `CreateTask` parks the prompt there; `FinalizeCreation` hands it back
via `CompletedCreation`, enqueues it, and starts the driver. The
`awaiting_trust` guard is untouched — nothing bypasses it.

Covered by `control::tests::a_first_prompt_is_parked_until_trust_completes_the_creation`
and `supervisor::tests::a_first_prompt_runs_only_after_trust_finalizes_the_creation`.

### 2. Exclusive lease is acquired too late in CLI startup — fixed

New `RepositoryBootstrap::acquire` takes the lease and opens the control
database before `open_session`; `RepositoryBootstrap::open_with_primary`
adopts the already-open primary. `RepositoryTaskError::AlreadyOwned` now
renders the owning pid, start time and workspace instead of a `Debug` blob.

### 5. Task-local view state — substantially completed

`TaskLocalViewState` now moves (not clones) the editor session/command/message
and viewport, diff view, stream preview and thinking, activity feed, banners,
turn stats, tool detail, turn expansion, approval and question presentation
state, busy/status/search chrome, pending turn and interaction, queue/task
cursors, turn timing, and the per-task model and effort — alongside the
original composer, navigation, source viewer, conversation view, focus,
overlay and attachments. `save` leaves the app blank so nothing bleeds across a
switch, and the conversation render cache is dropped on install.

Still excluded, deliberately: the interactive terminal and bottom panel (the
terminal is independent of the selected task by design), provider credentials
(authentication is global), and the explorer tree and its dialogs — those are
rooted at a workspace path, and file paths still resolve against the primary
workspace, so they belong with gap 4 below.

### 6. Task switcher UX — done

Rows are grouped (Needs you / Active / Unavailable / Archived) and rendered as
a `Table` rather than hand-padded list strings. `r` renames, `x` archives
behind a confirmation, `d` removes a clean managed worktree behind a
confirmation, and a `d` on an attached or still-live task explains which rule
blocks it rather than doing nothing.

### 7. New/Attach input UX — done

New mode now takes a label plus an optional first prompt; Attach takes label,
branch and workspace. Both support paste, clear per-field validation errors
that move the cursor to the offending field and clear as you type, and `~`
expansion with relative paths resolved against the launch directory. Membership
in the repository is validated by the supervisor, which refuses the main
worktree, a path Git does not list as a worktree of this repository, a branch
that does not match what is checked out, and a workspace already bound to a
live task (`RepositoryTaskError::WorkspaceInUse`).

### 8. Managed worktree source HEAD — fixed

`CreateTask` now branches from `state.cfg.resolved_workspace` (the initiating
worktree) rather than the repository main worktree.

### 9. Trust handling — fixed

`FinalizeCreation` records trust for the created workspace and rolls the whole
creation back if persistence fails. The trust store is injectable
(`spawn_with_trust_store`) so tests never touch the developer's real
`trust.toml`. A new `SupervisorCommand::TrustWorkspace` exists for the attach
flow's confirmation step.

### 10. Help binding — fixed

The task-strip help now reads `F3 or /sessions`, and the unimplemented
`Alt+1…9` pinned-slot line was removed. Guarded by
`app::tests::multi_task::the_task_strip_help_advertises_the_binding_that_is_actually_wired`.

### Recovery and lifecycle tests added

- `control::tests::interrupted_creations_are_reported_as_stale_until_resolved`
- `control::tests::attaching_the_same_worktree_twice_names_the_live_task`
- `control::tests::a_control_database_without_first_prompt_migrates_in_place`
- `control::tests::a_lease_conflict_names_the_owning_process`
- `supervisor::tests::cancelling_trust_removes_the_worktree_and_the_task_row`
- `supervisor::tests::attach_refuses_the_main_worktree_a_foreign_path_and_a_wrong_branch`

`RepositoryBootstrap::recover_interrupted_creations` rolls back creations an
earlier process left in `awaiting_trust` and reports each rollback as a startup
notice; the CLI calls it before opening the primary session.

## Remaining Gaps

### 3. Primary runtime ownership — complete

Repository startup adopts the already-open primary into a `SessionActor`; it is
not reopened from disk and is not retained by `TuiApp`. Primary and managed
Sessions share the same prompt, streaming, queue, stop, HITL, question,
continuation, model-setting, and background-task command paths.

`TuiApp` selects a Session by ID and reads `SessionRuntimeSnapshot`,
`TranscriptSnapshot`, and `SessionViewState`. Direct `AgentSession` access is
retained only for the non-repository single-session launcher and its tests.

### 4. Remaining compatibility boundary

`DirectSessionSlot` remains intentionally as the explicit direct-mode owner.
It is not populated by `new_supervised`, and no repository-mode command or
render path falls back to it. The old sibling-opening compatibility APIs and temporary
cutover validation workflows have been removed.

## Recommended Next Order

1. Review and merge the stacked multisession PRs bottom-up through the
   primary-supervisor cutover.
2. Exercise the repository UI against a real disposable Git repository,
   especially switching, terminal cwd, editor/diff state, resume, and exit
   summary.
3. Keep `DirectSessionSlot` isolated to the non-repository launcher until a
   future removal no longer improves clarity.
4. Re-run:

```sh
CARGO_HOME="$HOME/.cargo" cargo fmt --all -- --check
CARGO_HOME="$HOME/.cargo" cargo clippy --workspace --all-targets --locked -- -D warnings
CARGO_HOME="$HOME/.cargo" cargo test --workspace --all-targets --locked
```

This handoff is historical; the current implementation continues on the
multisession stack (`#562` and follow-up cleanup work).
