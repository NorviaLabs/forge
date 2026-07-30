# Forge V3.1 Redesign Audit

Date: 2026-07-30

Authoritative references:

- `docs/forge-v31-redesign-prompts/FORGE-V3.1-INTERACTION-CONTRACT.md`
- `docs/forge-design-kit/FORGE-DESIGN.md`
- `docs/architecture.md`
- `docs/ui.md`
- `docs/alpha-testing.md`

Scope note: this audit describes the current checkout. The tree already contained in-progress File Explorer filesystem-operation changes before this audit phase; those are treated as current local state and are not redesigned here.

Final status note: this document is the Prompt 00 baseline audit. Later V3.1 phases replaced the permanent Chat/Editor/Diff tab model with `WorkspaceView`, made Files visibility repository-scoped and independent, added scoped approval, contextual supporting surfaces, command-driven mouse input, safe double-click activation, and edge-state recovery. Use `docs/forge-v31-redesign-prompts/FORGE-V3.1-INTERACTION-CONTRACT.md` and `docs/forge-design-kit/FORGE-DESIGN.md` for the current contract.

## 1. Current State Model

The current TUI state is centralized in `TuiApp` (`crates/forge-tui/src/app.rs`). It is a single application model that owns session, layout visibility, keyboard focus, workspace tab selection, overlays, file tree, source viewer, run state, activity, and transient UI banners.

Primary state owners:

- `TuiApp` (`app.rs`): application shell, event loop state, focus, overlays, workspace mode, run lifecycle, connection state, slash dispatch, file watcher, terminal capture, and UI notices.
- `WorkspaceMode` (`app.rs`): current center workspace tab: `Chat`, `Editor`, or `Diff`.
- `FocusState` (`app.rs`): current keyboard owner using `FocusBlock` plus `FocusMode`; this is the current implementation equivalent of the future contract's focus target semantics.
- `InputModel` (`widgets/input.rs`): Composer text, cursor, paste placeholders, history browsing marker, hint state, and connection-warning presentation.
- `ConversationRenderCache` (`app.rs`): cached rendered chat lines keyed by conversation/session rendering inputs.
- `FileExplorer` (`file_explorer.rs`): root node, selected path, scroll, icon mode, focused flag, root path, and git-status cache.
- `SourceViewer` (`source_viewer.rs`): open file path, relative path, lines, viewport, search, jump-to-line, highlighting, file status, and notice.
- `GitStatusCache` (`git_status.rs`): asynchronous git status snapshot and diff helpers used by Files, Diff, and Inspector.
- `SidebarModel` and `InspectorView` (`sidebar.rs`): Inspector tab projection for Task, Context, and Runtime.
- `BottomPanelState` (`widgets/bottom_panel.rs`): open/closed, active bottom tab, and focused mirror flag.
- `RunStateModel` (`run.rs`): run draft, current run record, recent records, legacy validation command, editing flags, and run parse errors.
- `Overlay` (`overlays.rs`): modal/palette state for help, HITL, slash palette, model picker, connect prompts, resume picker, readonly file picker/viewer, status reports, and turn-limit prompts.
- `TerminalCapture` (`app.rs`): captured process output shown by the Bottom Panel Terminal tab.
- `AgentSession` (`forge-core/src/lib.rs`): durable session state, messages, events, pending HITL, token usage, active model, workspace root, and context lifecycle.

Current persisted or repository-scoped state:

- Durable session journal is owned by `forge-core` / durable storage and restored through `AgentSession::resume` and `TuiApp::dispatch_line("/resume ...")`.
- Provider credentials and last provider/model/effort selections are owned by `forge-connect` store calls in `TuiApp`.
- Run history is persisted under `.forge/run-history.json` by `TuiApp::load_run_history` and `TuiApp::save_run_history`.
- Files visibility, current workspace tab, Inspector tab, Bottom Panel open state, and focus are not persisted per repository in the current implementation.

Current mismatch with V3.1:

- V3.1 defines one dominant contextual workspace with views `Conversation`, `File(path)`, `Diff(context)`, and `Run(run_id)`.
- Current shipped state uses permanent `Chat`, `Editor`, and `Diff` workspace tabs plus an auxiliary Run bottom panel.
- V3.1 requires Files visibility remembered per repository; current `files_visible` is in-memory only.

## 2. Current Event-Routing Path

The event path is centralized in `TuiApp::handle_key` (`app.rs`).

Current keyboard routing order:

