# Agent experience specification

Baseline: `dfa46a07aa43551395ba7cb6b4867ab2d8a9c324`. Highest-priority implementation contract. Prototype states 03–18 are illustrative rendering fixtures, not model-quality claims. No tools, lifecycle states, model prompts, storage policies, commands or workflows are added.

## A. What watching work should feel like

At 30 seconds, the user can identify the request, current action and whether input is needed without decoding API names. At three minutes, the same live row still owns progress; command polling does not build a second transcript. At thirty minutes, earlier answers remain easy to scan, exact activity is still accessible, and the user can distinguish confirmed outcomes from model assertions. Time is elapsed time, never a fabricated percentage or estimate.

## B. Canonical turn anatomy

**Data, in order:** existing user message → intermediate assistant messages/reasoning and tool events → final assistant message → existing completion metadata. Build a reversible *view projection*. Never reorder the stored event stream, rewrite messages, introduce a new persisted plan model, or ask a model to summarize history for this design.

**Active view:** exact request; latest existing plan; consecutive tool groups; current durable narration; unresolved errors and approval; growing final response; one live status immediately above composer. The live status is outside transcript scroll so it remains visible when the user looks back. Do not pin a second copy of its contents in the footer.

**Completed view:** exact request; one neutral activity summary with counts and latest plan progress; any failed/denied/unknown outcomes; final answer; per-turn completion metadata. Intermediate narration, raw exposed reasoning, previous plan renderings and tool payloads remain under the existing Ctrl+O detail mode. The user does not acquire a new “turn object” command, per-turn accordion navigation or workflow.

```text
| You
| Trim name parts and add regression tests.

Plan · 1/3 complete
[x] Inspect formatter and call sites
[>] Edit formatter and add regression tests
[ ] Run unittest verification

[x] Explored repository · 3 reads · 2 searches
    The formatter joins raw values without trimming.
[>] Edit   formatter.py

[>] Editing formatter · 18s
> Composer ───────────────────────────────────────────
  Reply, or describe the next task…
```

```text
| You
| Trim name parts and add regression tests.
[x] Activity · 10 tools · Plan 3/3 · Ctrl+O details

Forge
Name formatting now trims surrounding spaces and
omits empty name parts.

Changes
formatter.py
test_formatter.py
Added blank-first, blank-last and whitespace cases.

Verification
[x] python3 -m unittest -v · exit 0
4 tests passed.
No broader test suite was run.

Finished · 10 tools · 44s
```

The headings, change descriptions and test-count prose above are example assistant content. The renderer **does not manufacture** “Changes,” “Verification,” test totals, uncertainty or findings if the model did not provide them. Existing structured tool summaries can display an exit code, changed path or verdict only when present. Render plain final answers plainly.

## C. Timing and streaming transitions

| Moment | Visible change | Durable / transient | Rendering constraint |
|---|---|---|---|
| T+0 accepted submit | Append request once; preserve existing clearing/queue rules; reserve live row | Request durable, slot transient | Same frame as acknowledgement; no empty assistant bubble |
| T+250ms | If work remains, show `[>] Working` and reachable interruption hint | Transient lifecycle only | Preserve current busy debounce if longer; no flash for already-finished request |
| T+1s | Elapsed updates to 1s; phase changes only on evidence | Transient | Timer invalidates chrome, not full Markdown cache |
| First reasoning | Honor current reasoning visibility setting; concise existing summary may update live label | Raw exposed reasoning retained under current visibility policy | Do not reveal hidden reasoning or synthesize chain-of-thought |
| First tool | Create typed row with exact target; use running state | Durable tool identity and events | No second “assistant is working” block |
| Multiple tools | Append to contiguous compatible group; current call remains visible | Group is a view, calls remain durable | Stable group identity; no all-history rescan per tick |
| Intermediate finding | Render exact assistant narration secondary between groups | Persistent; no heuristic deletion | Do not promote “I found” to a verified outcome by parsing prose |
| Waiting approval | Freeze active animation; focus treatment follows actual approval owner | Existing approval record retained | Command/reason/cwd/choices dominate; background footer quiet |
| Failure | Show `[!]` plus exact exit/error status immediately | Persistent and visible in compact mode | Failure is not conveyed by color alone |
| Recovery | Append confirmed successful result; retain failed predecessor | Both durable | Relate retries only when existing identifiers support it |
| Final response streaming | Stream into final-answer area; stop giving finished tools active emphasis | Partial answer retained using current semantics | Preserve settled Markdown cache; append unfinished tail |
| Completion event | Remove live row, freeze duration, compact routine activity once | Final answer and evidence durable | Stable visible anchor; do not compact if user is reading expanded detail |
| Five minutes later | Same completed view; no residual animation, stale “running,” or color decay timers | No timed deletion | Expansion still returns exact available detail |

