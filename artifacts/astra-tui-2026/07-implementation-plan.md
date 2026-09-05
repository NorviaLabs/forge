# Forge 2026 TUI implementation plan

Baseline: `dfa46a07aa43551395ba7cb6b4867ab2d8a9c324` (`forge 0.1.0-beta.9`). This plan changes presentation only. It adds no commands, tools, workflows, persistence fields, approval policy, or model behavior. References such as **AX D** point to sections in `04-agent-experience.md`; **CS Chat** points to components in `05-component-specs.md`; prototype numbers refer to the 30-state prototype.

## Phase 0 — Foundations

### DESIGN-001 — Semantic palette and presentation constants

- **Goal:** Make every renderer consume one restrained semantic vocabulary.
- **References:** design system “Semantic tokens”, “Cell metrics”, “Glyph vocabulary”; CS all components; prototype all states.
- **Files/modules:** `crates/forge-config/src/theme.rs`; `crates/forge-tui/themes/forge-dark.toml`; `forge-light.toml`; `crates/forge-tui/src/theme.rs`; a small constants module under `crates/forge-tui/src/` if needed.
- **Exact behavior:** Add optional `activity` to custom-theme parsing with `warning` fallback; remap built-in colors exactly to the token table; expose named style helpers and shared cell constants. Keep syntax fields, live preview, system theme, theme persistence, and drop-in compatibility.
- **Acceptance criteria:** Old custom themes without `activity` load; both built-ins resolve all tokens; no renderer hardcodes the new RGB values.
- **Visual acceptance:** Blue means focus/selection, orange only active work, green only confirmed success, yellow warning/wait, red failure; most cells remain neutral.
- **Regression risks:** custom-theme deserialization; hue-separation tests; contrast in light mode.
- **Dependencies:** none. **Complexity:** M. **Independently mergeable:** yes.

### DESIGN-002 — Shared state markers and text roles

- **Goal:** Replace inconsistent lifecycle symbols with the ASCII state grammar without changing Git or terminal content.
- **References:** design system “Typography” and “Glyph vocabulary”; AX D/E.
- **Files/modules:** `crates/forge-tui/src/status_glyph.rs`; `theme.rs`; tool/plan render call sites; relevant unit tests.
- **Exact behavior:** Provide `[ ] [>] [x] [!] [-] [?] [|]` helpers with glyph-plus-text fallbacks and typed labels `Read/Search/Shell/Git/Edit/Check/Web/Plan`. Preserve existing Git status semantics and raw terminal ANSI.
- **Acceptance criteria:** every agent lifecycle state remains understandable in monochrome; plan rendering only accepts its existing three states.
- **Visual acceptance:** every marker occupies three cells in supported fonts; no emoji/Nerd Font dependency or mixed checkmark styles.
- **Regression risks:** width accounting and snapshots.
- **Dependencies:** DESIGN-001. **Complexity:** S. **Independently mergeable:** yes.

## Phase 1 — Frame, focus, and responsive geometry

### DESIGN-003 — Deterministic application layout

- **Goal:** Implement the specified pane widths and aligned bottoms at all supported sizes.
- **References:** design system “Responsive geometry”; CS Application frame; prototypes 28–30.
- **Files/modules:** `crates/forge-tui/src/layout.rs`; `crates/forge-tui/src/app/render.rs`; `widgets/bottom_panel.rs`; layout and visual tests.
- **Exact behavior:** Replace the 95% content inset with one-column frame gutters; retain the effective 116-column Files gate; apply the Files/workspace/chat table and 44/32/88 constraints; dock the existing terminal below the shared body; reserve composer and footer before allocating terminal height.
- **Acceptance criteria:** exact region arithmetic matches the tables at 80×24, 100×30, 120×35, 160×45, and 220×55; no focus-triggered resize; no empty editor allocation when none is open.
- **Visual acceptance:** adjacent boundaries are single-cell; body bottoms, editor status, composer, terminal and footer align; composer remains visible at 80×24.
- **Regression risks:** mouse hit regions, narrow-height underflow, Files threshold behavior, terminal PTY resize.
- **Dependencies:** DESIGN-001. **Complexity:** L. **Independently mergeable:** yes.

### DESIGN-004 — Pane chrome and effective focus

