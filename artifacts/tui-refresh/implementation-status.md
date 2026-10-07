# Forge UI implementation

The complete scope is the [updated mockup proposal](https://github.com/NorviaLabs/forge/pull/839) and its five implementation phases. Production work started from main `cf61193ff9f009c2a61ce55672fd24557a8337ee` in an isolated worktree.

1. **Layout and start — merged in [PR #840](https://github.com/NorviaLabs/forge/pull/840); required CI passes.** Centered task entry, shared gutters, bounded composer, retained navigation and drafts, quieter chrome, updated built-in palettes.
2. **Conversation and inspection — merged in [PR #842](https://github.com/NorviaLabs/forge/pull/842); required CI passes.** Tighter transcript and inspector origins, approvals with safe defaults and scrollable details, compact pickers, retained shell input and explicit failure recovery. [Review the compiled captures](phase2-captures.html).
3. **Queue and dock — merged in [PR #843](https://github.com/NorviaLabs/forge/pull/843); all three CI checks pass.** Stable task and prompt identities, cancellation/edit race guards, bounded overflow and a live filtered task view. [Review the compiled captures](phase3-captures.html).
4. **Jobs — merged in [PR #844](https://github.com/NorviaLabs/forge/pull/844); Subagents — merged in [PR #845](https://github.com/NorviaLabs/forge/pull/845). All three CI checks pass on both.** Children expose actual execution metadata, full named decisions, retained reading state and explicit partial-result handoff. [Review the child captures](phase4-subagents-captures.html).
5. **Motion and complete workflows — implemented and validated on `feat/tui-refresh-motion`.** Clocked indicators, reduced motion, immediate available chunks, correct Unicode caret/wrapping and preserved handoff paragraphs. [Review the final captures](phase5-captures.html), [native motion replay](phase5-motion.html), and [required CI checks](https://github.com/NorviaLabs/forge/actions?query=branch%3Afeat%2Ftui-refresh-motion).

Phases 1–4 were subsequently merged into main `c800a445`. That tree exactly matches the previously validated Subagents head `d74061ba`; the final phase targets main directly. The phase-by-phase evidence below records the original handoffs.

## Phase 1 comparison

[Open the compiled before/after gallery](phase1-captures.html). It contains 60 frames per build: start, long draft, working, changed-file review and approval at 80×18, 80×24, 120×40 and 160×50, in dark, light and monochrome. These are the real Ratatui draw path with mock session fixtures, not live agent results. Review fixtures wait for actual Git status and patch completion. The main baseline changes only the opt-in capture harness and test-helper visibility.

The task group joins its heading, input, starting points and hints. Its 84-column cap limits scanning distance, while narrow terminals retain one complete starter. Explicitly requested navigation remains reachable; when columns fit, the task group stays beside it. The composer has one top rule, one to three input rows at compact heights and two to four at comfortable heights; longer drafts scroll without being discarded. Navigator width starts at 24 columns and preserves saved preferences within content floors. Simultaneous navigator/conversation/review requires 132 frame columns with the current pane floors, rather than copying the mockup's 136-column gate.

The header shares the text origin with its workspace identity. The footer reserves notes width before fitting the model, hides empty saved notes and unreported usage, and spells out disconnected state. All palettes remain runtime theme tokens; drop-in overrides, system selection and monochrome retain their existing paths. Custom-theme tests derive fixture edits from current tokens instead of relying on retired colors.

## Reproduce captures

```sh
FORGE_RENDER_DUMP_DIR=/tmp/forge-ui-refresh cargo test --package forge-tui --locked --lib capture_refresh_frames -- --ignored --nocapture --test-threads=1
node artifacts/tui-refresh/render-captures.mjs /tmp/forge-ui-refresh /tmp/forge-ui-refresh.html
node artifacts/tui-refresh/check-captures.mjs /tmp/forge-ui-refresh.html /tmp/forge-ui-refresh-screens
```

For a comparison, run the same capture harness on the baseline source and pass that capture directory as the renderer's third argument. The browser requires the installed Playwright module; `FORGE_PLAYWRIGHT_MODULE` can supply its location. Screens use representative system monospace typography, not a particular user's terminal font. Raw captures and fixture journals stay outside Git.

## Verification

The first CI run exposed start-screen integration issues: the task group painted over floating search, and a second prompt-less session's meaningful navigator was hidden. Search now paints above task entry, and multiple sessions retain navigation. Paging checks now create a transcript; notes checks use a nonempty document and the current count label. The allocation growth guard compares one settled turn against 150 turns, keeping both samples in the same conversation viewport rather than comparing task entry with history; its original 100 KiB limit is unchanged. All 195 focused checks across chrome, commands, scratchpad, multiple sessions and render performance pass after these corrections, along with workspace Clippy and formatting.

The original dependency audit rejected `yoke-derive 0.8.3`, unchanged from main. [Maintenance PR #841](https://github.com/NorviaLabs/forge/pull/841) advances only that transitive macro to `0.8.4`; its required CI passes. Phase 1 targets that branch and includes its validated lockfile locally. All three required checks also pass on Phase 1 head `80a64a74`. Neither PR has been merged.

169 distinct targeted tests pass across input, status, workspace, focus, mouse, conversation cache, theme registry, active-theme memoization, navigator lifecycle and supervised tab-row controls. Workspace Clippy, formatting and the debug CLI build pass. Behavior coverage includes first-task keyboard/mouse entry without submitting, paste payload retention, Unicode draft/caret resizing, navigation next to task entry, hidden-pane focus, session-local pane preferences, unsaved editor protection and settled-transcript caching. Appearance is captured for review, without unit tests freezing geometry or colors.

Live CLI validation uses fresh tmux sessions in disposable repositories. [The retained captures](live-phase1/) cover dark navigation with a Unicode draft at 80×18 and 120×40, light start at 120×40 and 160×50, and monochrome start at 80×18 and 80×24. Plain-text captures omit trailing empty terminal rows; their file names record the complete frame size. The installed and new debug binaries both report `forge 0.1.0-beta.11`; the source baseline, rather than that version string, identifies the comparison. No model prompt is submitted. The initial walkthrough found two first-task inconsistencies that are corrected here: hidden system context prematurely changed the placeholder, and opening navigation exposed the old metadata welcome block. It also found clipped trust choices at 80×18, corrected and checked in Phase 2.

Timing from the capture harness measures synchronous key dispatch plus TestBackend drawing in a debug build. It excludes a real terminal paint and is sensitive to machine load. Retain these measurements as a Phase 5 baseline, not evidence of measured usability improvement.

| 100-key sample at 120×40, dark | Main median / p95 | Refresh median / p95 |
| --- | --- | --- |
| First task | 3.765 / 5.066 ms | 1.435 / 1.560 ms |
| Working fixture | 3.713 / 6.916 ms | 3.068 / 3.202 ms |

Repeated working samples varied with machine load. Phase 5 must compare sustained streaming, many tasks and long transcripts with the same workload before claiming a performance change. At 80×18 the default composer consumes two rows rather than three, leaving one additional inspection row; the same short fixture patch shows four useful diff rows in both captures. Longer draft and evidence cases are retained for later workflow verification.

## Phase 2 comparison

[The Phase 2 gallery](phase2-captures.html) contains 180 production draw frames: fifteen states at the same four sizes and three presentations. It compares shared states against Phase 1; additional states have no previous-build pane. Plans, approvals and session/model rows use clearly labeled mock fixtures. Review waits for Git, and the terminal fixture waits for a real command completion with exit code zero before capture. The gallery validates presentation, not live provider behavior or a usability improvement.

Speaker labels now sit directly above their content. Diff titles share the source viewer's neutral frame and origin; compact review drops duplicate title/padding rows while keeping line numbers, signs, hunk headers, file navigation and close/help controls. Existing source panning and dirty-buffer protection remain covered.

Focused approvals replace their viewport completely, rather than painting on top of old transcript characters. Owner, request, literal invocation, cwd, environment and consequence scroll independently above pinned controls. New requests select Don't run. Confirmation requires a painted request; queued decisions retain session/call identity and the actor rejects a changed call. Session-wide approve-all retains its existing policy and continuation. Help and picker keys/clicks cannot activate a pending decision beneath them.

Session, file and resume pickers size to their content. The session picker keeps a visible search field and two short control rows; help pages independently with a pinned close hint. Closing help preserves the original draft, reading position and text selection when the underlying transcript mapping is unchanged.

Startup trust keeps both choices reachable at 80×18 with an unelided Unicode path. Long save errors join the scrollable details rather than displacing decisions. The built CLI was inspected in a disposable workspace and exited without granting trust. Generic errors retain bounded visible details after their toast expires; credential-like errors omit raw payloads and direct the operator to /connect without assuming a provider. Failure tests preserve disk contents, an unsaved editor buffer and a follow-up draft; the conversation remains reachable for error inspection. Existing transient provider retry behavior is unchanged; terminal failures and failures after partial output still require explicit recovery.

Phase 2 review exposed a remaining terminal issue: a horizontally clipped shell echo of an explicit `!command` can expose its status wrapper. It is recorded for the final terminal/workflow verification; manual shell input remains a separate path. This is observed execution evidence, not a simulated job tail.

For an opt-in native mock-session walkthrough, run the compiled `forge-tui` test executable with `walk_refresh_ui --ignored --nocapture --test-threads=1` inside tmux. `FORGE_NATIVE_THEME` selects a built-in theme. The fixture uses the production UI loop, temporary credentials/preferences/pane storage and the mock model; it must not be described as a live provider session.

All 565 distinct targeted TUI checks and the supervisor request-identity check pass. Workspace Clippy, formatting, the debug CLI build, all 180 gallery render checks and the manual native fixture pass. [Eight native captures](live-phase2/) retain minimum-size trust, help scrolling, Unicode drafts after help/resize, actual shell output at two sizes and composer input after returning from the shell. Connection setup was dismissed without selecting a provider; no live model request was sent. The remaining explicit-command echo issue above is still scheduled for Phase 5.

## Phase 3 comparison

[The Phase 3 gallery](phase3-captures.html) contains 228 production draw frames: nineteen states at four sizes and three presentations, with shared states compared against Phase 2. Queue text comes from the real queue API. New job fixtures run an actual successful command, a nonzero exit and a cancelled sleeper through the existing executor. The Agents filter is an explicitly empty state; child workflows remain Phase 4 work. Provider responses are mocked throughout.

Prompt and task selection now retain owner and identity instead of a mutable row index. Mouse actions use the identities painted in the last frame. Repository prompt cancellation atomically checks both owner and queued status; legacy session instructions use their separate queue identity. Promotion or removal cannot redirect a stale cancel to the successor. Returning the last prompt to the composer requires an empty draft before cancellation. If typing or inspection changes during acknowledgment, text is preserved in the owning parent's draft without taking focus or sending it.

The dock uses its actual line budget: a header and selected row below 28 frame rows, and up to three prompts and three tasks at comfortable heights. Only the selected background task can use an available detail row. Counts disclose hidden rows, while expired successes remain accessible in the live task view. `/tasks` and measured footer chips share Jobs, Agents and Queue filters. Empty filters use a short card; details scroll above pinned controls. Cancelled work is counted separately from success.

All 414 distinct targeted behavior checks pass across TUI interaction, ordering, expiry, identity, queue/edit races, the core queue and supervisor ownership. Workspace Clippy, formatting and the debug CLI build pass. All 228 browser render checks pass after reviewing the selected-row marker, light-theme heading contrast and compact empty state. Captures validate appearance rather than measured usability.

[Six native captures](live-phase3/) record the overflowing 80×18 dock, selecting prompt 2, cancelling that prompt only, the retained Jobs list at 120×40, the empty Agents filter and return to the unchanged Unicode parent draft. The production UI-loop fixture exits successfully. It uses temporary preferences, credentials, journals and real disposable jobs, without a live provider request. The native fixture opens task inspection after startup, because startup preflight owns connection/setup overlays.

The old shared Cargo target disappeared after a disk-space failure. Remaining builds use `/private/tmp/forge-ui-target` with development/test debug symbols and incremental compilation disabled. No user target was deleted by this work. Phase 5 performance comparisons must rebuild both sources with these same flags. All three CI checks pass on Phase 3 head `6849597d`; the PR remains open.

## Phase 4 — Jobs

[The Jobs gallery](phase4-jobs-captures.html) contains 276 production draw frames: twenty-three states across four terminal sizes and dark, light and monochrome. New states inspect long output through its end, cancelled partial output, the named stop default and explicit result insertion. Shell evidence comes from actual disposable commands through the existing executor; provider responses remain mocked. The previous-build pane uses Phase 3 captures.

Background shell launches expose their actual cwd and bounded stdout/stderr through the existing registry and immutable snapshot. The same readers continue draining with the existing model-facing output budget. Inspection retains the first 32 KiB per stream, labels truncation, displays controls and bidi formatting literally, and reports the process exit code as data. A cancelled task retains bytes captured before interruption; it does not invent an exit. Missing metadata and launch failure are explicit. Execution evidence is held in memory: this change does not alter the journal format or restore live output across a restart. Existing retained summaries/failures remain inspectable when present.

Normal completion leaves the draft, caret, focus and queue unchanged. `i` appends sanitized evidence to the parent's draft without submission or enqueueing; long inserted results remain editable and scroll within the composer. `x` on active work opens a named confirmation defaulting to Keep running/Keep waiting. Enter on that default returns without interruption. Stop checks the painted parent/task and current lifecycle. A terminal task is hidden only from the dock; its registry entry and `/tasks` result remain.

All 323 distinct targeted checks pass across the tools, core, session and TUI, including confinement, actual exit status, bounded reader evidence, immutable byte snapshots, process-tree cancellation, retained partial output, original-target stop, completion races, mouse ownership, end-of-output paging and explicit append. The first confined test run was blocked by the outer sandbox; the same tests pass with Forge's own confinement able to run. Workspace Clippy, formatting and the debug CLI build pass. All 276 browser render checks pass; representative dark, light and monochrome Jobs frames were visually reviewed.

[Six native captures](live-phase4-jobs/) show actual running output at 80×18, the default stop choice, the same job still running after Enter, its retained partial output after explicit stop, insertion without submission, and the parent draft above that result. The native fixture exits successfully. All six queued prompts survive the walkthrough. Its responses are mocked; this is native interaction and execution evidence, not a live-provider or usability benchmark.

Child mode/workspace metadata, exact child-request decisions, parent reading-state restoration and partial assistant findings are implemented in the next Phase 4 slice. Motion, final release verification and fair performance measurements remain Phase 5 work.

All three Jobs CI checks pass on head `f3d2a7e5`; PR #844 remains open. The Subagents branch starts from that head.

## Phase 4 — Subagents

[The child gallery](phase4-subagents-captures.html) contains 348 production draw frames: twenty-nine states at four sizes and three presentations. The new states cover the Agents list, read-only journal inspection, exact requests, safe named stop, interrupted evidence and explicit insertion. Provider responses are mocked. The dirty child file is test setup; cancellation and shell execution use the actual runtime. These captures establish presentation, not measured usability.

Child inspection refreshes the actual journal while active and reads the final journal once after completion. Its owner, child session, execution, mode, workspace and branch remain separate from the parent's live stream and pending decisions. Returning restores the parent's draft, caret, focus, follow/reading state, cache and text selection. If the parent advanced, a one-time full settled render locates its previous visible rows; ordinary scrolling still reuses the existing bounded cache buckets. Unchanged child journals preserve their revision and render cache.

Both `a` and `d` inspect the same full child request, defaulting to Don't run. Confirmation binds the painted owner, task, unique execution, child and unique request; the producer rechecks the full payload and rejects stale or repeated replies, including reused provider call IDs. Initial and resumed child activity now comes from the existing stream probe. Retained child actors support explicit follow-up without changing writer scheduling or permission policy. Read-only scheduling mode is metadata, not a new filesystem access policy.

Named stop checks that execution at dispatch. Failed or cancelled children can explicitly append retained assistant findings, bounded and labelled partial/unverified, without sending or queueing them. Reasoning and tool messages are excluded. Stopping retains dirty worktrees; a read-only child's shared workspace is never treated as a disposable child checkout.

All 329 distinct affected checks pass across core, session and TUI. The final 26 task checks also cover corrected environment disclosure, same-length journal changes, producer identity guards, hidden parent decisions, final evidence, reading/selection restoration and sibling/queue/dirty-checkout retention. Workspace Clippy, formatting and the debug CLI build pass. All 348 browser render checks pass after correcting a fixture that initially reset the light child theme to dark.

[Native captures](live-phase4-subagents/) record the actual production UI loop at 80×18 and 120×40: full request paging, Don't run and Keep waiting defaults, named stop, partial insertion, retained parent draft, an explicitly approved disposable command and a separately refused sibling command. All six queued prompts and the dirty checkout survive the interruption. The walkthrough exits successfully. A second short fixture captures corrected decision metadata. These are disposable mock-provider sessions, not live-provider or usability measurements.

## Phase 5 — Motion and complete workflows

Working indicators use the existing event loop and a monotonic 125 ms clock. Braille frames retain one cell, `NO_COLOR` uses ASCII, and `[tui] reduced_motion = true` keeps a static `*`. Missing preferences default to false. Waiting, queued and terminal outcomes stay static; background approvals say waiting instead of changing an elapsed counter. Input and external-state polling continue independently of decoration. Available stream chunks paint on the next frame, with the artificial reveal timer removed; resize/cache behavior remains bounded. The existing 150 ms busy-line entrance debounce is retained. The proposed arrival flash was evaluated and omitted in favor of immediate focus, exact request text and a steady warning.

The composer paints its caret on the retained grapheme without adding a character to the draft; literal block characters and combining text survive. End-of-draft painting reuses parsed lines and source spans. Explicit result handoff preserves paragraph breaks. Child reading metadata uses four source rows with visible path/branch elision, while decisions and `/tasks` keep full details. Shared word wrapping now measures terminal cells rather than UTF-8 bytes, fixing extra rows around ellipses and wide text. This reuses the already locked `unicode-width 0.2.2` package; no package version changes. Explicit Zsh commands keep their status wrapper hidden through clipping, scrolling and resizing. The legacy foreground path now initializes its turn timer so the debounced waiting indicator can appear.

[The final gallery](phase5-captures.html) contains **564 production frames**: 32 states at 80×18, 80×24, 120×40 and 160×50 in dark/light/monochrome, plus ten critical states at 115×27, 116×28, 120×29, 131×40, 132×40 and 133×40. The control-output state executes an actual disposable command containing ESC, tab, carriage return and bidi bytes and shows them escaped. Overflowing literal commands, Unicode paths, partial output, retained dirty children and result insertion are included. All frames render without browser errors. The browser font remains representative; these are rendering checks rather than measured usability.

[Native evidence](live-phase5/) covers immediate mock-provider streaming with retained typing, moving versus reduced-motion indicators, explicit Zsh output before/after resizing, a middle caret with a literal block character, child request/stop defaults, partial insertion, and the corrected four-row child banner at 80×18. Native fixtures exit successfully. The [motion replay](phase5-motion.html) and [WebM](phase5-motion.webm) replay actual timed native text captures; terminal colors and fonts are not reproduced. Provider responses are mocked, while shell execution, cancellation, journals and child workspaces use the real disposable runtime.

**512 distinct targeted checks pass** across TUI, configuration, transcript and the eight allocation/cache guards. The final caret/foreground cohort passes all 47 checks after optimization. Workspace Clippy, formatting and the release CLI build pass; the binary reports `forge 0.1.0-beta.11`. Required remote checks are linked above and must pass on the final published head.

### Input measurement

[Raw paired rounds](phase5-latency.json) compare Phase 1 `80a64a74` with the final code using identical debug/test flags, 120×40 TestBackend drawing and actual key dispatch. Five pairs per workload alternate process order; each measures 100 keys after five warm draws. Setup is outside the sample. Many-jobs setup runs 40 real disposable commands, history contains 150 turns, and the long draft starts at 8,000 characters. Stream samples measure dispatch/draw with available chunks, not provider transport or a real terminal paint.

| Workload | Phase 1 median / median round p95 | Final median / median round p95 |
| --- | --- | --- |
| Typing | 2.429 / 2.540 ms | 2.436 / 2.536 ms |
| Available stream chunks | 3.442 / 3.618 ms | 3.432 / 3.620 ms |
| 40 jobs | 2.526 / 2.624 ms | 2.523 / 2.631 ms |
| 150 turns | 2.965 / 3.069 ms | 3.002 / 3.112 ms |
| 8,000-character draft | 16.391 / 16.525 ms | 16.542 / 16.789 ms |

Tightly interleaved samples still show a small history increase of 0.037 ms and a long-draft increase of 0.151 ms. Final medians fit the baseline ranges observed across earlier identical-flags repeats, retained in the report. Machine-load runs varied much more; do not infer a speed or usability improvement. Long drafts still require about 16.5 ms of synchronous work, and terminal paint time remains unmeasured.

### Complete-workflow evidence

| Acceptance flow | Concrete evidence |
| --- | --- |
| Delegate and steer | [Native queue selection/cancellation](live-phase3/); prompt identity, promotion races, late edit acknowledgment and parent-local draft checks in `app::tests::commands` and `multi_task`. |
| Inspect and decide | [Native help/review/shell states](live-phase2/) and Phase 2 captures; safe default, exact painted request, picker ownership and unsaved-reader checks. |
| Handle a job | [Native output, named stop and explicit handoff](live-phase4-jobs/); real output/exit status, cancellation retention, no-child explanation, observer-only completion and append-without-submit checks. |
| Handle a child | [Native exact decisions and interruption](live-phase4-subagents/) plus final spacing captures; producer request/run guards, parent reading/selection restoration, sibling/queue retention and dirty checkout checks. |
| Recover and resize | Phase 2 recovery/retained shell evidence, final breakpoint gallery and [native reduced-motion/Unicode editing](live-phase5/); affected cache, input, foreground event and performance checks. |

The source tree of merged main `c800a445` matches the original Subagents baseline exactly. Phase 5 is delivered as a separate feature PR against that main; the user's mockup checkout and untracked implementation plan are preserved.