Only phase text, empty placeholders, spinner frames and redundant live counters may disappear. A message with nonempty text is never deleted because it sounds repetitive. Nonfinal narration can move into the detail presentation after completion; it must be restored verbatim, in event order, on expansion.

## D. Tool rows and grouping

State prefix is 3 columns; one gap; kind is up to 6 columns; one gap; target uses the remainder. Wrap continuation under the target. At narrow widths, remove the kind padding before wrapping the exact target. Do not append `Ctrl+O` to every tool row; one hint belongs to the group/detail boundary.

| Actual state | Compact row | Detail |
|---|---|---|
| Queued | `[ ] Shell  <command>` | Arguments/cwd if already known; no invented start time |
| Running | `[>] Shell  <command>` + `session #n · running` | Output received so far, exact elapsed if available |
| Completed successfully | `[x] Shell  <command> · exit 0` | Exact output, duration and existing verdict fields |
| Failed | `[!] Shell  <command> · exit 1` | stderr/stdout and structured diagnostic; unknown exit is `failed`, not guessed numeric value |
| Cancelled tool event | `[-] Shell  <command> · cancelled` | Partial output remains |
| Turn cancelled, process unconfirmed | `[-] Turn cancelled` + `Last reported process state: running` | Keep session identity; no animated running badge in historical summary |
| Denied | `[|] Shell  <command> · not run` only when denial guarantees no execution | Exact reason and previous sandbox attempt remain accessible |
| Unparsed / unknown | `[?] <tool> · outcome unavailable` | Raw available payload; never green |

**Grouping algorithm:** within a user turn, group adjacent read/search/list operations until an assistant narration, edit, shell invocation, approval, failure, plan boundary or other semantic event interrupts them. Classify from tool name/schema, not substring guessing in arbitrary output. Existing unknown/MCP tools retain their registered name. A single read stays a read. Two or more compatible exploration calls may become `Explored repository · 3 reads · 2 searches`. Counts are event counts; do not call repeated reads “unique files” unless deduplicated by canonical target.

A shell command and subsequent `write_stdin` calls referencing the same **existing session ID** render as one command lifecycle. Poll count can be secondary expanded metadata. Keep polls in event order in details. Nonempty writes to stdin are significant and must remain explicitly visible in expanded history; never group different shell sessions together. A final poll result can update the compact session outcome. Do not reclassify an original failed command as successful because a different command later succeeded.

Compact routine successful output has zero payload rows. Active output shows up to 3 most recent received lines when those lines already exist, plus exact command/session identity; failure shows up to 3 diagnostic lines, then existing detail access. Expanded output retains full available text and current redactions/truncation markers. Never claim a truncated or redacted payload is complete. Preserve control-character sanitization, ANSI behavior, privacy redaction, large-output limits and tool-result parser semantics.

### Recovery semantics

“Recovered” is a visual scenario, not a new backend status. For unrelated commands, show `exit 1` followed by `exit 0`, leaving their relationship to the assistant's explanation. For a retry with existing explicit identity, show both attempts and latest confirmed outcome. An error may recede to a one-line red-marked record after completion, but must not disappear into the generic count. Uncertainty remains visible even when the turn itself ended normally.

## E. Plan

Use the latest existing plan within the current turn; show heading `Plan · c/n complete`, then ordered items. Active marker orange, active label bold primary; pending and completed secondary. No full green list. One row per item when it fits; wrapped text aligns after the 4-column marker-plus-gap. Avoid blank rows between items.

Existing step-associated tool metadata stays available. During work show at most one metadata line under the active step; completed-step metadata goes into expanded detail. This is display clipping with detail retention, not shortening stored command strings to misleading fragments like `python3 -m`.

Every plan update replaces the visible current plan, rather than adding a second complete checklist to the compact transcript. Expanded detail contains previous available updates in original order with their existing metadata. The baseline's pinned summary remains, but only while the active plan is offscreen; a completed plan does not remain pinned above unrelated later turns. `/plan` remains the existing inspection route and must use the same labels and padding.

The tool supports pending, in_progress and completed. **Do not add failed or skipped to its schema.** If a task fails, leave its last reported plan state and show failure at the turn/tool level. If a task is cancelled, keep the last reported plan state but remove its live animation in completed history. If a provider or existing future payload explicitly supports another state, the design-system vocabulary is reserved; this plan does not authorize new state creation.

## F. Narration and thought

Raw reasoning visibility is inherited unchanged. No automatic AI summarizer, classifier, new “finding” tool or rewritten model prompt. Existing thinking summaries may label the live reasoning phase. Empty `Thought` headings carry no durable value and may be omitted when there is no actual reasoning content or duration to inspect.