1. Ignore non-press events except arrow-key repeats for selection UIs.
2. Explorer filesystem dialog, if present in the current checkout, owns input before other overlays.
3. `Overlay::StatusReport` gets special handling so Enter closes and printable input can resume Composer.
4. Any active `Overlay` routes through `handle_overlay_key` (`overlays.rs`) and returns an `OverlayAction`.
5. `FocusMode::Transient(SourceSearch)` routes to `handle_search_key`.
6. `FocusMode::Transient(JumpToLine)` routes to `handle_jump_key`.
7. `FocusBlock::Composer` routes to `handle_chat_composer_key`.
8. Tab / Shift+Tab cycle visible focus blocks before active-block handling.
9. `handle_active_block_key` dispatches to Files, Workspace, Inspector, or Bottom Panel handlers.
10. `handle_global_key` handles global shortcuts such as Alt workspace tabs, Ctrl+K palette, Ctrl+E files, Ctrl+B inspector, Ctrl+P bottom panel, queue controls, and Ctrl+C/D quit.
11. `type_to_compose` catches otherwise unhandled printable input and moves focus to Composer with the first character preserved.

Paste routing:

- `TuiApp::handle_paste` sends paste to the active explorer name field, active overlay, Composer, source search, jump-to-line, or no-op depending on current owner.

Slash command path:

- Composer Enter calls `dispatch_line`.
- `dispatch_line` calls `parse_slash` (`commands.rs`).
- Slash commands either mutate local UI state, open overlays, queue background work, or dispatch chat to the model.
- Slash commands are Composer-local. Ctrl+K opens `Overlay::Slash`, which is currently a command palette but still backed by slash command strings.

Mouse path:

- `TuiApp::handle_mouse` currently only handles `MouseEventKind::ScrollUp` and `ScrollDown`, and always scrolls the conversation.
- There is no current click-to-focus, row selection, double-click activation, wheel-under-pointer routing, or mouse approval action handling.

Async routing:

- File watcher events enter `file_change_rx`, are drained by `poll_file_changes`, and call `refresh_after_filesystem_change`.
- Run process output enters `run_rx`, is drained by `poll_run`, and updates `TerminalCapture` / `RunStateModel`.
- OAuth polling is timer-driven through `poll_oauth_tick`.
- Agent/model events are drained in the main event loop and update messages, events, stream preview, status, and activity.

## 3. Current Layout Ownership

Layout is owned by `TuiApp::draw` and helpers in `layout.rs`.

Key layout functions:

- `split_areas_with_side_panels` (`layout.rs`): computes status, files, workspace, inspector, feedback, input, footer, and bottom panel rectangles.
- `is_too_small` (`layout.rs`): emergency guard for terminals below current minimum height or very narrow width.
- `render_workspace_tabs` (`app.rs`): renders permanent `Chat Editor Diff` tab strip.
- `render_diff_workspace` (`app.rs`): renders current Diff tab directly from `GitStatusCache`.
- `FileExplorerWidget`, `SourceViewerWidget`, `SidebarWidget`, `BottomPanel`, `InputBar`, `StatusBar`, `FooterBar`, and `FeedbackBar`: render component-local content into rectangles selected by `TuiApp::draw`.

Current visual hierarchy:

- Status bar at top.
- Optional Files pane on left.
- Workspace tab row plus Chat, Editor, or Diff content in center.
- Optional Inspector on right.
- Optional Bottom Panel below workspace.
- Feedback strip and Composer near bottom.
- Footer at bottom.
- Overlays render last and visually supersede all blocks.

Current responsive behavior:

- Side panels are conditional based on width and visibility booleans.
- Focus is normalized away from hidden blocks after layout calculation.
- At very small sizes, the app renders a terminal-too-small message.
- `80x24` is expected to render without panic and is covered by characterization.

Current mismatch with V3.1:

- V3.1 removes permanent Chat/Editor/Diff tabs and permanent shortcut footer.
- Current layout uses a permanent tab strip and footer hints.

## 4. Current Navigation Behaviour

Workspace navigation:

- `Shift+Right` and `Shift+Left` switch active tabs in the focused block.
- `WorkspaceMode::next` / `previous` cycles Chat -> Editor -> Diff.
- `select_workspace_tab` focuses Workspace when selecting Editor or Diff.
- Opening a file from Files calls `open_file_in_editor`, switches to `WorkspaceMode::Editor`, focuses Workspace, and selects that file in Files.
- Diff navigation uses Up/Down in Workspace focus when `workspace_mode == Diff`.

Block navigation:

