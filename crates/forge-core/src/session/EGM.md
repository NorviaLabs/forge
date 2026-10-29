# forge-core session — AgentSession Behavior

## Overview

Inherent `AgentSession` methods, grouped by concern: construction/resume/fork,
task lifecycle and queueing, model requests and turn orchestration, tool
execution and evidence, HITL approval and grant application, user questions,
context compaction, and read-only inspection. The `AgentSession` struct itself
stays in `lib.rs` because its fields are private; every module here descends
from the crate root and can therefore reach them.

## Interface

- Construction and egress (`create.rs`):
  - `AgentSession::create`, `resume`, `resume_session`, `fork`,
    `set_governance`, `apply_egress_policy`, `adopt_egress_state`,
    `grant_egress_host`, `allow_suggested_pattern_for_session`,
    `clear_session_pattern_allows`, `session_pattern_allow_count`,
    `session_pattern_allows`, `grant_covers`, `has_network_egress`,
    `egress_tcp_addr`, `drain_egress_events`, `egress_grant`.
- Lifecycle, queue, and background (`tasks.rs`):
  - `yield_current_turn_for_queue`, `queue`, `background`,
    `cancel_background_task`, `pending_hitl`, `enqueue_task`,
    `cancel_queued_at`, `cancel_queued_item`, `promote_next_queued`.
  - Internal `transition`, `enter_waiting`, `transition_to_new_task`.
- Approval and grants (`approval.rs`, `grant.rs`):
  - `resolve_hitl`, `resolve_hitl_with_feedback`, `prepare_approved_hitl`.
  - `decide_hitl`, `prepare_hitl_decision`, `apply_grant_decision`.
  - `GrantOutcome`, `GrantRejection`, `AppliedGrant`, `pattern_grant_eligible`,
    `validate_grant_decision`.
- Model steps and turns (`turn_ops.rs`):
  - `append_user_message`, `append_user_message_with_attachments`,
    `record_composer_line`, `build_model_request`, `apply_model_response`,
    `begin_model_response_application`, `finalize_turn_failure`,
    `mark_cancelled`, `mark_model_call_failed`, `mark_interrupted_if_stale`,
    `run_user_message`, `run_user_message_with_stream`, `prepare_model_step`,
    `prepare_model_step_after_compaction`, `run_agent_turns`,
    `run_agent_turns_in_scope`, `force_context_reset_async`.
- Tool execution (`tools.rs`):
  - `ModelResponseApplication`, `PendingToolApplication`,
    `CompletedToolApplication`, `PendingHitlExecution`,
    `CompletedHitlExecution`, `finish_tool_application`,
    `begin_hitl_execution`, `finish_hitl_execution`.
  - Internal `next_tool_application`, `start_tool_call`, `finish_tool_call`,
    `run_one_tool_exec_only`, `pre_tool_state`, `backfill_tool_outcome`,
    `push_success_evidence`, `push_denied_evidence`, `publish_tool_result`.
- Questions (`question.rs`): `pending_question`, `resolve_question`.
- Compaction (`compaction.rs`):
  - `PendingContextCompaction::execute`, `CompletedContextCompaction`,
    `compact_context`, `begin_context_compaction`,
    `finish_context_compaction`, `begin_auto_context_compaction`,
    `compaction_policy`, `set_context_window`, `compaction_telemetry`,
    `context_state`, `context_tokens`, `context_pressure_reached`,
    `installed_checkpoint_count`, `checkpoint_context_message`,
    `compact_tokens`.