During a turn, nonfinal assistant narration stays secondary. On completion, routine intermediate messages share the activity detail area. **All** such messages use the same rule; never guess which “I'll…” sentence is unimportant. Final assistant response remains expanded. Failure/approval/uncertainty structured events remain compact-visible independent of narration. If a final response contains an explicit uncertainty paragraph, render it normally; do not extract or relabel sentences as verified facts.

## G. Approval, questions and interruption

Approval is the existing inline focus block, not a new modal workflow. Order: title/state; exact action; exact reason; cwd; existing risk metadata if present; choices; contextual hints. Risk is not computed by the renderer. The baseline outside-workspace fixture offered Run once, Don't run, Don't run and say why; preserve those options, their keys and their scope. Do not add “Always allow” to this filesystem prompt because another CLI has it. Source inspection confirms Forge already conditionally offers AllowPattern/AllowPatternAlways for eligible payloads and denied hosts (`app/approvals.rs::approval_menu_kinds`); preserve those existing options and their exact semantics whenever supplied.

At ordinary height the exact command and reason wrap, with 1 blank row before choices. At constrained height, keep title and choices visible, let the existing scrollable content region carry long context, and use its existing navigation. Do not clip the only reason or advertise a new shortcut to retrieve it. If current input routing cannot expose overflow content, that is a prerequisite presentation/input accommodation to resolve in the implementing slice; it cannot be silently accepted as an omission.

Composer remains visibly paused under the existing policy; no second WAIT badge. Approval focus is blue; warning rail is yellow. Existing state transitions restore the prior valid owner after acceptance/denial. Existing free-text denial reason and `ask_user_question` retain text entry, selection, multiple-choice and resume behavior; apply the same geometry without changing answers.

## H. Completion and historical scanning

Use neutral `Finished`, not a green success claim, for a completed model turn. Duration/tool/token/throughput/cache values appear only if already available, remain associated with their own turn, and wrap into secondary metadata when expanded. Compact completion line prioritizes duration and tool count; other existing values are retained in details and existing status surfaces. No data may be dropped merely because it is repetitive.

Final Markdown: sentence-case headings, one blank row before a section, no extra blank row after a heading; lists use a stable hanging indent; links keep existing target behavior; code retains existing language, copy/open behavior if any, and syntax spans. Preserve tables and all content. The UI does not generate a “verified” section from generic model completion.

Historical default is collapsed **activity**, not a collapsed answer. The request is never summarized to an invented title. Global Ctrl+O retains its existing scope; do not introduce individual accordion shortcuts. Expansion restores the exact available event sequence, including old plan versions and narration. If the existing reasoning visibility setting hides raw reasoning, Ctrl+O must not override that privacy/visibility policy.

### Scroll and cache invariants

Keep a stable `(turn identity, block identity, intra-block wrapped-line offset)` anchor when a group compacts, expands or resizes. Source reconciliation: `Message` has no stable ID; derive a presentation key from existing session ID plus user-message ordinal within the current transcript revision. Tool blocks use existing tool-call/session IDs. Preserve anchors across append-only revisions; on clear/resume/compaction use existing view reset semantics rather than inventing durable message IDs. If following the tail, follow the live answer. If reading history, preserve the anchor and do not jump to the newest event. Retain PageUp/PageDown and Ctrl+End behavior as observed. An expansion preference remains until the existing toggle changes it; no five-minute auto-collapse timer.

Terminal resize, theme changes, new chunks and tool state updates must invalidate the relevant layout/cache entries. Timer ticks must not reconstruct the entire event history or re-highlight settled code. Ratatui can still diff full frame buffers; the expensive document projection is what must be cached. Never trade correctness for a cache that leaves old “running” states behind.

## I. Boundaries and acceptance

No new command, tool, agent capability, Git operation, process control, task workflow, model behavior, persistence format or approval policy. Presentation may derive counts and joins from existing identifiers. If information is absent, use unknown/omit optional field; do not invent it. The later agent must verify these mappings against current source before coding and stop a slice if it requires a new capability outside this scope.

Acceptance: after the fixture edit, the compact completed view contains request, activity/plan count, full final answer and timing; after failure/recovery, both outcomes remain inspectable and failure remains compact-visible; after cancellation, no live animation remains for the cancelled turn and no unconfirmed process termination is claimed. In the three-minute polling case, one shell lifecycle replaces repeated poll rows in compact view. Expansion recovers every available call and its payload without new input bindings.

## Source reconciliation notes

The baseline already replaces superseded full plan cards with `PlanUpdated` summaries. Preserve and restyle those existing summaries; only reconstruct full prior checklist detail from original tool-call arguments already present in `messages`, never from missing data. `from_messages` currently accepts but ignores `_events`, so do not assume events already drive the presentation. Completion timing is ephemeral and only the newest summary is held in `banner_state`; associating it with its original turn is a view-state correction. Retain future observed summaries in per-session presentation state if needed; do not create journal fields or fabricate historical timings after resume. Restored turns may legitimately have no timing metadata.
