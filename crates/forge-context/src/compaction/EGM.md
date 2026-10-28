# compaction — Context Compaction Engine

## Overview

Pure, I/O-free compaction of model-visible context. Canonical session history is
never touched here; the module proposes a replacement projection consisting of
the stable system prefix, one structured `<forge_checkpoint>` message, and a
recent raw conversation tail. The transactional install (journal + cache epoch)
lives in `forge-core`'s session module.

## Interface

- Checkpoint (`checkpoint.rs`):
  - `Checkpoint { version, sections }` — `parse(&str)`, `validate()`,
    `section(name)`, `has_user_constraints()`, `render()`.
  - `CheckpointError` — `Empty`, `MissingRoot`, `UnterminatedRoot`,
    `NoSections`, `MissingSection`, `UnsupportedVersion`.
  - `CHECKPOINT_VERSION`, `CHECKPOINT_ROOT`, `CHECKPOINT_SECTIONS`,
    `REQUIRED_SECTIONS` (`objective`, `next_action`).
- Planning (`engine.rs`):
  - `plan_compaction(context, checkpoint, policy, protected_facts)
    -> Result<CompactionPlan, CompactionError>`.
  - `CompactionPlan` — complete new message list plus tail/token metadata.
  - `CompactionError` — `AlreadyRunning`, `NothingToCompact`,
    `Provider(String)`, `Journal(String)`, `Checkpoint(CheckpointError)`,
    `MissingProtectedFacts`, `NotSmaller { old, new }`,
    `StillOversized { new, limit }`, `InvalidStructure(&'static str)`; stable
    `category()` labels for telemetry.
  - `CompactionRecord` (+`succeeded()`), `CompactionTelemetry::record`.
  - `SessionContextState { epoch, checkpoint, tail_start_message_index,
    protected_facts }`, `has_checkpoint`.
  - `is_checkpoint_message(&Message)`.
- Facts (`facts.rs`):
  - `ProtectedFact { kind, text, source_message_index }`,
    `ProtectedFactKind::{UserConstraint, UserCorrection, ExplicitDecision,
    IrreversibleAction}` (+`as_str()`), `protected_fact`, `collect_protected_facts`,
    `recent_facts`, `MAX_SUPPLIED_FACTS`.
- Policy (`policy.rs`):
  - `CompactionPolicy::for_window`, `usable_context`, `trigger_threshold`,
    `should_compact`, `runway_limit`, `tail_target_tokens`, `utilization`,
    `tool_schema_budget`.
  - `CompactionTrigger::{Automatic, Manual}` (+`as_str()`).
  - Constants `TRIGGER_UTILIZATION` (0.85), `POST_COMPACTION_TARGET` (0.40),
    `TAIL_FRACTION`, `TAIL_MIN_TOKENS`, `TAIL_MAX_TOKENS`,
    `DEFAULT_OUTPUT_RESERVE`, `DEFAULT_SAFETY_RESERVE`,
    `DEFAULT_EXPECTED_TURN_TOKENS`, `TOOL_SCHEMA_DEFERRAL_FRACTION`.
- Prompt (`prompt.rs`): `compaction_instruction`, `compaction_message`,
  `checkpoint_message`.
- Tail selection (`tail.rs`): `select_tail`, `is_valid_start`,
  `tool_calls_are_paired`, `tool_results_are_complete`, `message_tokens`,
  `messages_tokens`, `TailSelection { start, tokens, starts_on_user }`.

## Invariants

- `AlreadyRunning`, `Provider`, and `Journal` are never produced by the pure
  planner; they belong to the vocabulary that `forge-core` uses for the
  transactional install.
- Everything in this module is pure: no filesystem, network, journal, or
  provider access.
- `Checkpoint::parse` reads only the `<forge_checkpoint>` element, ignores
  surrounding prose/fences, rejects truncated or empty responses and unsupported
  versions, and validates required sections.
- `Checkpoint::render` is deterministic: canonical section order, only populated
  sections.
- `plan_compaction` checks only that protected facts have a home: a
  non-empty protected-fact set with no `user_constraints` section yields
  `MissingProtectedFacts`. Individual facts are not checked against the
  checkpoint.
- A previous checkpoint in the stable prefix is removed, not stacked; repeated
  compaction merges state.
- Candidates are accepted only when strictly smaller than the input and at or
  below the policy runway limit (or, at worst, below the trigger threshold);
  otherwise `NotSmaller`/`StillOversized`/`NothingToCompact` is returned and no
  context is mutated.
- Tail selection never opens on a tool result and never separates a tool call
  from its results. A valid start is a user message or a tool-call-free
  assistant message; it backs up one extra message only when the start is an
  assistant reply directly following its user request, and may overshoot the
  token target to reach a valid boundary.
- Policy arithmetic: trigger is `min(window * 0.85, usable_context)`;
  compaction is considered when current tokens plus the expected turn reach
  or exceed the trigger; tail target is `clamp(window * 0.12, 16K, 64K)`
  capped at `window / 3`.
- Protected facts longer than 600 characters keep the first 600 plus a `…`
  marker, and the most recent 24 are supplied to the compaction prompt.
- `SessionContextState` records how the projection was derived, never a copy of
  canonical history.

## Side Effects

- None. All functions are in-memory transformations of messages and metadata.

## Verification

- Run: `cargo test --package forge-context --locked compaction`
