# forge-types/src

## Overview

Canonical shared types for Forge. `lib.rs` holds the message/tool/model/journal
model plus governance and plan/question payloads; `image.rs` holds
path-reference image metadata and header-only inspection; `git.rs` holds the
read-only git subcommand list shared across crates.

## Interface

- `strip_protocol_markers(&str) -> String`: removes known control envelopes
  (e.g. `\confidence{…}`) and trims.
- `title_from_prompt(&str) -> String`, `session_name_from_prompt(&str) -> String`,
  `TITLE_MAX_CHARS` (60).
- `SessionId = Uuid`.
- `TaskLifecycle`: `Ready`, `Working`, `Waiting`, `Completed`, `Failed`,
  `Cancelled`, `Interrupted` (non-exhaustive; serde aliases `running` and
  `awaiting_hitl`).
- `SideEffectClass`: `Read`, `Write`, `Network`, `Exec`, `Meta`.
- `MessageRole`: `System`, `User`, `Assistant`, `Tool`.
- `Message { role, content, tool_call_id, name, thinking,
  thinking_duration_secs, tool_calls, outcome, attachments }` with
  `new(role, content)`, `with_attachments`, `from_tool_output(&ToolCall,
  &ToolOutput)`.
- `ToolCall { id, name, arguments }`; `ExecutionOutcome` (success/failure
  variants) with `is_success()`.
- `ToolOutput { content, is_error, exit_code, outcome, attachments }` with
  constructors `success`, `failed_exit`, `spawn_failed`, `denied`, `cancelled`
  and `effective_outcome()`.
- `ToolValidationError { tool, path, message, schema_hint }` (`Display`,
  `Error`).
- `Usage { prompt_tokens, completion_tokens, prompt_cache_read_tokens,
  prompt_cache_write_tokens }` and `ModelStreamEvent`, `ModelResponse { text,
  tool_calls, usage, thinking }`.
- `JournalEventType` (session/user/model/tool/state/hitl/context/queue/
  background/subagent/composer tags) and `JournalEvent { seq, session_id, ts,
  event_type, schema_version, payload, trace_id }`.
- `ToolDescriptor { name, description, input_schema, side_effect_class,
  idempotent }`; `MCP_TOOL_NAME_PREFIX = "mcp:"`;
  `SEARCH_TOOLS_TOOL_NAME = "search_tools"`.
- `ProgressDocument` (context-handoff checkpoint) with `new(session_id, goal)`.
- Plan tool: `PlanStepStatus` (`as_str()`), `PlanItem`, `UpdatePlanArgs` (JSON-schema
  derived).
- Question tool: `AskUserQuestionOption`, `AskUserQuestionItem`,
  `AskUserQuestionArgs` (limits `MIN/MAX_QUESTIONS` 1–4, `MIN/MAX_OPTIONS`
  2–4, `MAX_HEADER_CHARS` 30, `normalize()` validation), `AskUserQuestionAnswerItem`,
  `AskUserQuestionResult`, `QuestionPayload`.
- Governance: `PolicyDecision`, `Principal` with `local_dev()`/`restricted()`,
  `HitlDecision`, `HitlPayload { …, kind() }`, `ApprovalKind`, `GrantDecision`
  (`is_approval`, `as_str`).
- IDs and lifecycle: `TaskId`, `AttemptId`, `WaitReason` (`Approval`,
  `Question`, `Clarification`, `Selection`, `MissingConfiguration`,
  `ExternalAction`; non-exhaustive) with `request_id()`, `QueueItemId`,
  `QueueItemStatus`, `BackgroundTaskId`.
- Images: `MAX_IMAGE_BYTES` (5 MiB), `ImageRef { path, mime, byte_len, width,
  height, detail }` with `new`/`with_dimensions`, `ImageMeta`,
  `ImageInspectError`, `sample_png_bytes()`, `inspect_image(&[u8])`,
  `sniff_allowed_image(&[u8]) -> bool`.
- `is_readonly_git_subcommand(&str) -> bool`.

## Invariants

- `strip_protocol_markers` removes only structural envelopes, never prose; an
  unterminated marker rewinds so the tail is emitted exactly once and no text
  is duplicated or dropped.
- `title_from_prompt` draws at most `TITLE_MAX_WORDS` (4) words from the first
  sentence, skipping filler words so they do not count toward that budget
  (all-filler input falls back to the literal opening). The survivors are
  kebab-cased to at most `TITLE_MAX_TOKENS` (4) normalized tokens, and the
  result never exceeds `TITLE_MAX_CHARS` characters.
- `TaskLifecycle` is authoritative for UI busy/waiting state; consumers must
  not derive their own lifecycle from streaming flags.
- `Message.outcome`/`ToolOutput.outcome` are the recorded result; an absent
  outcome is derived from `is_error`/`exit_code` by `effective_outcome`.
- `JournalEventType` tags are wire contracts: `SessionStatus` stays
  `"session_status"` for existing journals.
- `AskUserQuestionArgs::normalize` enforces 1–4 questions and 0 or 2–4 options
  each, requires non-empty question text and option labels, fills missing ids
  as `q1…qN`, derives/truncates missing headers to `MAX_HEADER_CHARS`, rejects
  duplicate ids, and returns a human-readable error otherwise.
- `ProgressDocument.version` is the checkpoint schema version and must be bumped
  when the payload shape changes incompatibly.
- `ImageRef.path` is workspace-relative and, after insertion, points at the
  session-cache snapshot rather than the original file; bytes are never stored
  on the type.
- `inspect_image` is header-only (no decode), recognizes PNG/JPEG/GIF/WebP,
  and rejects empty input, inputs above `MAX_IMAGE_BYTES`, and inputs whose
  signature matches none of the four formats. A PNG signature with a short or
  non-`IHDR` header, or a JPEG `FF D8 FF` prefix with no SOF segment, is
  accepted with `width`/`height` of `None`; `sniff_allowed_image` is the
  cheap prefix check.
- `is_readonly_git_subcommand` matches on the subcommand alone: `status`,
  `diff`, `log`, `show`, `branch`, `rev-parse`, `ls-files`, `blame` (exact,
  case-sensitive match; callers lowercase first); everything
  else is treated as mutating, and callers needing argument-level precision
  (`git branch -d`) must add their own checks.

## Side Effects

- All functions are pure except `ProgressDocument::new`, which reads the wall
  clock (`Utc::now()`) for `updated_at`; image inspection reads only the
  supplied byte slice.

## Verification

- Run: `cargo test --package forge-types --locked`
