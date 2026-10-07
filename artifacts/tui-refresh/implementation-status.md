# Forge UI implementation

The complete scope is the [updated mockup proposal](https://github.com/NorviaLabs/forge/pull/839) and its five implementation phases. Production work started from main `cf61193ff9f009c2a61ce55672fd24557a8337ee` in an isolated worktree.

1. **Layout and start — implemented in [PR #840](https://github.com/NorviaLabs/forge/pull/840); CI corrections verified locally.** Centered task entry, shared gutters, bounded composer, retained navigation and drafts, quieter chrome, updated built-in palettes.
2. **Conversation and inspection — underway on the next branch.** Transcript rhythm, approvals with safe defaults and reachable long commands, pickers, shell ownership and recovery.
3. **Queue and dock — pending.** Stable task and prompt identities, cancellation/edit races, compact overflow and a live task view.
4. **Jobs and subagents — pending.** Execution evidence, explicit result handoff, read-only child inspection, exact decisions, named stop and retained partial findings.
5. **Motion and complete workflows — pending.** Reduced motion, state-driven effects, performance comparison, real walkthroughs and required CI.

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

The dependency audit separately rejects `yoke-derive 0.8.3`, which is unchanged from main. A lockfile-only maintenance branch advances that transitive macro to `0.8.4`; it is kept separate from the UI changes. Required CI remains a delivery gate.

169 distinct targeted tests pass across input, status, workspace, focus, mouse, conversation cache, theme registry, active-theme memoization, navigator lifecycle and supervised tab-row controls. Workspace Clippy, formatting and the debug CLI build pass. Behavior coverage includes first-task keyboard/mouse entry without submitting, paste payload retention, Unicode draft/caret resizing, navigation next to task entry, hidden-pane focus, session-local pane preferences, unsaved editor protection and settled-transcript caching. Appearance is captured for review, without unit tests freezing geometry or colors.

Live CLI validation uses fresh tmux sessions in disposable repositories. [The retained captures](live-phase1/) cover dark navigation with a Unicode draft at 80×18 and 120×40, light start at 120×40 and 160×50, and monochrome start at 80×18 and 80×24. Plain-text captures omit trailing empty terminal rows; their file names record the complete frame size. The installed and new debug binaries both report `forge 0.1.0-beta.11`; the source baseline, rather than that version string, identifies the comparison. No model prompt is submitted. The initial walkthrough found two first-task inconsistencies that are corrected here: hidden system context prematurely changed the placeholder, and opening navigation exposed the old metadata welcome block. It also found clipped trust choices at 80×18; compact startup decisions belong to Phase 2 and remain outstanding.

Timing from the capture harness measures synchronous key dispatch plus TestBackend drawing in a debug build. It excludes a real terminal paint and is sensitive to machine load. Retain these measurements as a Phase 5 baseline, not evidence of measured usability improvement.

| 100-key sample at 120×40, dark | Main median / p95 | Refresh median / p95 |
| --- | --- | --- |
| First task | 3.765 / 5.066 ms | 1.435 / 1.560 ms |
| Working fixture | 3.713 / 6.916 ms | 3.068 / 3.202 ms |

Repeated working samples varied with machine load. Phase 5 must compare sustained streaming, many tasks and long transcripts with the same workload before claiming a performance change. At 80×18 the default composer consumes two rows rather than three, leaving one additional inspection row; the same short fixture patch shows four useful diff rows in both captures. Longer draft and evidence cases are retained for later workflow verification.
