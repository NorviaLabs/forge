# forge-core src — Agent Loop

## Overview

Agent-loop orchestration. `AgentSession` owns the transcript, the durable
journal adapter, the tool registry, the model client, governance, context
engine, lifecycle state, queue, background-task registry, compaction state, and
token accounting. Sibling modules implement the individual concerns; the
session's inherent methods live in `session/`.

## Interface

- Session and public types (`lib.rs`, `types.rs`):
  - `AgentSession` fields: `session_id`, `messages` (`SharedMessages`),
    `events`, `active_task`, `active_model`, `active_profile_id`,
    `token_usage`, `last_completion`.
  - `LoopError`, `LoopConfig { workspace, journal_dir, enable_context_lifecycle,
    enable_governance, web_search, agents }`, `TurnEvent`,
    `SessionTokenUsage`, `TokenUsageReport`, `ApplyOutcome`, `ResumeReport`.
- Turn coordination (`turn.rs`): `TurnCoordinator::run` (private).
- Model streaming (`stream.rs`): `ModelStepAccumulator`,
  `accumulate_stream_event`, `merge_streamed_response`, `stream_turn_event`,
  `observe_stream_event`.
- Lifecycle (`lifecycle.rs`): `ActiveTaskState`, `TransitionReason`,
  `TransitionError`; legal-transition table.
- Completion evidence (`completion.rs`): `TaskExpectation`,
  `ToolExpectation`, `FileEffectKind`/`FileEffectExpectation`,
  `GitEffectKind`/`GitEffectExpectation`, `ExecutionEvent`, `EvidenceEntry`,
  `ExecutionEvidence`, `CompletionReason`, `EvidenceSummary`,
  `CompletionDecision`, `CompletionEvaluator`, `DefaultCompletionEvaluator`.
- Background work (`background.rs`): `BackgroundTaskKind`,
  `BackgroundTaskStatus`, `BackgroundTaskHandle`, `BackgroundControl`,
  `BackgroundTaskRegistry`, `BackgroundTaskOutcome`, `BackgroundRunId`,
  `BackgroundShellExecution`, `BackgroundChildExecution`,
  `BackgroundApprovalRequest`, `DescendantWorktree` (re-exported).
  `BackgroundTaskOutcome` is `pub` in the private module but not re-exported.
- Subagents (`subagent.rs`): `SubagentSpec`, `SubagentOutcome`.
- Agent coordination (`agent_coordinator.rs`): `AgentCoordinator`,
  `AgentCoordinatorConfig`, `AgentCoordinatorError`, `AgentStatus`,
  `AgentActivity`, `AgentSnapshot`, `AgentWaitResult`.
- Queue (`queue.rs`): `QueuedTask`, `TaskQueue`.
- Permission/egress (`permission.rs`): `EgressRuntime`,
  `egress_policy_for_workspace` (re-exported). `start_egress` and
  `start_egress_with_shared` are `pub` inside the private module and are not
  re-exported, so they are crate-internal.
- Audit (`audit.rs`): durable `SessionAudit` queue (crate-private).
- Resume (`resume.rs`): journaled tool replay and incomplete-intent
  reconciliation (methods on `AgentSession`).
- Prompt/helpers (`helpers.rs`): public `session_messages`,
  `session_title_hint`, `TURN_FAILED_MARKER`; crate-private
  `assemble_system_prompt`, `restore_system_message`,
  `compress_recognized_command_output`.
- Re-exported from `session/`: `PendingContextCompaction`,
  `CompletedContextCompaction`, `compact_tokens`, `GrantOutcome`,
  `GrantRejection`, `AppliedGrant`, `pattern_grant_eligible`,
  `validate_grant_decision`, and the `*ToolApplication`/`*HitlExecution` types.
- Model selection (`model_selection.rs`): `ModelExecutionSelection`.
- Isolation (`work.rs`): `IsolatedTask`.
- Internal plumbing: `persistence::SessionPersistence`, `task_runtime::TaskRuntime`,
  `turn_state::TurnState`.

Subdirectory:

- [session](./session/EGM.md) — `AgentSession` inherent methods by concern.

## Invariants

- Only one foreground task is active per session; any number of background tasks
  may run alongside it.
- `ActiveTaskState` is the single source of truth for working/waiting state and
  wait reason; UI code must not derive its own copy.
- Legality of transitions is pure and table-driven; terminal states require a
  new task/attempt.
- A turn is `Completed` only when runtime evidence satisfies the classified
  `TaskExpectation`; model text alone is never sufficient, and `Unclassifiable`
  never resolves to `Completed`.
- Background-task registry state is not itself durable; callers must journal
  lifecycle transitions before or alongside mutating it.
- Subagents share the parent's model client and tool registry but get their own
  session id, journal, context engine, workspace worktree, and cancellation
  token; writer subagents are serialized per workspace root.
- Queue item ids are stable and monotonic across restore; visible length counts
  only `Queued`/`Promoting`.
- Egress is empty/deny-by-default unless the personal permission file allows a
  host; starting a proxy failure leaves the network off rather than open.
- `EgressRuntime` must outlive its grant: dropping it stops the listener and
  removes the socket.
- `IsolatedTask` polls its future from the blocking pool so synchronous work
  cannot starve the async executor; dropping/aborting signals the worker and
  aborts the handle.
- Every tool call is authorized at one policy point
  (`AgentSession::authorize_tool_call` in `session/tools.rs`), which applies
  approve-all only to `Hitl` decisions and writes an audit row for each
  decision.
- Per-turn bookkeeping (`TurnState`) is reset by each new user message and is
  never persisted; resume reconstructs it empty.
- The compaction vocabulary (`CompactionPolicy`, `Checkpoint`,
  `SessionContextState`, telemetry) is re-exported here so frontends need only
  depend on `forge-core`.

## Side Effects

- Journal/database writes for every durable session event.
- Model-provider network calls, including streaming and compaction calls.
- Tool execution (filesystem, subprocesses, network) through `forge-tools`.
- Spawns background shell jobs and subagent sessions/worktrees.
- Starts egress proxy listeners and socket files; reads merged permission files.
- Queue and background registry mutations; audit-row persistence.
- Emits `TurnEvent`s and tracing warnings.

## Verification

- Run: `cargo test --package forge-core --locked`

Subdirectory contracts:

- [session](./session/EGM.md) — AgentSession methods.