- **Goal:** Make the actual keyboard owner unmistakable while removing decorative boxes.
- **References:** design principles 3–6; border audit; CS Application frame, Files, Editor, Terminal, Chat.
- **Files/modules:** `app/render.rs`; `widgets/panel.rs`; `file_explorer.rs`; `editor.rs`; `conversation.rs`; `interactive_terminal.rs`; focus tests.
- **Exact behavior:** focused pane title is blue bold with `>`; inactive titles are neutral; modals suppress underlying focus markers; use shared separators and title rows instead of outer pane boxes. Preserve `FocusState`, input routing, and all focus traversal.
- **Acceptance criteria:** exactly one effective focus marker whenever input is accepted; closing an overlay restores the previous valid owner.
- **Visual acceptance:** focus is identifiable without color from the `>` marker; inactive panes have no painted caret or blue border.
- **Regression risks:** focus enum/display-name mismatch, modal restoration, compact layouts.
- **Dependencies:** DESIGN-003. **Complexity:** M. **Independently mergeable:** yes.

## Phase 2 — Conversation projection and streaming

### DESIGN-005 — Stable per-turn presentation projection

- **Goal:** Associate each request, activity, answer, and completion metadata with the correct turn.
- **References:** AX B/H “Scroll and cache invariants”; CS Agent turn; prototypes 15–18.
- **Files/modules:** `crates/forge-transcript/src/lib.rs`; `crates/forge-tui/src/app/turn.rs`; `app/render.rs`; `app/types.rs`; `conversation.rs`; transcript/TUI tests.
- **Exact behavior:** derive view-only turn keys from session identity plus user-message ordinal; project turn blocks without changing stored messages; replace the single global `ChatItem::TurnSummary` ownership with per-turn presentation state for the current session. Restored turns may omit ephemeral timing. Preserve the global Ctrl+O behavior.
- **Acceptance criteria:** a newer live action never inherits an older `Finished`; multiple completed turns retain their own available answer/activity association; resume needs no new journal schema.
- **Visual acceptance:** each completion line sits directly under its answer; historical activity is quiet while answers remain expanded.
- **Regression risks:** transcript revisions, clear/resume, task switching, cache keys, scroll anchors. This is the largest-risk slice.
- **Dependencies:** DESIGN-001/002. **Complexity:** L. **Independently mergeable:** no; merge with DESIGN-006 if intermediate projection states cannot be hidden safely.

### DESIGN-006 — Active turn live row and streaming answer

- **Goal:** Give a long-running turn one stable, truthful locus of motion.
- **References:** AX B/C; CS Chat, Agent turn, notification/status; prototypes 4–9 and 14.
- **Files/modules:** `widgets/turn_line.rs`; `app/render.rs`; `conversation.rs`; render cache code and tests.
- **Exact behavior:** render one `[>] <evidence-backed phase> · elapsed` row above composer; remove the character counter and duplicate footer busy state; append the request once; stream the final answer in its final location; on completion remove motion and freeze available duration.
- **Acceptance criteria:** the row never claims a tool phase without an event; timer ticks invalidate chrome rather than settled Markdown; cancellation/completion leaves no active indicator.
- **Visual acceptance:** active work is orange and locally dominant; transcript does not gain one row per poll or timer tick.
- **Regression risks:** busy debounce, streaming cache invalidation, queued prompts, follow-tail behavior.
- **Dependencies:** DESIGN-005. **Complexity:** M. **Independently mergeable:** no.

### DESIGN-007 — Historical compaction and scroll anchoring

- **Goal:** Recede routine activity while preserving exact available detail and reading position.
- **References:** AX H and scroll invariants; CS Chat/Agent turn; prototypes 16–18.
- **Files/modules:** `conversation.rs`; `app/render.rs`; transcript projection; conversation scroll/cache tests.
- **Exact behavior:** compact completed routine activity into one neutral count row; keep failures, denials, unknowns and final answers visible; Ctrl+O restores available events in order subject to reasoning policy. Anchor by turn/block/intra-block line when resizing or toggling; follow tail only when already following.
- **Acceptance criteria:** expansion loses no nonempty message or tool payload already available; resizing while reading history does not jump to the newest turn.
- **Visual acceptance:** old answers dominate their turns; no residual animation or green wash five minutes later.
- **Regression risks:** wrap-height changes, cache invalidation, large transcripts.
- **Dependencies:** DESIGN-005/006. **Complexity:** L. **Independently mergeable:** no.

