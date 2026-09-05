# Baseline

- Commit: **`dfa46a07aa43551395ba7cb6b4867ab2d8a9c324`** — latest fetched `origin/main`, PR #519 release merge.
- Version: **Forge 0.1.0-beta.9**, verified from the newly built binary.
- Branch: `feat/astra-tui-2026`, isolated worktree `.worktrees/astra-tui-redesign/`.
- Build: `cargo build --release --locked --package forge-cli`, documented in README; passed at the final baseline in **45.36s** after the initial full build. `./target/release/forge --version` passed.
- Original checkout: clean `feat/forge-tui-2026-design` at the initial main SHA. No existing user work was removed or reset. The isolated design branch was fast-forwarded when PR #519 reached main.
- Terminal: real tmux PTYs on macOS, host `TERM_PROGRAM=iTerm.app`, `TERM=xterm-256color`, `COLORTERM=truecolor`. Forge used existing OpenAI ChatGPT route, GPT-5.6 Luna, Medium. The host's exact font setting was not read; font validation is separately documented.
- Sizes: **80×18, 80×24, 100×30, 120×35, 160×45, 220×55**.
- Investigation timing: real Forge PTY started **2026-09-05 09:05:55 UTC**. **30m52s** across 09:05:55–09:34:44 and 14:22:51–14:24:54 UTC. The intervening pause is excluded; see [the investigation log](current/investigation-log.md). Build and competitor-only startup time are excluded. The investigation window includes interacting, observing asynchronous turns, recording findings and checking proposed layouts; it is not a claim of uninterrupted keystrokes.
- Main scenarios used the baseline repository read-only. Editing, tests, trust and editor conflict scenarios used the same binary against a separately committed disposable Python fixture, `/private/tmp/astra-forge-fixture`, branch `fix/ux-fixture`.

Main advanced once during the design session from `25f0cb82a86482a3a588e294b7f992355760ef07` (beta.8) to the final baseline above. The intervening diff changes only workspace version entries in `Cargo.toml` and `Cargo.lock`; no TUI/runtime source changed. The full 30m52s scenario study ran on the source-identical beta.8 build, followed by a successful beta.9 release build and a fresh real-PTY launch at 120×35. The final proposal is therefore mapped to the latest main source while retaining valid interaction evidence.

# What Forge currently gets right

- Conversation expands when no editor/diff is open. Do not reintroduce a large empty editor placeholder.
- The composer is a strong keyboard anchor; multiline pasted input remains legible and grows predictably.
- Exploration groups and Ctrl+O disclosure already exist. The redesign should improve their boundaries and outcomes, not add a competing interaction.
- Plans have counts, active-step treatment, associated tool metadata, a pinned summary and `/plan` inspection. Preserve these capabilities.
- Model picker preserves route identity: the same Luna model through OpenAI ChatGPT, OpenCode Go and OpenCode Zen appears separately. Current selection and active model are distinct. Even 80×18 keeps identity columns and active-route detail.
- Embedded editor has Normal/Insert, dirty protection and a distinct external-change prompt. These are valuable safeguards.
- Interactive terminal has an unmistakable focused rule and preserves the shell when closed. Its output remains a real PTY, not a simulated command log.
- Review Changes handles binary files explicitly, keeps a changed-file list and provides existing navigation/hunk/review controls.
- Conversation scrolling from composer and Ctrl+End return-to-latest work. Do not reset those anchors on every stream update.
- Semantic grouping, privacy redaction, provider routes, queues and task ownership already provide the information needed for most proposed polish.

# Top UX problems

Rank reflects frequency and impact in these sessions, not a quantitative user study.

