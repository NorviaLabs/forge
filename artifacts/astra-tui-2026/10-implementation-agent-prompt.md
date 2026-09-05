# Forge 2026 TUI redesign — implementation agent prompt

You are the implementation agent for Forge’s 2026 TUI redesign.

Forge is an open-source, keyboard-first terminal coding-agent environment written in Rust and Ratatui. Implement the approved redesign from the design package at:

```text
artifacts/astra-tui-2026/
```

The design baseline is:

```text
dfa46a07aa43551395ba7cb6b4867ab2d8a9c324
forge 0.1.0-beta.9
```

The baseline may be behind current `main` when you begin. Start by running `git status`, inspect remotes, fetch safely, and use the latest available `main` HEAD. Preserve existing user work. If the checkout cannot be updated safely, create an isolated worktree and feature branch. Never rewrite history or discard local changes. Record the actual implementation baseline and reconcile any source changes since the design baseline before editing.

## Objective

Implement the design so Forge feels like a coherent terminal-native engineering environment rather than a set of independent Ratatui panels. The highest-priority result is the agent experience: active work must be legible and stable, tool activity must remain truthful, completed turns must be easy to scan, and exact existing detail must remain accessible.

This is an implementation task. Continue through code changes, focused tests, broader validation, a release build, and real-PTY visual verification. Do not stop after producing a plan.

## Hard product boundary

Do not introduce new functionality.

Do not add tools, commands, keybindings, workflows, model behavior, agent capabilities, Git operations, LSP features, sandbox behavior, collaboration features, approval policies, persisted schemas, or product concepts.

You may change presentation, view projection, layout, grouping, spacing, semantic styling, focus treatment, responsive behavior, and reversible disclosure of information Forge already has. Preserve all existing information and interaction semantics. When required metadata is absent, omit it or display an honest unknown state. Never infer success, verification, process termination, risk, or recovery without structured evidence.

## Source-of-truth documents

Read these completely before editing:

1. `AGENTS.md`
2. `FORGE-DESIGN.md`
3. `artifacts/astra-tui-2026/03-forge-design-system.md`
4. `artifacts/astra-tui-2026/04-agent-experience.md`
5. `artifacts/astra-tui-2026/05-component-specs.md`
6. `artifacts/astra-tui-2026/07-implementation-plan.md`
7. `artifacts/astra-tui-2026/08-implementation-manifest.md`
8. `artifacts/astra-tui-2026/09-validation-plan.md`
9. `artifacts/astra-tui-2026/prototype/README.md`

Use `01-current-state-audit.md`, `02-reference-cli-study.md`, `06-before-after.md`, the current-state captures, and the HTML prototype as supporting evidence.

`FORGE-DESIGN.md` is the current runtime contract. The 2026 proposal explicitly supersedes its full pane boxes, violet narration, green-tinted neutrals, and 95% frame inset. Preserve its focus, input-ownership, displayed-binding, outcome, and accessibility invariants. Update `FORGE-DESIGN.md` only after the implementation matches the new design and validation passes.

The HTML prototype is a terminal-cell rendering reference. Do not reproduce its browser controls in Forge. Do not translate HTML implementation details into runtime concepts.

## Required implementation sequence

Work through the 18 items in `08-implementation-manifest.md` in dependency order. Use the exact behavior, acceptance criteria, regression risks, dependencies, complexity, and merge guidance in `07-implementation-plan.md`.

Implement in this order:

1. **DESIGN-001–004:** semantic tokens, state markers, deterministic layout, pane chrome, and effective focus.
2. **DESIGN-005–007:** stable per-turn projection, the live status row, final-answer streaming, historical compaction, and scroll anchoring.
3. **DESIGN-008–011:** truthful tool outcomes, shell-session coalescing, grouped activity, plans, final answers, and completion metadata.
4. **DESIGN-012–013:** composer, footer, status ownership, approvals, and agent questions.
5. **DESIGN-014–015:** shared overlay family and model-picker layout.
6. **DESIGN-016–017:** Files/search/tree, editor, terminal, Review Changes, and conflict dialogs.
7. **DESIGN-018:** responsive, accessibility, performance, consistency, documentation, and release validation.

Do not begin with a broad cosmetic rewrite. Establish the semantic and geometry foundations first. Treat DESIGN-005 as the main architectural checkpoint: request, activity, answer, and available completion metadata must belong to the correct turn before historical compaction or tool grouping is layered on top.

## Implementation constraints