## Phase 3 — Tools, plans, failures, and completion

### DESIGN-008 — Truthful individual tool rows

- **Goal:** Render queued/running/completed/failed/cancelled/denied/unknown outcomes from structured evidence.
- **References:** AX D; CS Tool row, Error; prototypes 7, 11–13.
- **Files/modules:** `crates/forge-transcript/src/lib.rs`; tool formatting modules under `crates/forge-tui/src/`; `conversation.rs`; parser and render tests.
- **Exact behavior:** use 3-cell state marker, kind, exact target, session/outcome metadata; show exit codes when present; preserve up to three active/failure lines compactly and all current detail through Ctrl+O. Do not infer success from turn completion.
- **Acceptance criteria:** exit 1 stays failed after later exit 0; denied means “not run” only when execution was prevented; unknown payload never renders green.
- **Visual acceptance:** failed target remains primary with one red marker; routine completed output consumes zero compact payload rows.
- **Regression risks:** provider payload variants, truncation/redaction, long command wrapping.
- **Dependencies:** DESIGN-002/005. **Complexity:** M. **Independently mergeable:** yes after DESIGN-005.

### DESIGN-009 — Shell-session coalescing and activity groups

- **Goal:** Turn command polling and adjacent exploration into legible activity, using existing identities.
- **References:** AX D grouping algorithm; CS Tool group; prototypes 8–9.
- **Files/modules:** `crates/forge-transcript/src/lib.rs`; conversation projection/rendering; tool-call parsers and tests.
- **Exact behavior:** join `exec_command` and `write_stdin` events only when their existing session ID matches; retain nonempty stdin writes and every event in expanded order; group adjacent reads/search/list calls until an explicit semantic boundary. Counts are calls unless canonical targets are actually deduplicated.
- **Acceptance criteria:** three-minute polling produces one compact shell lifecycle; separate shell sessions never merge; narration/edit/approval/failure/plan boundaries split groups.
- **Visual acceptance:** active call remains visible under a quiet group summary; only the group carries the detail hint.
- **Regression risks:** malformed JSON, missing session IDs, interleaved tools, streaming partial output.
- **Dependencies:** DESIGN-005/008. **Complexity:** L. **Independently mergeable:** yes after dependencies.

### DESIGN-010 — Plan hierarchy and update history

- **Goal:** Make the active step dominant and completed steps quiet without inventing plan states.
- **References:** AX E; CS Plan; prototypes 6 and 15.
- **Files/modules:** `crates/forge-transcript/src/lib.rs`; plan widgets/renderers; `app/render.rs`; plan tests.
- **Exact behavior:** latest plan renders `Plan · c/n complete`; active step orange/bold, completed neutral, pending secondary; one active metadata line; preserve existing `PlanUpdated` summaries and prior available arguments in details; show pinned summary only while active plan is offscreen.
- **Acceptance criteria:** only pending/in_progress/completed are consumed; `/plan` uses identical labels and spacing; completed plan does not pin over a later turn.
- **Visual acceptance:** no large green checklist; wrapped labels align after the marker.
- **Regression risks:** current plan supersession logic, metadata truncation, offscreen detection.
- **Dependencies:** DESIGN-002/005. **Complexity:** M. **Independently mergeable:** yes after DESIGN-005.

### DESIGN-011 — Final answer and completion hierarchy

- **Goal:** Make the final response the durable visual center of a completed turn.
- **References:** AX H; CS Agent turn/Chat; prototypes 14–17.
- **Files/modules:** `conversation.rs`; Markdown/render helpers; `app/turn.rs`; cache tests.
- **Exact behavior:** retain exact final Markdown; apply sentence-case heading/list/code/reference roles without rewriting text; render neutral `Finished` with only existing duration/tool/token fields; keep additional metadata in details. Never manufacture changes, verification, or success claims.
- **Acceptance criteria:** tables/code/links/content remain intact; completion metadata belongs to the correct turn; restored turns tolerate absent timings.
- **Visual acceptance:** answer uses primary text and more visual weight than activity; completion is one secondary line.
- **Regression risks:** Markdown layout, code highlighting, long links, cache widths.
- **Dependencies:** DESIGN-005/007/008. **Complexity:** M. **Independently mergeable:** no.