- `Tab` and `BackTab` cycle visible focus blocks using `FocusBlock::ORDER`.
- Hidden Files, Inspector, and Bottom Panel cannot keep focus after normalization.
- Closing a panel attempts to restore previous valid focus through `restore_focus_after_closing`.

Composer navigation:

- Composer Enter submits, Shift+Enter or Alt+Enter inserts newline.
- Slash suggestions are navigated with Up/Down while input starts with `/`.
- Escape from Composer returns to previous block and preserves draft text.

Files navigation:

- Up/Down move selection.
- Right expands selected directory.
- Left collapses selected directory or moves to parent.
- Enter opens selected file/symlink in Editor or toggles directory expansion.
- `r` refreshes selected directory.

Editor navigation:

- Plain arrows and `h/j/k/l` move viewport/cursor.
- `g` and `G` move to first/last line.
- `r` refreshes the source viewer and git status.
- `Ctrl+F` starts source search.
- `Ctrl+G` starts jump-to-line.
- `e` queues external editor open.
- `Ctrl+A` toggles file attachment.

Bottom Panel navigation:

- Ctrl+P opens/closes the panel.
- Shift+Left/Right cycle bottom tabs.
- Enter in Run tab starts/cancels current run.
- `r`, `e`, `m`, `i`, and `d` operate on Run tab state.
- Escape restores previous focus.

Inspector navigation:

- Shift+Left/Right cycle Inspector tabs.
- Alt+[ and Alt+] are also supported as global inspector shortcuts.

Current mismatch with V3.1:

- There is no `Back`, `Home`, `PushView`, or `ReplaceView` stack.
- Current Run is a bottom panel, not a workspace view.
- Background activity can update status/activity but does not intentionally navigate; this aligns with V3.1 and should be preserved.

## 5. Current Focus Precedence

Current effective owner is `FocusState { block, mode }`.

Focus blocks:

- `Files`
- `Workspace`
- `Composer`
- `Inspector`
- `BottomPanel`

Focus modes:

- `Navigation`
- `Transient(SourceSearch)`
- `Transient(JumpToLine)`

Precedence:

1. Modal/overlay/dialog input.
2. Source search and jump transient modes.
3. Focused block.
4. Global shortcuts.
5. Type-to-compose fallback.

Important invariants already represented:

- Exactly one effective keyboard owner is intended at a time.
- Transient input wins over background panes.
- Modal overlay wins over block navigation.
- Handled events return early and do not fall through to Composer.
- Type-to-compose only receives otherwise unhandled printable input.
- Hidden blocks are invalid focus targets.

Current risk:

- The future contract prohibits bare printable global bindings because they conflict with type-to-compose. Current implementation has several plain printable local bindings, but only under active block focus. Those must remain block-local or move behind semantic command routing in phase 01.

## 6. State Duplicated Across Multiple Surfaces

Known duplicated or projected state:

- Git status appears in Files markers, Diff tab, Inspector summary, status/header dirty state, and sync flow.
- Active file path appears in `SourceViewer`, Files selected path, pending file attachment, and external editor request.
- Provider/model connection appears in `TuiApp.runtime`, `connect_profile`, `AgentSession.active_model`, status bar, footer, input hint, and connect overlays.
- Busy/session status appears in `AgentSession.status`, `TuiApp.busy`, `BusyPhase`, status bar, feedback strip, activity feed, and chat stream rendering.
- Run state appears in `RunStateModel`, Bottom Panel Run tab, Bottom Panel Terminal tab, activity feed, status/busy phase, and `.forge/run-history.json`.
- Error information appears in feedback strip, chat banners, notices, activity, overlay-local error fields, and component-local error states.
- Focus is canonical in `FocusState` but mirrored into `file_explorer.focused`, `bottom_panel.focused`, and `source_viewer.focused` for rendering.
- File tree root/workspace root is stored in `AgentSession`, `TuiRuntimeConfig.cwd`, `FileExplorer.root_path`, and `RunStateModel` working directory.

Migration implication:

- Phase 01 should introduce semantic commands without creating a second source of truth.
- Phase 02 should centralize contextual workspace navigation before removing tabs.
- Phase 04 must carefully define which async updates can refresh data and which cannot issue navigation commands.

## 7. Components That Can Be Reused

Reusable without major semantic change:

