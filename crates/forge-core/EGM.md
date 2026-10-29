# forge-core

## Overview

Agent-loop crate: session lifecycle, turn orchestration, tool-call execution,
human-in-the-loop approval, context compaction transactions, queue and
background-task management, subagents, completion evaluation, and egress
wiring. It composes `forge-model`, `forge-tools`, `forge-context`,
`forge-durable`, `forge-governance`, `forge-storage`, `forge-config`, and
`forge-types`. Source lives in `src/`. It deliberately does not depend on
`forge-mcp` or `forge-connect`; session assembly that needs them lives in
`forge-session`.

## Subdirectories & Boundaries

- [src](./src/EGM.md) — agent loop and public session types.

## Interface

- `AgentSession`, `LoopConfig`, `LoopError`, `TurnEvent`, and token-usage
  reports (crate root); its inherent methods are grouped by concern in
  [src/session](./src/session/EGM.md).
- Lifecycle (`ActiveTaskState`), completion evidence, background tasks,
  subagents, `AgentCoordinator`, `TaskQueue`, `EgressRuntime`, `IsolatedTask`,
  and grant/compaction transaction types. The full list is in
  [src/EGM.md](./src/EGM.md).
- Re-exported compaction vocabulary from `forge-context` (`Checkpoint`,
  `CompactionPolicy`, `CompactionError`, `CompactionTrigger`,
  `ProtectedFact`/`ProtectedFactKind`, `SessionContextState`, telemetry types).

## Invariants

- `ActiveTaskState` is the single source of truth for task/wait state; one
  foreground task per session.
- A turn is `Completed` only on runtime evidence, never on model text alone,
  except that a read-only turn completes once the model has replied and a
  queue yield completes the turn without consulting the evaluator.
- With governance enabled, every tool call is authorized at one audited policy
  point before it runs; a few unaudited re-checks may only narrow a decision.
  With `enable_governance` off, no authorization runs.
- Each durable event is journaled before its side effect; resume replays the
  journal and reconciles incomplete intents. Audit rows are queued in memory
  and written before the next tool intent (and at other durable points), so
  a call's authorization row is durable before the tool runs.
- Compaction is transactional and never fails the user's turn.

## Side Effects

- Session journal writes (SQLite through `forge-durable`) and audit rows.
- Model-provider network calls; tool execution (filesystem, subprocesses,
  network); background shell jobs and subagent worktrees.
- Egress proxy listeners and socket files.

## Verification

- Run: `cargo test --package forge-core --locked`