## Phase 4 — Composer, status, approvals, and questions

### DESIGN-012 — Composer and footer geometry

- **Goal:** Align input and status chrome while preserving every input mode and binding.
- **References:** design cell metrics; CS Composer/Footer/Model status; prototypes 1–5, 19–22.
- **Files/modules:** `widgets/input.rs`; `widgets/footer.rs`; `app/render.rs`; input/footer tests.
- **Exact behavior:** composer uses one top rule, one-column padding, 1–10 visual rows within the height formula; focused title blue `>`; working and approval states use existing editability rules; footer is one row with model/provider/effort and contextual keys, deduplicated by current priority.
- **Acceptance criteria:** multiline, placeholder, queueing, submit/newline, paste and key routing remain unchanged; no duplicate busy/approval status.
- **Visual acceptance:** composer text aligns with transcript; footer never requires a decorative separator; narrow screens preserve essential model and action hint.
- **Regression risks:** Unicode width, textarea cursor, paste burst, footer truncation.
- **Dependencies:** DESIGN-003/004/006. **Complexity:** M. **Independently mergeable:** yes after dependencies.

### DESIGN-013 — Approval and user-question presentation

- **Goal:** Present existing decisions with complete context and clear input ownership.
- **References:** AX G; CS Approval and generic modal; prototype 10.
- **Files/modules:** `crates/forge-tui/src/app/approvals.rs`; `app/overlays.rs`; approval/question widgets and tests.
- **Exact behavior:** preserve exact command/action, reason, cwd, supplied risk, all existing conditional menu kinds, denial text and question answer behavior; use blue focus title plus yellow waiting rail; ensure long context scrolls while title and choices remain reachable.
- **Acceptance criteria:** action mapping and approval scope are byte-for-byte unchanged; accepting/denying restores a valid focus owner; 80×24 exposes the reason through existing navigation.
- **Visual acceptance:** choices dominate; background live motion freezes; warning color never substitutes for focus.
- **Regression risks:** approval key routing, denied-host variants, free-text modes, overflow.
- **Dependencies:** DESIGN-001/004/012. **Complexity:** M. **Independently mergeable:** yes.

## Phase 5 — Existing selectors and dialogs

### DESIGN-014 — Common overlay family

- **Goal:** Give help, command palette, theme, provider/connect, confirmations and conflict dialogs one geometry and focus grammar.
- **References:** design modal geometry; CS Help, Command palette, Theme selector, Provider/connect, Generic modal, Save/Discard/Cancel; prototypes 25 and 27.
- **Files/modules:** `crates/forge-tui/src/overlays.rs`; `app/overlays.rs`; shared modal widgets; overlay tests.
- **Exact behavior:** one border, two-column horizontal padding, content-driven height, shared selected row and hint placement; preserve every current datum, action, binding, theme bottom-dock behavior, preview and cancel restoration.
- **Acceptance criteria:** all existing options remain present and actionable; overlay close restores focus; overflow scrolls at minimum size.
- **Visual acceptance:** no nested boxes or double separators; selected row uses neutral fill plus blue pointer.
- **Regression risks:** large monolithic overlay renderer, per-dialog input assumptions, compact height.
- **Dependencies:** DESIGN-001/004. **Complexity:** L. **Independently mergeable:** yes.

### DESIGN-015 — Model picker information layout

- **Goal:** Preserve its strong information while fixing alignment and duplication noise.
- **References:** design modal geometry; CS Model selector; prototype 24.
- **Files/modules:** `overlays.rs`; model-picker state/filter code; snapshot/render tests.
- **Exact behavior:** pointer + flexible model + 20-column provider + 15-column source/account; filter row and column labels share origins; current selection remains marked; selected route detail wraps below before identities are clipped. Reorder/group only existing fields.
- **Acceptance criteria:** filtering, route distinction, current selection, provider/account/source and footer hints all remain; duplicated IDs remain distinguishable.
- **Visual acceptance:** columns align at 120×35; at 80×24 the selected detail is readable and actions remain visible.
- **Regression risks:** wide Unicode model names, route labels, filter selection retention.
- **Dependencies:** DESIGN-014. **Complexity:** M. **Independently mergeable:** yes.

