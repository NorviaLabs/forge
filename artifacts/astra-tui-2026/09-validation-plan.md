# Forge 2026 TUI validation plan

Validate the later implementation against baseline `dfa46a07aa43551395ba7cb6b4867ab2d8a9c324`. TestBackend coverage is necessary for geometry and semantics; a real interactive PTY is authoritative for focus, streaming, resizing, cursor behavior, terminal emulation, and visual rhythm.

## 1. Reproducible setup

1. Record implementation SHA, Forge version, OS, terminal app, `$TERM`, color capability, font, font size, and cell dimensions.
2. Build with the repository-documented command: `cargo build --release --locked --package forge-cli`; run `./target/release/forge --version`.
3. Use a disposable committed fixture repository for edit/approval/failure tests. Never risk uncommitted user work.
4. Keep one long real repository session for realistic wrapping and history. Record provider/model/effort because response content varies; judge harness behavior, not intelligence.
5. Run both built-in dark and light themes. If font coverage is uncertain, repeat glyph checks in Menlo or Monaco, SF Mono, Consolas, and JetBrains Mono where available.

## 2. Required real-PTY scenario suite

Capture each named checkpoint as text/buffer evidence and a terminal screenshot. Do not crop away header, composer, footer, or pane boundaries.

| Scenario | Action and checkpoints | Required evidence |
|---|---|---|
| Fresh start | launch cleanly; move focus across visible panes | obvious composer first target; exactly one focus owner; calm empty states |
| Simple task | “Summarize this repository.” | request appears once; live row mutates; final answer occupies final location; neutral completion |
| Streaming task | use a response long enough to observe T+0, 250ms, 1s, first reasoning/tool, final stream, completion | no empty assistant block, duplicate busy indicator, whole-history redraw artifact, or stale active marker |
| Tool-heavy task | architectural/performance investigation with reads/searches/shell | exploration groups at semantic boundaries; exact calls recover through Ctrl+O; active call remains visible |
| Three-minute command | start one harmless long command that uses session polling | one compact shell lifecycle; elapsed remains truthful; poll events and nonempty stdin remain expanded |
| Plan | multi-step safe task with plan updates | heading count correct; one dominant active step; old updates available; no invented failed/skipped state |
| Real edit | inspect, edit two fixture files, verify, open Review Changes | exact changed files and diff; answer does not invent verification; dirty/review state coherent |
| Approval | trigger safe outside-workspace or policy-gated action | command/action, reason, cwd, supplied risk and all existing choices readable; accept and deny paths return focus correctly |
| Failure/recovery | run `sh -c 'exit 1'`, react, then `printf 'recovered\n'` | failed outcome remains compact-visible; later success is separate; no retroactive success |
| Cancellation | cancel active harmless task, then submit follow-up | animation ends; turn says cancelled; process termination is not claimed without evidence; input remains usable |
| Long conversation | at least five substantial turns including failure and edit | user can find requests, answers, changes, failures, successes, and uncertainty without expanding routine activity |
| Editor | open file; exercise NORMAL, INSERT, dirty, unsaved, external conflict | mode, title, `*`, cursor and focus remain accurate; dialog choices/actions unchanged |
| Terminal | open/focus terminal; type a command; switch to editor/chat; resize | terminal receives keys only when focused; ANSI/prompt legible; lifecycle preserved; composer never vanishes |
| Files | navigate nested tree; filter matching and no-result query | one-row search, correct indentation/selection/matches, distinct empty/no-result states |
| Model selector | filter, move selection, inspect duplicate routes, cancel and choose | model/provider/source/account/current state retained; columns and detail readable |
| Theme selector | preview dark/light, cancel, select | preview and restoration unchanged; selector remains bottom dock; both palettes readable |
| Review Changes | inspect multi-file real diff at three widths | file identity, focus, +/- counts, syntax and conversation relationship remain clear |

## 3. Size and screenshot matrix

Minimum release set:

- **80×24:** fresh, active tool, approval, editor, terminal, Files hidden behavior, model picker, conflict dialog.
- **100×30:** multiline composer, plan, long command, completed turn.
- **120×35:** full workspace, active agent, tool group, error/recovery, Review Changes, model picker.
- **160×45:** Files search/tree, editor+chat, terminal, theme picker, long conversation.
- **Very wide (220×55 or current desktop):** long investigation, max-width answer, code/diff/terminal width use.

Also run the enforced 80×18 minimum for every overlay family and one active turn. At each frame, assert calculated pane widths from the design-system table when file/diff is open. Resize a single live session through 160→80→120→220 columns and back; verify focus and scroll anchor, not merely static launch frames.

Store screenshots under an implementation validation directory with stable names such as `120x35-tool-group-dark.png`. Pair screenshots for before/after review, but do not bless terminal-app chrome as part of Forge. Keep a small curated set; buffer fixtures cover the combinatorial matrix.

## 4. Invariant checks

These are release blockers:

- Effective keyboard focus is always visually identifiable by one structural marker; modal focus suppresses background markers.
- Active work is visually dominant and orange; routine completed activity recedes; final response dominates a completed turn.
- Color is never the only carrier of status. Mandatory glyphs occupy predictable cells.
- No stale active state, spinner, “running,” approval wait, or live elapsed counter remains after completion/cancellation.
- Failed, denied, cancelled, uncertain, truncated, and unknown outcomes are not converted to success by turn completion or a later command.
- A shell session and its polling render once compactly; expanded detail preserves every available event and nonempty input.
- Pane bottoms align. The composer/footer geometry is consistent, and the composer remains visible at 80×24 with terminal open.
- The Files gate remains 116 frame columns; focus does not resize panes; transcript prose stops at 88 columns while code/diff/terminal can use space.
- Every baseline field, choice, output, answer, plan update, and currently exposed narration remains accessible under the existing disclosure/visibility controls.
- No new command, binding, tool, workflow, capability, persisted schema, approval policy, or model behavior appears.
- Displayed key hints exactly match active input routing in every focus and overlay state.
- Light theme preserves readable contrast and the same semantic grammar; most content stays neutral in both themes.

## 5. Automated coverage

Add tests at the ownership layer, avoiding snapshots that merely mirror implementation:

1. **Theme/config:** optional `activity` fallback; built-in exact tokens; custom theme backward compatibility; hue/contrast invariants.
2. **Layout:** exact rectangles for five widths, 80×18 height pressure, Files gate, terminal allocation, absent-editor case, no underflow.
3. **Projection:** two completed turns plus one active; summary ownership; resume without ephemeral timing; plan supersession; Ctrl+O detail completeness.
4. **Tool lifecycle:** queued/running/exit 0/exit 1/denied/cancelled/unknown; same-session poll coalescing; interleaved and missing-session noncoalescing; nonempty stdin retention.
5. **Scroll/cache:** append while following; append while reading history; resize; expand/collapse; theme change; active timer tick without settled Markdown reprojection.
6. **Input/focus:** pane traversal, modal suppression/restoration, approval/question actions, multiline composer, terminal ownership.
7. **Critical render assertions:** semantic region bounds, unique focus marker, exact essential labels/options, absence of stale active markers, stable one-cell separators. Use selective goldens for representative full screens only.

Run focused tests throughout. Before handoff run:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked --no-fail-fast
cargo build --release --locked --package forge-cli
./target/release/forge --version
```

Compare render performance on a long transcript before/after. Timer-only frames must not rebuild or re-highlight settled history. Treat performance regressions, flicker, cursor trails, and excess redraw observed in a real PTY as failures even if buffer tests pass.

## 6. Review protocol

For each implementation slice, attach the relevant prototype state, current-state capture, new real-PTY capture, sizes/themes exercised, automated checks, and known metadata limitations. A reviewer should be able to trace each visible change to one DESIGN item and one existing behavior. If implementation requires data Forge does not currently expose, render unknown/omit the optional field and record the gap; do not widen product scope.

Final sign-off requires one uninterrupted 30-minute dogfood session containing a long task, history review, cancellation, editor/terminal switching, model/theme selectors, and Review Changes. Five minutes after completion, inspect old turns again: their answers must remain easy to scan, exact detail must still expand, and no timed styling may have changed.
