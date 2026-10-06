# Forge interface refresh proposal

Open [the interactive study](index.html). Use the state tabs, size and theme selectors, and **Play walkthrough**. The walkthrough stops at the decision; selecting **Allow once** resumes it. **Current home** shows captured Forge cells at the same terminal preset. SVG and PNG exports save the visible state.

[Nine-screen overview](contact-sheet.png) · [Spacing before / after](spacing-comparison.html) · [Motion walkthrough](motion.webm) · [Full study preview](overview.png) · [Individual screens](screens/) · [Validation results](validation.json)

This is a proposed interface and local interaction simulation. The task, patch, timings, context use, session roster, and test results in the proposal are illustrative. They are not results from executing an agent task. Forge's Rust implementation and design contract have not been changed.

## Evidence and scope

Reviewed 6 October 2026, starting from current `origin/main`, `cf61193f` (“Make conversation the primary TUI workspace”, PR #838). Work is on `feat/tui-refresh-mockups`. The installed Forge binary reports `0.1.0-beta.11`; its modification time is 6 October. The UI audit also read the current `FORGE-DESIGN.md`, layout, hints, approval, and conversation code. A binary timestamp does not establish an embedded source revision; the installed and source evidence are identified separately.

Live inspections used a newly created disposable repository at `/private/tmp/forge-refresh-audit`, with no agent prompt submitted. Forge home was captured at 80×18, 80×24, 120×40, and 160×50; `/model` was captured at 120×40. The repo had no initial commit, so the “No commits yet” branch text in these captures is a fixture condition. Active conversation states were assessed from the current renderer and design reference; the proposed active states use sample content. This is a bounded design review, not a usability experiment.

The reference viewer reconstructs captured text, foreground/background colors, and bold with a representative monospace font. Original ANSI geometry is retained; terminal-specific italic/dim rendering is not reproduced. The Codex capture replaces a personal usage-limit warning with an explicit omission marker.

| Reference | Evidence reviewed | Pattern to apply | Tradeoff to watch |
| --- | --- | --- | --- |
| [Codex CLI](https://learn.chatgpt.com/docs/codex/cli) | Installed 0.160.1 startup and official CLI examples | Compact identity and a direct composition surface keep the next task obvious. Preserve readily accessible configuration and permissions. | A transcript alone gives less simultaneous visibility into files and other sessions; Forge should retain its inspector and navigator. |
| [Claude Code](https://code.claude.com/docs/en/fullscreen) | Installed 2.1.291 in safe mode; fullscreen and interactive-mode documentation | A clear prompt boundary and restrained transcript hierarchy. Its documented focus view and adjacent diff support showing summary and evidence together. | Keep inspection discoverable when tool details are folded. The safe-mode capture contains a warning that is not ordinary production chrome. |
| [OpenCode](https://opencode.ai/docs/tui/) | Installed 1.18.34 startup and official TUI commands | An intentional start composition, a prominent prompt, and local action hints. Details remain expandable. | A large startup wordmark costs rows. Forge's compact start should emphasize the task at its minimum size. |
| [Amp](https://ampcode.com/docs/cli/keybindings) | Official command-palette screenshot and keybindings; installed `0.0.1789416054-g834320` version | Searchable actions paired with their bindings; collapsible thinking and tool blocks support a calmer transcript. | The installed account was expired; no authenticated live workflow was inspected or reconfigured. Treat the screenshot as an official reference, not a version-pinned local rendering. |
| [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) | Official [desktop/Web preview](https://www.deepseek.com/harness/), repository, and documented Web launch; repository revision `5badb15009ae1756c3afe0ae0cef1faafc290ccc` | A session workspace with readable conversation and inspectable execution history. Keep capability/configuration detail available through dedicated views. | This is a cross-surface comparison: the reviewed official visual is desktop/Web, not a captured terminal TUI. Translating browser surfaces into permanent terminal panels would consume the reading budget. |
| [Reasonix](https://reasonix.io/) | Official illustrated CLI sequence, stable `main-v2` TUI source, and version-line documentation; source revision `60204c30300241526541f5003bc82a3aa005ec02` | Readable progression through plan, decision, edits, and validation. The [TUI ownership gate](https://github.com/esengine/DeepSeek-Reasonix/blob/60204c30300241526541f5003bc82a3aa005ec02/internal/cli/chat_tui.go#L2235) distinguishes modal controls from prompt-owned controls. | Stable 1.x CLI and Studio 2.x are different lines. No Reasonix binary or provider task was run; do not attribute the desktop's appearance to its terminal implementation. |

The six tools are sources for specific patterns, not a claim that they share one visual design or have identical capabilities. There are no invented rankings or task-time improvements.

## Findings in current Forge

1. **An empty navigator has high visual weight.** At 120×40, the full-height framed navigator occupies roughly 28 columns even in a fresh empty workspace. Its nested tab outlines compete with the first action. Proposal: omit it on the start screen and use quiet tabs and a separator when navigation is useful.
2. **Configuration repeats while the input remains modest.** Home shows provider and model metadata, and the footer repeats it. The input is a small bordered field at the bottom. Proposal: lead with a concrete task entry, three complete starters at comfortable heights, and one at 80×18. Keep configuration in a consistent footer.
3. **Narrow metadata can collide.** The 80×18 live capture shows the notes chip truncated next to the lifecycle text. Proposal: budget the configuration and lifecycle separately, then remove secondary metadata before useful controls.
4. **Existing strengths should carry forward.** Main already gives the conversation priority over inspection, supplies meaningful session states, and separates focus from selection and outcomes. The refresh improves alignment, hierarchy, disclosure, and attention within that foundation.

The first three are observations from the captured startup. Active-state changes are design hypotheses grounded in the source and task flows; this study does not establish that users complete those flows faster.

## Proposed experience

| State | Primary task | Proposed treatment |
| --- | --- | --- |
| Start | Begin useful work | One purposeful composition; direct task input; full starter prompts; navigation on demand. |
| Plan | Understand the next step | One checklist with a current task and reported completion count. Plan state remains distinct from execution evidence and permission. |
| Working | Follow and steer | Grouped activity, a fixed-width running indicator, and a readable answer. The composer can queue a follow-up without taking focus away from inspection. |
| Review | Assess the patch | Conversation alongside code at sufficient width. Old/new line numbers and signs remain intact. Horizontal panning reveals full code; F6 reaches the retained narrow inspector. |
| Approval | Decide about one command | Exact command, affected session/workspace, and consequence in one surface. Static waiting state and paused prompt. Proposed default: “Don't run”. |
| Sessions | Find work needing intervention | Text and distinct markers for needs-you, working, queued, complete, failed, cancelled, and retained/blocked state. A selected session preview names its branch and next action. |
| Commands | Discover an action | Searchable Forge commands, descriptions, and bindings. Rank command-name matches ahead of incidental description matches. Closing restores the prior view and draft. |
| Terminal | Inspect local execution | A retained shell surface with its own input owner. Model input remains separate. The local test example is explicitly distinct from remote CI. |
| Recovery | Continue after interruption | Persistent failure text, retained patch and draft, and an explicit retry or inspection action. No automatic retry or invented completed state. |

### Geometry and hierarchy

Product screens use one monospace size and a literal cell grid. Presentation zoom scales the screenshot; Forge does not change the user's terminal font. The outer window bar and study page are presentation scaffolding.

- One header row for repository/branch and session attention; one neutral rule.
- One text-tab row naming conversation and the resource. A `>` marker identifies keyboard ownership independently of selection.
- During a task, the composer spans the working surface. On the start screen it joins the heading and starters in a centered task group, capped at 84 columns. Its surface and top rule identify the input boundary, with no enclosing nested box.
- Prose and inspector metadata share a two-column inner inset. Focus, disclosure, and outcome markers use explicit leading columns rather than adding a second layer of pane padding.
- One blank row separates distinct transcript blocks. Speaker labels and their body, list items, and grouped command output stay together.
- Two footer rows show contextual actions and model/lifecycle state. Neither reserves an empty background strip while idle.
- Routine activity stays neutral. Warning means a human decision; success labels require evidence in the actual implementation. Prose remains the primary foreground.

| Preset | Review arrangement | Compact behavior |
| --- | --- | --- |
| 160×50 | 24-column navigator; remaining work surface split approximately 60/40 between chat and inspector | Persistent navigation is possible without narrowing the prompt. |
| 120×40 | Chat and inspector; navigator on demand | Inspector has less width than a full-screen source view; horizontal panning is available. |
| 80×24 | Retained chat or inspector, reached with F6 | Compact transcript and no permanent navigator. |
| 80×18 | One primary body view; composer and necessary actions remain | One starter; compact checklist; complete command and approval options; all seven example sessions fit. Longer lists would scroll to the selection. |

Actual implementation should decide breakpoints from pane minimums rather than these four presets. The prototype uses current review eligibility at 116 columns and a proposed 24-column navigator from 136. Validate the breakpoint neighborhoods before changing production layout.

### Spacing review

The [before / after comparison](spacing-comparison.html) ([PNG](spacing-comparison.png)) uses the original proposal at `c3f14bab` and this revision, with matching terminal dimensions and example content. It is a comparison of proposal versions; **Current home** remains the separate installed-Forge reference.

| Surface | Issue in the first proposal | Revised treatment |
| --- | --- | --- |
| Start | The welcome text, starters, and bottom prompt were separated by a large gap and used different left edges. | One centered task group; heading, prompt text, starters, and local hints share their text origin. The prompt sits directly below the heading and configuration, followed by the starters. |
| Conversation | Prompt text, activity, answer, and file rows had different insets. Two- and three-row rests separated short related blocks. | Two-column inner inset; one blank row between distinct blocks; labels and their content stay together. Tight lists remain consecutive. |
| Inspector | The title and metadata started at different columns, and the hunk header added another empty row before code. | Align title, metadata, and hunk text; keep line numbers and signs in fixed columns; show code immediately after the hunk. Horizontal panning retains the full source. |
| Approval | A tall fixed card and a gap above it separated the command from the explanation and decision. Hints repeated inside the card and footer. | The card follows its context and sizes to the wrapped explanation and choices. Exact command, workspace, consequence, and both choices remain visible. One authoritative footer gives the decision bindings. |
| Compact workspace | A three-row prompt and a spare row above it consumed two useful body rows at the minimum size. | Two-row prompt and no extra body-to-prompt spacer below 28 frame rows. The 80×18 example retains its test result and shows seven session entries, compared with five before. |
| Sessions and shell | Session hints repeated in the body. The shell was detached from the answer by a large vertical gap. | Contextual footer hints; session rows use available body height; shell output follows its explanation with one blank row. |
| Pickers and footer | Menus used a fixed height and sat low in the frame. Some hint widths reserved more space than the adjacent metadata needed. | Menus size to their result count and center within the body, keeping the header and input area clear. Footer labels reserve their actual widths and leave a gap before hints or model text. |
| Study page | Presentation padding used several unrelated pixel values. | Eight-pixel steps for section padding, headings, and preview captions, with four/eight-pixel gaps for controls. This scale applies to the surrounding HTML study; product spacing remains character cells. |

Short terminals remove padding and repeated hints before removing the command, decision, editable prompt, or validation evidence. The start group's surrounding empty canvas is intentional; related content stays together without stretching rows to fill the window. These choices improve the proposal's consistency and content budget; they do not establish a measured usability gain or change Forge's runtime layout.

### Color and effects

| Semantic role | Dark | Light |
| --- | --- | --- |
| Canvas | `#14171b` | `#f6f7f9` |
| Surface | `#1d2229` | `#ebeff4` |
| Primary text | `#edf0f5` | `#202936` |
| Secondary text | `#b5becb` | `#47576a` |
| Focus | `#86b5ff` | `#255caa` |
| Active work | `#e5bb80` | `#80511b` |
| Warning | `#f3c575` | `#745100` |
| Success | `#9bd1ad` | `#246040` |
| Error | `#ee94a0` | `#a52d43` |

These are proposed mappings onto Forge's semantic theme roles. Mono mode demonstrates readable focus, outcomes, and diff signs without hue. More production color-capability validation is needed; this is not an ANSI-fallback test against a real terminal.

| Motion | Timing / trigger | Static behavior |
| --- | --- | --- |
| Active work | One-cell eight-frame spinner at 125 ms; only while running | Fixed `>` with “Working” and elapsed text. |
| Answer arrival | Simulate incremental arrival in the walkthrough; render available chunks without deliberately slowing the real stream | Existing content remains readable. |
| Tool completion | Replace the active marker with a tick in the same reserved cells | Result wording persists. No moving row. |
| Approval arrival | Brief brighter top rule for 240 ms, then steady warning | Steady rule and explicit decision wording. Waiting has no looping animation. |
| Focus / pane change | Immediate cell redraw | Owner marker, caret, and local rule remain. |

Use the existing UI tick; redraw only changed regions in production. Do not add a second animation loop, a fake completion percentage, shimmer placeholders, fading text, sliding panes, blur, transparency, or terminal font dependencies. Debounce running indicators for very short work. Stop motion when idle, and honor a reduced-motion preference/configuration. A production implementation still needs latency and sustained-stream measurements.

## Validation and next implementation slice

`node artifacts/tui-refresh/check.mjs` uses the installed Playwright module and Chromium to inspect the prototype. `FORGE_PLAYWRIGHT_MODULE` can point at another installed Playwright module. `python3 artifacts/tui-refresh/capture-reference.py` rebuilds the offline reference data from saved ANSI captures. No Rust checks are necessary for this artifact-only change; the relevant checks are browser rendering and interaction.

`node artifacts/tui-refresh/check.mjs --video` records the motion sequence. The recorded keyboard selection is a simulated human action; the interactive walkthrough itself never approves automatically. `--references` refreshes the two official page screenshots and requires network access.

`node artifacts/tui-refresh/check.mjs --spacing PATH` renders the comparison using an original proposal HTML file and the current version. The delivered comparison used the original HTML saved before this spacing pass. SVG/PNG exports also include the visible draft or placeholder, so the gallery shows the complete input composition.

The browser check covers 108 combinations: nine principal states × four terminal presets × dark/light/mono. Another 90 renders cover the sizes around starter-count, density, inspector, and navigator changes, plus compact and comfortable file/model/help menus. Checks verify contained actions, clearance around editable input and footer, complete minimum-size approval content, visible compact validation evidence, the seven example sessions, and preservation of decision state with a long model label. Interaction checks cover keyboard decisions, drafts through view switches and resizing, command routing, shell-to-composer return, file selection and panning, session peek and return, SVG/PNG exports including the draft, original captures, reduced motion, and browser exceptions. A clock-controlled walkthrough check confirms it stays at approval until an explicit choice and then reaches review. Updated screenshots and the motion sequence are retained. These are prototype checks, not verification of an implemented Ratatui refresh or a user study.

Before implementation, compare the current and proposed versions on these tasks at the same terminal sizes:

1. Send a task, inspect grouped activity, and queue a follow-up while work runs.
2. Review both changed files, scroll and pan code, then return to the retained draft.
3. Decline a command, and separately approve one exact invocation. Confirm the decision affects the named session and preserves the draft.
4. Find a background request, inspect its owner, answer it, and return to the original session.
5. Open the shell, type without changing the model prompt, leave and reopen it, and inspect a failure without losing work.

Record completed tasks, key/focus transitions, visible useful lines, missed decisions, and any reading-position or draft loss. Set targets from that comparison; do not infer improved usability from the mockup's appearance.

Implement a focused first slice around `widgets/status.rs`, `widgets/footer.rs`, the composer renderer, `widgets/navigator.rs`, `setup.rs`, and `layout.rs`: quieter chrome, consistent origins, purposeful start, and compact budgets. Next, update grouped activity and decision hierarchy in `conversation.rs` / `app/approvals.rs`, reusing current event and authorization paths. Keep the diff and terminal surfaces' input boundaries intact. Review theme tokens and the appropriate `FORGE-DESIGN.md` defaults with the implementation; preserve its §4 invariants.

Do not bundle provider changes, session persistence changes, new runtime permission semantics, terminal emulator replacement, or dependencies into the visual refresh. The proposed approval default and navigator width are explicit behavior/layout changes that need their own targeted checks. Production changes should follow the repository's feature-branch and PR workflow.