- Inspection (`inspect.rs`):
  - `model_selection`, `set_model_selection`, `journal_dir`,
    `loaded_skills_count`, `loaded_skill_names`, `loaded_skills`, `list_tools`,
    `tool_count`, `context_usage_ratio`, `journal_cursor`,
    `token_usage_report`, `token_usage_lines`, `model_client`,
    `tool_registry`, `background_control`, `workspace_root`,
    `request_turn_cancel`, `begin_turn_cancellation_scope`,
    `set_active_model`, `set_active_profile_id`, `set_image_input_supported`,
    `image_input_supported`, `set_reasoning_effort`, `reasoning_effort`,
    `set_thinking_enabled`, `thinking_enabled`, `set_approve_all`,
    `approve_all`, `set_workspace_trusted`, `workspace_trusted`,
    `set_route_table`.

## Invariants

- Task lifecycle is authoritative only in `ActiveTaskState`; all mutation goes
  through `transition`/`enter_waiting`/`transition_to_new_task`, never a direct
  field write. Terminal states have no outgoing edge; the only escape is a
  fresh task/attempt via `start_fresh_attempt`.
- `resolve_hitl` is valid only while the session is `Waiting` on the matching
  payload; it executes the original unredacted call, not the redacted payload.
- Approve-all auto-approves HITL decisions but never widens shell confinement;
  it refuses to enable unless the workspace is trusted.
- Grant decisions validate the payload shape before applying; redaction markers
  disqualify a pattern grant.
- A turn that has not produced runtime evidence is never marked `Completed`;
  the completion evaluator decides from tool/file/git evidence. The exception
  is `yield_current_turn_for_queue`, which moves straight to `Completed`
  (`QueueYield`) to hand off to the queue.
- Validation failures consume a per-turn budget and are retried only within it.
- Read-only, idempotent, `parallel_safe` calls may run concurrently (max 8);
  everything else runs serially.
- Compaction is transactional: `begin_context_compaction` only builds a request,
  `finish_context_compaction` installs and journals the plan or records a
  failure while leaving the current context untouched. Automatic compaction
  never fails the user's turn.
- Context tokens are memoized for an append-only transcript and invalidated when
  the transcript allocation changes.
- Resuming replays the journal, reconciles incomplete tool intents (re-running
  only idempotent tools), restores queue, background tasks, context projection,
  token usage, and pending wait state.
- `fork` copies the parent's conversation and canonical user turns into a new
  session; it does not mutate the parent.
- `set_approve_all(on)` stores `on && workspace_trusted` and returns whether
  the request was honored (`approve_all == on`), so it returns `false` only when
  enabling on an untrusted workspace.
- When governance is enabled, tool authorization goes through the private
  `authorize_tool_call` policy point, which queues an audit row for every
  decision, written before the call's tool intent (`journal_tool_intent`);
  approve-all upgrades only `Hitl` to `Allow` and never overrides a `Deny`.
  The explicit unconfined retry bypasses it and audits its own decision
  (`explicit_unconfined_retry`). The other, unaudited `governance.authorize`
  calls only narrow: the parallel-read pre-check (the call is still
  authorized and audited when it starts) and the re-checks after a HITL
  approval of the call and of an unconfined retry request (fail-closed on a
  changed policy). With `enable_governance` off, no authorization runs.
- `resume` and in-place `resume_session` both close out the replayed
  session's unfinished background tasks (`reconcile_orphaned_background_tasks`):
  nothing runs them after a restart or a session switch, so they are marked
  cancelled, or interrupted and retained for a subagent, never dropped.

## Side Effects

- Writes session journal records (messages, tool intents/results, lifecycle,
  queue, background tasks, compaction, audit rows).
- Calls the model provider (completion and streaming), including compaction
  calls.
- Executes tools: filesystem writes, subprocesses, network, background jobs.
- Spawns and cancels subagent sessions and background tasks.
- Starts/adopts the egress proxy, installs host grants, and drains egress
  events.
- Strips the route table's provider credential environment variable names
  from child processes (`set_route_table`); it never sets them.
- `apply_grant_decision` appends allow rules to the user's personal
  permissions file for `AllowHostAlways` and `AllowPatternAlways`.
- Emits `TurnEvent`s and audit entries.

## Verification

- Run: `cargo test --package forge-core --locked session`
