# forge-transcript

## Overview

Projects a Forge session into a renderable conversation transcript. Messages
and turn events are reduced to `ChatItem`s, grouped into `ConversationBlock`s,
and shaped into `*Presentation` structs; the crate also provides activity
grouping, verification-command detection, verdict parsing, and text wrapping.
It has no terminal dependency, which is what lets headless callers project a
transcript without linking a UI.

## Subdirectories & System Boundaries

- `src`: Component package containing the entire projection
  (`src/lib.rs`). Contract: [`./src/EGM.md`](./src/EGM.md).
- Reads `forge_core::AgentSession`/`TurnEvent`/`TURN_FAILED_MARKER`,
  `forge_types` message and outcome types, and uses
  `forge_tools::tool_invocation` to label tool calls; writes no data back.

## Interface

- Child component contract: [`./src/EGM.md`](./src/EGM.md) — the public
  symbols, invariants, and side effects implemented by this crate.
- Crate-level façade exports and verification target are documented there.

## Invariants

- The transcript is a pure projection: the same messages, events, lifecycle
  status, and `ConversationViewOpts` produce the same items.
- Presentation is derived, never persisted. Turn identity (`TurnKey`) is
  computed from session identity plus the 0-based user-message ordinal.
- System prompts are not shown; user-visible text is stripped of attached
  context and internal protocol control markers.
- Nothing in this crate draws: colors, glyphs, and layout belong to the
  frontend.

## Side Effects

- None beyond allocation and returning owned values; no I/O, clock, or global
  mutable state.

## Verification

- Run: `cargo test --package forge-transcript --locked`
