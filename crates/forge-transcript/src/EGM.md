# forge-transcript/src

## Overview

Single-module projection layer. `lib.rs` turns session history into chat items,
semantic blocks, and presentation structs, and provides the formatting helpers
the UI renders with. Everything is computed from owned inputs; the module never
mutates the session.

## Interface

- `STREAM_CARET: char` (`▎`): caret appended to a still-arriving answer.
- `ChatItem`: tagged enum of renderable rows (`Brand`, `Home`, `System`,
  `User`, `Thinking`, `Assistant`, `StreamingAssistant`, `ToolCard`,
  `ActivityGroup`, `VerificationCard`, `DiffCard`, `ApprovalPending`,
  `QuestionPending`, `AgentRoster`, `AgentReceipt`, `PlanChecklist`, `Banner`,
  `TurnSummary`, …).
- `ConversationBlock`: semantic grouping of items (`UserMessage`,
  `AssistantAnswer`, `ActiveProgress`, `ActivityGroup`, `Callout`, `CodeBlock`,
  `DiffBlock`, `VerificationBlock`, `ApprovalPending`, `Home`,
  `QuestionPending`, `PlanChecklist`, `Metadata`, `Thinking`, `TurnSummary`,
  `AgentRoster`, `AgentReceipt`).
- Presentation structs: `TurnSummaryPresentation`, `ThinkingPresentation`,
  `UserMessagePresentation`, `AssistantAnswerPresentation`,
  `ActiveProgressPresentation`, `ActivityGroupPresentation`,
  `CalloutPresentation`, `CodeBlockPresentation`, `DiffBlockPresentation`,
  `VerificationBlockPresentation`, `ApprovalPendingPresentation`,
  `QuestionPendingPresentation`, `PlanChecklistPresentation`,
  `AgentRosterPresentation` (`expanded()`), `AgentReceiptPresentation`,
  `HomePresentation`, `MetadataPresentation`;
  `TurnSummaryPresentation::tokens_per_second()`.
- Row/view helpers: `ApprovalRequestView`, `ApprovalMenuRow`, `AgentRosterRow`,
  `QuestionMenuRow`.
- Enums: `ToolCardState`, `ActivityCategory` (`icon()`), `ActivityOutcome`, `BannerKind`,
  `ActiveProgressStatus`, `AgentRowState`, `AgentReceiptKind`, `StreamWaitPhase`,
  `Verdict`.
- `ConversationViewOpts { busy, tool_expanded, compact, stream_wait,
  stream_thought_secs, pulse_dim }` (all defaulted).
- `TurnKey { session, user_ordinal }`.
- `ConversationModel { items, scroll, follow, opts, turn_summaries,
  turn_summary_base }` with:
  - `from_messages(&[Message], &[TurnEvent], TaskLifecycle, ConversationViewOpts)`
  - `from_session(&AgentSession, ConversationViewOpts)`
  - `semantic_blocks()`, `turn_boundaries()`, `is_in_latest_turn(usize)`
  - builders: `with_turn_summaries`, `with_streaming_preview`,
    `with_streaming_assistant`, `with_brand`, `with_home`,
    `with_activity_summary`, `with_running_tool`, `with_pending_approval`,
    `with_pending_child_approval`, `with_agent_roster`, `with_agent_receipts`,
    `with_pending_question`, `with_extra_banners`
  - `scroll_up(n)`, `scroll_down(n)`, `last_tool_index()`.
- Free functions:
  - `tool_kind_label(&str) -> &str`, `SHELL_KIND_LABEL`,
  - `is_verification_command(&str) -> bool`,
  - `parse_verdict(&str, &ExecutionOutcome) -> (Verdict, Vec<String>)`,
  - `format_elapsed_compact(f64) -> String` (`12s`, `1m12s`, `1h02m`),
  - `format_elapsed_tenths(f64) -> String` (0.1s under 5s, then whole seconds),
  - `sanitize_final_answer_text(&str) -> String`,
  - `group_routine_activity(Vec<ChatItem>) -> Vec<ChatItem>`,
  - `wrap(&str, usize) -> Vec<String>`.

## Invariants

- `from_messages` starts from messages only: system prompts are skipped and
  tool cards are reconstructed by matching `ToolCall.id`; evaluation/repair
  turns produce evaluator or repair items rather than plain assistant text.
- `TurnKey` is never persisted; resume rebuilds keys from session identity plus
  the user-message ordinal, so restored turns simply carry no ephemeral timing.
- `turn_boundaries` returns indices of `ChatItem::User`; `is_in_latest_turn`
  returns true when no turn has started (only brand/home items).
- `group_routine_activity` collapses only consecutive routine items of the same
  category; every non-routine item passes through unchanged and order is
  preserved.
- `sanitize_final_answer_text` removes only known structural control
  envelopes (e.g. `\confidence{…}`), never filters phrases; an unclosed marker
  is left in place, so no surrounding text is dropped.
- `wrap` measures terminal cell width (`unicode-width`), breaks at whitespace,
  splits words wider than the whole line rather than overflowing, clamps width
  to at least 1, and returns `[""]` for empty input.
- `format_elapsed_*` clamp negative input to zero and never emit fractional
  seconds where the format promises whole units.
- `Verdict::glyph`/`headline` and `AgentRowState::word`/`is_terminal` are total
  functions over their enums; terminal states are the completed/failed family.
- `parse_verdict` returns evidence lines that exclude blank lines and internal
  `fff:` debug lines.
- Presentation structs are plain data (no hidden references to the session),
  so callers may retain or re-render them freely.

## Side Effects

- None. No filesystem, network, subprocess, clock, randomness, or global state;
  all functions are deterministic over their arguments (elapsed-time strings
  take the elapsed value as input).

## Verification

- Run: `cargo test --package forge-transcript --locked`