| Rank | Current behavior | Why it feels unpolished | Frequency / consequence | Reference comparison | Proposed principle |
|---|---|---|---|---|---|
| 1 | Original shell row still says `running` after successful polling completion; cancelled turn also leaves a running row. | History can contradict the actual turn state. | Every long command/poll cycle tested; user cannot trust a quick scan. | Codex hides polling mechanics more effectively; its failed empty command is still ambiguous. | Reconcile compact state by existing session identity; preserve unknown outcomes. |
| 2 | Previous `Answered in …` metadata appears beneath newer activity while a new turn runs. | Completion is visually detached from the answer it describes. | Multiple turns; old timing competes with live work. | Codex visibly separates final answer from prior activity with a rule. | Per-turn ownership of answer and metadata. |
| 3 | Opening editor/diff at 120 columns leaves chat about 30 columns wide. | Real answers wrap excessively and completion metadata clips. | Every editing/review workflow; delegation becomes hard to follow while inspecting code. | Reference CLIs devote most width to conversation; Forge needs a deliberate two-purpose compromise. | Set a 44-column chat floor, retain 32-column workspace minimum. |
| 4 | Open terminal at 80×24 consumes most height; composer disappears. | The user's primary control is not reliably present. | Compact terminal scenario; disrupts return to delegation. | OpenCode maintained its composer during narrow long-task rendering. | Reserve input and minimal transcript before allocating terminal height. |
| 5 | Poll calls and repeated waiting narration accumulate above a very short final answer. | Watching work feels like reading transport logs. | Three-minute command: repeated `write_stdin` and waiting messages. | Codex exploration aggregation helps; OpenCode thought labels still accumulate. | Active information stays visible; routine historical detail recedes reversibly. |
| 6 | Completed checklist retains multiline tool metadata and pinned 3/3 summary. | Finished work occupies attention needed by the answer. | Every planned fixture turn. | OpenCode also appends multiple todo revisions. Neither pattern should be copied unchanged. | Latest plan in compact view; revisions/detail retained; active step dominates. |
| 7 | Approval repeats waiting in transcript, strip, composer and footer; reason truncates. | More signaling does not produce more understanding. | Safe outside-workspace read; action context competes with chrome. | OpenCode concentrates scope and choices at the input boundary. | One approval owner; exact action/reason/cwd; original choices. |
| 8 | Files has an outer box, three-row search box and separator; no-results is blank. | Five rows of chrome precede results; empty and absent look alike. | Every startup/search; important area spends scarce rows on borders. | OpenCode picker uses simple field + list hierarchy. | One-row search, explicit no-result state, shared pane separators. |
| 9 | Editor repeats mode in top and bottom rows; dialogs are taller than content, and conflict footer says `Enter confirm`. | Geometry and hints vary by implementation path. | Editor dirty/conflict and plan/help inspection. | Codex model dialog sizes rows naturally; Claude selector clearly lists local actions. | Shared modal family; single metadata owner; exact reachable hints. |
| 10 | At 220 columns, unbounded prose spans most of the screen; global footer telemetry is dense. | Dense data lacks reading measure and priority. | Wide desktop; scanning long answers costs eye travel. | Codex keeps a simpler input/status hierarchy. | Bound prose at 88 content columns, keep code/terminal wide. |

# Systemic diagnosis

Forge has useful individual patterns but no fully consistent ownership model for **space, time and state**. Panels allocate their own chrome; adjacent panes repeat boundaries. The turn line, transcript projection, feedback strip and footer each contribute lifecycle-looking information. Commands and polling results appear as separate historical entries, so a final process outcome does not necessarily update the earlier visible command. Plan revisions are treated more like new content than an evolving view of the same work.

These are architectural presentation seams. A color refresh alone cannot solve them. The proposal makes existing data flow through a coherent, reversible view projection and central geometry. It does not change execution, journal semantics or agent capability.

The task database warning in the baseline worktree is a real environment-specific failure (`UNIQUE constraint failed: tasks.slot`). The disposable fixture's task strip worked. This study does **not** infer that task mode always fails, nor does it fix the database. It does show that a persistent application-level notice should not look like part of every subsequent answer.

# Scenario evidence and limitations

| Scenario | Result / evidence |
|---|---|
| Fresh start | Actual binary, 120×35. Files + large conversation + composer; degraded task mode notice in baseline, working task strip in fixture. |
| Simple conversation | Repository summary completed in 64s; observed narration, exploration, final streaming and completion. |
| Tool-heavy investigation | Architecture/performance task completed in 84s, 33 tools displayed. No model-quality benchmark or verified performance finding claimed. |
| Real code change | Fixture formatter and tests edited; `python3 -m unittest -v` reported four passing tests. Diff and editor inspected. |
| Plan | 0/3 → 2/3 → 3/3, tool metadata and `/plan` modal inspected. Failed/skipped not supported by observed plan states. |
| Approval | Outside-workspace cat triggered sandbox approval. Run-once showed an approved notice then approval returned; final attempt declined. Successful approval execution in Forge is not claimed. |
| Failure/recovery | Intentional Python exit 1 then printf exit 0; both rows and final explanation observed. |
| Cancellation | Ctrl+C stopped active turn; footer `stop`, original command row still `running`. No claim that child process was terminated. |
| Editor | Normal, Insert, unsaved, command row, discard, inactive view and external conflict exercised. |
| Terminal | Real shell multiline output, focus, close/reopen and compact allocation inspected. Captured internal-looking status-marker text; no production terminal fix attempted. |
| Files/search | Nested expansion, matching/nonmatching filter, selection/open and no-result blank state. |
| Model selector | Three identity columns, filter, current-vs-selection, route duplicates, 80×18 minimum. |
| Review Changes | Real text/binary fixture changes. Width relationship to chat observed. |
| Long conversation | Summary, investigation, long command, failure/recovery, approval, cancellation and follow-up in one session; scroll and detail toggle tested. |
| Responsive | Required five sizes plus enforced 80×18; various editor, terminal and selector states. This is not every component × every size in actual Forge. |

Captures under [current/](current/) are real PTY cell text. They intentionally are not presented as screenshots of font shapes or color fidelity. The optional screenshot requirement is satisfied by evidence captures plus rendered proposed screenshots; current raster screenshots were not required. Prototype font/browser validation cannot substitute for later Ratatui/terminal validation.

# Post-design source reconciliation

`record_turn_summary` in `app/turn.rs` explicitly replaces the previous global summary; `app/render.rs` appends banners after message projection. `classify_tool_content` in `forge-transcript` returns early for exec/write_stdin JSON with running/exited labels. `supersede_plan_checklist` already replaces old full lists with `PlanUpdated` summaries. The design therefore refines plan history instead of claiming Forge still appends every full checklist. The source mapping was performed after the UX investigation and design draft.
