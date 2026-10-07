# Forge UI implementation

The complete scope is the [updated mockup proposal](https://github.com/NorviaLabs/forge/pull/839) and its five implementation phases. Production work started from main `cf61193ff9f009c2a61ce55672fd24557a8337ee` in an isolated worktree.

1. **Layout and start — implemented in [PR #840](https://github.com/NorviaLabs/forge/pull/840); required CI passes.** Centered task entry, shared gutters, bounded composer, retained navigation and drafts, quieter chrome, updated built-in palettes.
2. **Conversation and inspection — implemented on `feat/tui-refresh-conversation`; local and native checks pass.** Tighter transcript and inspector origins, approvals with safe defaults and scrollable details, compact pickers, retained shell input and explicit failure recovery. [Review the compiled captures](phase2-captures.html).
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