- `InputModel` editing operations and paste placeholder logic.
- `ConversationModel` and `ConversationLinesWidget` rendering model.
- `SourceViewer` file loading, status states, search, jump-to-line, highlighting, viewport logic.
- `FileExplorer` tree model, sorting, expansion, visible-node flattening, selection, git decoration, and safe readonly path loading.
- `GitStatusCache` parsing, polling, changed-files projection, and diff helpers.
- `SidebarModel` line projection for Inspector content, though placement may change later.
- `BottomPanelState`, `RunStateModel`, and run parsing/execution model, though Run may become a workspace view.
- `Overlay` variants for HITL/connect/model/resume/status can be adapted into future overlay command handling.
- `TerminalGuard`, `restore_terminal`, `reinit_terminal`, and `clear_terminal`.
- Existing layout split helpers can remain as implementation detail until contextual workspace cutover.

## 8. Components Requiring Adapters

Adapters likely needed:

- `handle_key` / `handle_global_key` / active block handlers: should emit semantic commands in phase 01 rather than directly mutating view state.
- `WorkspaceMode`: should be adapted into future navigation state in phase 02 before tab removal.
- `open_file_in_editor`: currently directly replaces tab state; future `OpenFile(path)` should route through navigation state.
- `render_diff_workspace`: currently tied to permanent Diff tab; future `ReviewChanges(context)` should own Diff context explicitly.
- `open_bottom_panel` and Run key handling: future `OpenRun(run_id)` should expose Run as contextual workspace while preserving Bottom Panel support until cutover.
- Slash palette and Ctrl+K overlay: current Ctrl+K returns slash command strings; future global palette should emit semantic commands.
- Mouse handling: current scroll-only path needs target resolution and command emission in phases 08-09.
- HITL overlay actions: current overlay actions resolve HITL directly; phase 06 should adapt these to scoped approval commands without changing security semantics.

## 9. Components Likely To Be Removed Later

Likely later removals or reductions, not in this phase:

- Permanent `WorkspaceMode::Chat/Editor/Diff` tab strip.
- Full-screen Editor empty destination when no file is selected.
- Diff as a permanent workspace tab.
- Run as only a Bottom Panel interaction once `Run(run_id)` workspace view exists.
- Permanent footer shortcut strip if V3.1 contextual actions replace it.
- Readonly `/file` overlay may be redundant after contextual File view and Files controls mature.
- Ad hoc direct state mutations from individual key handlers after semantic command routing is complete.

## 10. Migration Risks And Dependencies

High-risk areas:

- Focus fallthrough: any missed `return Ok(())` can send printable input to Composer unexpectedly.
- Overlay precedence: approval, connect API key, slash/model palettes, file dialogs, search, and jump all depend on early routing.
- External editor lifecycle: terminal restoration is tested but currently has failing tests in this checkout; avoid touching without dedicated phase.
- Run lifecycle: `drain_pending_validation`, `poll_run`, terminal capture, run history, and Bottom Panel focus are coupled.
- Git status: shared by Files, Diff, Inspector, and sync; refresh semantics must avoid clearing file tree or duplicating stale paths.
- Session restoration: core restores durable conversation/session state, but UI state persistence is limited.
- Responsive layout: focus normalization depends on actual rendered regions, not just preference booleans.
- Mouse introduction: click targets must not bypass modal/approval blocking or type-to-compose rules.
- Approval scope: remembered approval safety belongs to governance/security and must not be generalized from UI convenience.
- Current checkout contains in-progress File Explorer mutation code; later phases must decide whether to keep, adapt, or isolate it before broad interaction refactors.

Known validation risk in current checkout:

- Full `forge-tui` tests currently fail in several pre-existing widget/terminal expectations unrelated to this audit's added characterization tests. These failures must be resolved before using full-suite green as a phase gate.

## 11. Proposed Phase-By-Phase Code Migration Map

Aligned with `docs/forge-v31-redesign-prompts/README.md`.

### 00 - Audit and Characterisation

- Create this audit.
- Add behavioral characterization tests only.
- Do not change visible behavior.
- Do not introduce `WorkspaceView`.

### 01 - Semantic Command Foundation

- Add a semantic command enum/module that maps current controls to existing mutations.
- Route keyboard, slash palette, and future mouse intents through the command layer.
- Preserve current `FocusState`, `WorkspaceMode`, and visible tabs during this phase.
- Add tests proving handled commands do not fall through to Composer.

### 02 - Workspace Navigation State

- Introduce view-stack/navigation state behind the existing `WorkspaceMode`.
- Implement `PushView`, `ReplaceView`, `Back`, `Home`, `OpenFile`, `ReviewChanges`, `OpenRun`, and `ToggleFiles` semantics.
- Keep old tabs visible while internally proving navigation state correctness.
- Add repository-scoped Files preference only if required by this phase's prompt; otherwise defer to phase 05.

