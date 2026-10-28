# forge-durable/src

## Overview

Journal, replay, and durable audit implementation. `lib.rs` owns the SQLite
connection, event appends for every domain event, replay (with periodic
checkpoints), and restored-state types. `audit.rs` adds the append-only audit
table that is a record only — replay never reads it.

## Interface

- `JournalError`: `Sql`, `Json`, `Io`, `Other(String)`.
- `Journal`: `directory()`, `open(dir, session_id)`, `last_seq()`, `append`,
  `replay(session_id)`, `Journal::cached_tool_result(&ReplayState, call_id)`
  (associated function), and typed appenders:
  `append_session_created`, `append_user_message`,
  `append_user_message_with_attachments`, `append_composer_line`,
  `append_tool_intent`, `append_tool_intents`, `append_tool_results`,
  `append_tool_result`, `append_validation_failed`,
  `append_model_request`, `append_model_response`, `append_status`,
  `append_hitl_wait`, `append_hitl_resume`, `append_context_reset`,
  `append_context_compacted`, `append_queue_enqueued`,
  `append_queue_promoting`, `append_queue_promoted`, `append_queue_removed`,
  `append_background_task_started`(+`_with_parent`),
  `append_background_task_finished`, `append_subagent_spawned`,
  `append_subagent_finished`.
- `latest_session_id(dir: &Path) -> Result<Option<SessionId>, JournalError>`.
- `ToolResultPayload { call_id, name, output }`.
- `ReplayState` fields: `session_id`, `status`, `last_seq`, `tool_results`,
  `incomplete_intents`, `user_messages`, `composer_lines`, `messages`,
  `model_responses`, `events`, `pending_hitl`, `queue_items`,
  `background_tasks`, `subagent_workspaces`, `last_prompt_hash`,
  `last_prompt_bytes`, `cache_epoch`, `last_cache_transport`, `context_state`,
  `compaction_count`, plus constructors/helpers on the type.
- Private (not exported): `ReplayCheckpoint`, the serialized checkpoint
  payload with `last_seq`, and `REPLAY_CHECKPOINT_INTERVAL` (256).
- `RestoredQueueItem { id, text, created_at, status }`,
  `RestoredBackgroundTask { id, kind, label, child_session_id,
  parent_session_id, status, summary, finished }`.
- `new_session_id() -> SessionId`.
- Audit: `AuditKind` (`Authorize`, `HitlRequest`, `HitlResolution`,
  `HostGrant`, `PatternGrant`, `ApproveAll`, `ToolResult`, `Egress`,
  `Unknown`), `AuditEntry`, `AuditRow`, `Journal::append_audit`,
  `Journal::audit_events(session_id)`.

## Invariants

- `append` inserts a row with `schema_version = 1`, returns
  `last_insert_rowid` as the assigned `seq`, and serializes payloads as JSON.
- Each append is a single durable write under WAL with `synchronous=NORMAL`;
  recent commits may be lost on power failure but the database must not be
  corrupted, because replay re-derives state from events rather than trusting
  the file blindly.
- The pool uses exactly one connection, so appends are serialized.
- Every 256th `seq` requests a replay checkpoint; the request is best-effort
  (`try_send` on a depth-1 channel) and its failure is ignored.
- Checkpoints are accepted only when their stored `seq` matches
  `ReplayCheckpoint.last_seq`; otherwise replay starts from an empty state and
  all events are read.
- Replay reads events for one `session_id` in ascending `seq`, starting after
  the checkpoint; it tracks open tool intents and clears them on matching
  results, and does not re-queue a queue item whose promotion already
  materialized a user message.
- `latest_session_id` considers only files with a `.db` extension whose stem
  parses as a UUID, returns the most recently modified, ignores invalid files,
  and returns `Ok(None)` when the directory does not exist.
- `open` creates the directory, enables `create_if_missing`, and runs
  idempotent `CREATE TABLE IF NOT EXISTS` migrations for events, checkpoints,
  and audit rows.
- Audit rows are append-only and correlated by tool call id; resume never reads
  them, so journals predating the audit table resume unchanged with an empty
  audit table.
- `AuditKind` uses `snake_case` wire values and unknown future kinds decode as
  `Unknown`.

## Side Effects

- Creates the journal directory and `<session_id>.db` (plus SQLite WAL/SHM
  files); writes/deletes rows during checkpoint maintenance.
- Spawns a named `forge-checkpoint` thread with its own current-thread Tokio
  runtime and its own single-connection pool on first checkpoint need.
- Reads directory metadata via `read_dir`/`metadata`.
- No network access; no process spawning.

## Verification

- Run: `cargo test --package forge-durable --locked`