- Keep `forge_config::ThemePalette` as the theme boundary and current theme accessors as renderer entry points. Add `activity` as an optional backward-compatible theme field with the specified fallback.
- Use the exact dark/light semantic values, spacing constants, glyph vocabulary, pane minimums, width table, and height formulas from `03-forge-design-system.md`.
- Preserve the effective 116-column Files visibility threshold when removing the 95% inset.
- Use the same computed rectangles for painting, wrapping, scrolling, hit testing, cursor placement, and PTY resize.
- Preserve the global Ctrl+O disclosure behavior. Do not add per-turn accordion controls.
- Keep stored messages and journal semantics unchanged. The redesigned transcript is a reversible view projection.
- Because `Message` has no durable display ID, follow the specified session-plus-user-ordinal presentation key. Do not create a persistence migration for this design.
- Associate `exec_command` and `write_stdin` only through existing matching session IDs. Preserve nonempty stdin writes and all available raw events in expanded detail.
- Keep failure, denial, cancellation, truncation, redaction, and unknown outcomes truthful after later recovery or normal turn completion.
- Plans consume only current schema states: pending, in_progress, completed. Do not add failed or skipped states.
- Preserve existing reasoning visibility and privacy behavior. Do not generate summaries, classify prose as findings, reveal hidden reasoning, or change model prompts.
- Preserve every existing approval choice and its exact scope. Never copy a competitor permission option into Forge.
- Preserve terminal ANSI output, editor behavior, Files behavior, Review Changes behavior, selectors, queues, tasks, notifications, and all active input mappings.
- Every displayed key hint must correspond to a binding reachable in the current state.
- Avoid full-pane boxes, saturated backgrounds, decorative icons, hover concepts, animation-heavy rendering, and web-dashboard patterns.

## Testing strategy

For each design slice:

1. Inspect the current owning modules before changing them.
2. Add focused tests for behavior and presentation invariants that can regress.
3. Run the smallest relevant crate tests.
4. Exercise the changed state in the release binary through a real PTY.
5. Mark the corresponding manifest item complete only when its code, behavior, and visual criteria pass.

Tests should establish semantic ownership and geometry rather than mirror every implementation detail. Use selective full-screen goldens, plus direct assertions for calculated rectangles, critical labels, option availability, focus ownership, lifecycle state, information retention, and absence of stale motion.

Pay special attention to:

- multiple completed turns followed by a new active turn;
- shell command polling for at least three minutes;
- exit 1 followed by a separate successful recovery command;
- cancellation with unconfirmed child-process state;
- Ctrl+O detail completeness;
- append while following the tail versus append while reading history;
- resize while active and while reading history;
- modal focus suppression and restoration;
- composer visibility with terminal open at 80×24;
- model routes with duplicate model IDs but different provider/account identities;
- dark and light custom themes without an `activity` token.

## Real-PTY visual validation

The real TUI is authoritative. Test at minimum:

```text
80x18
80x24
100x30
120x35
160x45
220x55 or the current wide desktop terminal
```

Run every scenario in `09-validation-plan.md`, including fresh start, simple conversation, streaming, tool-heavy work, a long polled command, plan updates, a safe real edit, approval and denial, failure/recovery, cancellation, long history, editor modes, terminal focus, Files search/no-results, model selector, theme selector, Review Changes, unsaved changes, and external conflict.

Capture a small curated before/after set. Do not declare a visual slice complete from TestBackend output alone. Verify actual focus, cursor ownership, wrapping, ANSI terminal content, resizing, pane bottoms, modal reachability, and scrolling in a real PTY.

## Release invariants

The implementation is incomplete if any of these fail:

- Exactly one effective keyboard owner is visually identifiable.
- Active work is visually dominant; completed routine activity recedes.
- The final answer dominates a completed turn.
- No stale running, spinner, waiting, or elapsed state remains after completion or cancellation.
- Errors and unknown outcomes are never displayed as success.
- Polling produces one compact shell lifecycle while expanded detail retains all available events.
- Pane bottoms align and the composer remains visible at 80×24 with terminal open.
- Historical expansion loses no currently available nonempty content.
- No existing information, command, choice, binding, or capability disappears.
- No new functionality appears.
- Both built-in themes and old custom themes remain valid.
- Long-transcript/timer rendering does not unnecessarily rebuild settled Markdown or introduce visible flicker.

## Required final checks

Run focused checks throughout, then before handoff run:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked --no-fail-fast
cargo build --release --locked --package forge-cli
./target/release/forge --version
```

Also complete the uninterrupted 30-minute dogfood session specified in `09-validation-plan.md` and inspect completed turns again five minutes later.

## Handoff report

At completion, report:

- actual baseline and final commit SHA;
- completed DESIGN items;
- files/modules changed by slice;
- focused and workspace validation results;
- real-PTY scenarios, sizes, terminal/font, and themes tested;
- screenshot/capture locations;
- performance comparison for a long transcript and timer-only frames;
- any metadata the UI honestly omitted because Forge does not expose it;
- remaining risks or incomplete items.

Do not claim the redesign is complete while any manifest item, required invariant, real-PTY scenario, or final check remains unresolved. Do not merge to `main` unless the user explicitly asks you to complete that external action.