### 03 - Contextual Workspace Cutover

- Replace visible Chat/Editor/Diff tab strip with contextual workspace rendering.
- Map current Chat to `Conversation`, current Editor to `File(path)`, and current Diff to `Diff(context)`.
- Remove empty full-screen Editor destination.
- Preserve source viewer, diff renderer, and conversation renderer internals.

### 04 - Async Activity And Non-Hijack

- Formalize async events as state updates that do not issue navigation commands.
- Ensure agent streaming, file watcher, git status, run start/success/failure, and model errors preserve user-controlled workspace.
- Consolidate activity summary priority.

### 05 - Files Visibility And Responsive Policy

- Make Files visibility independent of workspace view and remembered per repository.
- Implement V3.1 width-based temporary collapse/restore semantics.
- Add command palette and keyboard path for Files visibility.

### 06 - Scoped Approval Overlay

- Rework HITL overlay data and actions to match V3.1 approval scope.
- Ensure Shell-mode invocations are allow-once only.
- Preserve durable HITL resume and terminal safety.
- Block all underlying keyboard and future mouse targets.

### 07 - Supporting Surfaces And Chrome

- Reduce duplicated status/chrome and footer hint clutter.
- Adapt Inspector and Bottom Panel to contextual workspace model.
- Keep Run model and terminal capture reusable.

### 08 - Mouse Foundation

- Add target resolution for panes, rows, controls, and wheel-under-pointer scrolling.
- Mouse emits semantic commands, never direct state mutation.
- Maintain modal blocking and keyboard focus rules.

### 09 - Double-Click Activation

- Add double-click recognition only after stable mouse target identity exists.
- Support file open and folder toggle.
- Prevent double activation of explicit controls.

### 10 - Edge-State Transitions

- Implement V3.1 edge-state behavior for run cancel/failure/spawn failure, stale diff, unavailable file, repository changes, and approval lifecycle.
- Strengthen recovery and non-hijack tests.

### 11 - Final Shell Polish And Regression Gate

- Remove obsolete adapters and old tab code.
- Refresh visual tests/screenshots.
- Run full workspace validation and manual acceptance.
- Confirm no regression in terminal restoration, session resume, external editor, Run, Files, Composer, and approval.

## Characterisation Coverage Added In This Phase

Added tests in `crates/forge-tui/src/app.rs`:

- `characterization_workspace_tabs_are_reachable_with_current_controls`
- `characterization_files_selection_and_expansion_survive_focus_roundtrip`
- `characterization_80x24_draws_without_panic`
- `characterization_run_completion_preserves_bottom_panel_focus`

Existing relevant tests already covering required behavior include:

- Chat/Composer/type-to-compose: `chat_input_keeps_literal_brackets_and_shift_arrows_do_not_switch_tabs`, `type_to_compose_keeps_first_unbound_printable`, `non_printable_keys_do_not_type_to_compose`, slash autocomplete tests.
- Modal/transient precedence: `overlay_precedes_block_navigation`, source search and jump transient tests, connect API key modal tests.
- Files behavior: `file_change_does_not_reload_tree_while_files_sidebar_is_focused`, `workspace_refresh_reloads_tree_and_preserves_expanded_directories`, `refresh_selected_directory_reloads_only_that_directory`.
- Bottom Panel focus: `opening_and_closing_bottom_panel_transfers_focus`, `shift_arrow_tabs_only_apply_to_the_active_navigation_block`, bottom panel widget tests.
- External editor and terminal lifecycle: external editor precondition/resume tests and `terminal.rs` guard/restoration tests.
- Session restoration: core resume tests in `forge-core` and TUI `/resume` tests in `app.rs`.
- Responsive rendering: `resize_drops_focus_from_a_zero_width_files_block`, visual tests, and the added `80x24` characterization.

## Gaps Not Characterised In This Phase

- Current mouse behavior beyond conversation scrolling is intentionally minimal and not expanded.
- Full V3.1 `Back`, `Home`, and contextual view stack do not exist yet.
- Repository-scoped Files visibility persistence does not exist yet.
- External editor restoration has an existing failing test in this checkout and should be addressed before a final regression gate.
- Full approval scope semantics from V3.1 are not yet implemented.
- Current UI-state session restoration is limited; durable conversation/session state is restored, but focus/workspace/pane state is mostly process-local.