## Phase 6 — Files, editor, terminal, and review

### DESIGN-016 — Files tree and one-row search

- **Goal:** Recover vertical space and improve selection/search clarity.
- **References:** CS Files/Files search/Tree; prototype 23.
- **Files/modules:** `crates/forge-tui/src/file_explorer.rs`; workspace navigation code; tests.
- **Exact behavior:** replace the three-row search box and separator with one surface row; use `>`/`v`, 2-column indentation, neutral selected row plus pointer, highlighted matches; preserve filtering, nested navigation, Git status, empty/no-result behavior and bindings.
- **Acceptance criteria:** query and results update exactly as before; no-result is distinct from empty repository; selected nested paths remain visible within available width.
- **Visual acceptance:** search consumes one row; tree begins immediately below; inactive selection loses blue but remains locatable.
- **Regression risks:** constants baked into hit testing, scroll offsets, path truncation.
- **Dependencies:** DESIGN-003/004. **Complexity:** M. **Independently mergeable:** yes.

### DESIGN-017 — Editor, terminal, diff, and conflict chrome

- **Goal:** Apply one focus/state language to the existing workspace surfaces.
- **References:** CS Editor, Editor status row, Terminal, Review Changes, Save/Discard/Cancel; prototypes 19–22, 26–27.
- **Files/modules:** `editor.rs`; `editor_session.rs`; `interactive_terminal.rs`; `widgets/bottom_panel.rs`; `diff_view.rs`; `app/diff.rs`; `app/workspace.rs`; relevant tests.
- **Exact behavior:** editor title shows exact file and `*`; NORMAL/INSERT row aligns to composer baseline; terminal keeps its output and one top rule; Review Changes keeps file list/diff/add-remove counts with clearer selected file; shared modal styling covers unsaved/external conflict. No editing, shell, or Git semantics change.
- **Acceptance criteria:** mode/cursor/dirty state, terminal focus and lifetime, diff navigation, review actions and conflict choices behave unchanged; terminal receives correct resize.
- **Visual acceptance:** workspace focus is evident at all widths; chat never falls below 44 columns; diff syntax and +/- remain legible without full-pane success/error tint.
- **Regression risks:** terminal emulator clipping, editor cursor painting, diff scroll, dirty/conflict state.
- **Dependencies:** DESIGN-003/004/014. **Complexity:** L. **Independently mergeable:** yes.

## Phase 7 — Consistency and release gate

### DESIGN-018 — Responsive, accessibility, performance, and consistency pass

- **Goal:** Prove the redesign as one system across every current workflow.
- **References:** all specs; `09-validation-plan.md`; prototype all states.
- **Files/modules:** TUI visual/render/performance tests including `tests/render_perf.rs`, `render_inspect.rs`, `visual_test.rs`; `FORGE-DESIGN.md` after implementation is accepted.
- **Exact behavior:** add representative buffer fixtures at specified sizes/themes and state transitions; audit every displayed binding against active input handling; profile long transcript and timer rendering; update the runtime design contract to the implemented tokens and invariants.
- **Acceptance criteria:** validation-plan invariant and regression matrices pass; `cargo fmt`, Clippy, workspace tests, release build and version run pass.
- **Visual acceptance:** no double borders, clipped sole actions, stale live states, ambiguous focus, disappearing information, or new product concepts.
- **Regression risks:** platform terminal differences and brittle snapshots; assert semantic regions and critical text in addition to selective golden images.
- **Dependencies:** DESIGN-001–017. **Complexity:** L. **Independently mergeable:** no; final integration gate.

## Recommended merge sequence

Merge 001–004 first. Land 005–007 as a tightly reviewed transcript foundation, then 008–011 so tool and plan semantics consume that projection. Composer/approval work (012–013), overlays (014–015), and workspace surfaces (016–017) can proceed in parallel after foundations. Finish with 018. Keep each PR behavior-neutral outside its named slice, and attach before/after PTY captures at 80×24, 120×35, and 160×45.
