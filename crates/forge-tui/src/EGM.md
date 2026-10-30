# forge-tui/src — Source Root

## Overview

The flat module root of the TUI crate. `lib.rs` declares every module, keeps all
of them private except four (`design`, `git_prefs`, `links`, `notify`), and
re-exports the supported surface. Two subdirectories split the code that has
internal structure: [`./app/EGM.md`](./app/EGM.md) (application state, event
loop, input routing) and [`./widgets/EGM.md`](./widgets/EGM.md) (pure render
models).

Modules fall into five groups: chrome and painting, transcript and content
rendering, editor and workspace panes, overlays/setup, and transport/utilities.

## Interface

Public modules:

- `design` — fixed 2026 design-system cell metrics (`FRAME_INSET_X`,
  `PANE_GAP_X`, `PANE_TITLE_H`, `COMPOSER_PAD_X`, `MAX_COMPOSER_INPUT_H`,
  `COMPACT_FRAME_H`, `HOME_MAX_WIDTH`, `NAVIGATOR_MIN_WIDTH`, `FOOTER_H`,
  `MODAL_PAD_X`, `MODAL_PAD_Y`, `MIN_FRAME_W`, `MIN_FRAME_H`, `AIRY_MIN_ROWS`,
  and peers). Metrics, not colors.
- `git_prefs` — process-wide git preferences installed from user config:
  `install(smart_commit)`, `smart_commit()`.
- `links` — terminal hyperlink policy: `TerminalHyperlink`, `HyperlinkLine`;
  decides which destinations may become clickable and emits OSC 8.
- `notify` — out-of-band notification: `Channels`, `channels_for`,
  `terminal_supports_osc9`, `sanitize`, `osc9_sequence`, `BELL`, `install`,
  `mode`, `set_focused`, `is_focused`, `term_program`, `notify`, `captured`,
  `take_captured`, `OSC9_TERMINALS`.

Crate-level re-exports (see `lib.rs`): `ActivityFeed`/`ActivityItem`/
`ActivityKind`, app entry points and `TuiApp` types, `parse_slash`/
`SlashCommand`/`CommandError`, `ConversationModel`/`ConversationRender`/
`ConversationViewOpts`/`ChatItem`/`BannerKind`/`StreamWaitPhase`/
`ToolCardState`, `ReasoningEffort`, `FileExplorer`/`FileExplorerWidget`,
`HistoryStore`/`InputHistory`/`MAX_INPUT_HISTORY`, `decide_launch`/
`LaunchDecision`, `is_too_small`, `split_areas`/`split_areas_ex`/
`split_areas_full`/`LayoutRegions`/`MIN_WIDTH`/`MIN_HEIGHT`, overlay types and
`handle_overlay_key`/`filter_palette`/`default_palette_items`/
`models_from_catalog`/`ResumeSessionItem`, `run_setup`/`SetupRequest`/
`SetupResult`, validation types, and the widget models.

Chrome and painting:

- `theme` — color tokens and style helpers (`text`, `dim`, `panel`, `border`,
  `activity`, `metadata_style`, `modal_title`, `pane_title`, `fill`,
  `canvas`, `strip_colors_if_disabled`).
- `theme_registry` — discovers and loads themes from bundled files plus
  optional directories.
- `theme_preview` — compact token preview used by the theme picker.
- `status_glyph` — state markers and text roles.
- `layout` — top-level splits and the enforced minimum frame.
- `overlay_layout` — geometry shared by modals and previews, including
  `centered_rect`.
- `hints` — one grammar for key hints on modals and prompts (`Hint` tables
  such as `PICKER`, `APPROVAL`, `MOVE_SELECT_CLOSE`).
- `path_display` — fits long text (paths above all) into fixed columns.
- `tasks_strip` — sidebar background-activity view-model.
- `user_message_gutter` — submitted user-message rendering.
- `activity` — bounded activity ring buffer.

Transcript and content rendering:

- `conversation` — projects `forge-transcript` output into ratatui lines.
- `markdown` — CommonMark rendering for assistant answers.
- `mermaid` — dependency-free flowchart renderer for terminal previews.
- `diff_view` — `/diff` as a mode of the Workspace pane.
- `source_viewer` — read-only source view plus Forge-owned editor chrome.
- `clipboard` — OSC 52 clipboard transport, no native dependencies.
- `clipboard_image` — reads images from the local OS clipboard.
- `selection` — mouse text selection and the right-click context menu.
- `status_report` — shared `/status` report data and rendering.
- `validation` — parses test output into `ValidationSnapshot`;
  `ValidationStatus`, `ValidationParseState`, `CargoTestSummary`,
  `parse_cargo_test_output`, `is_cargo_test_command`,
  `validation_command_text`, `MAX_FAILED_DISPLAY`.

Workspace and editor:

- `file_explorer` — `FileExplorer`, `FileExplorerWidget`, `FileNode`,
  `VisibleNode`, `FileKind`, `FileSearchMode`, and `safe_path` (workspace
  confinement check for explorer-driven paths).
- `editor` — external editor resolution and argument parsing.
- `editor_session` — Forge-owned state around the `edtui` core.
- `interactive_terminal` — PTY-backed shell/terminal pane (scrollback capped at
  1024 lines, 64-chunk output queue for backpressure).
- `terminal` — `TerminalGuard`: restores raw mode, alternate screen, cursor and
  keyboard flags on normal shutdown, returned errors and panics. Also
  `restore_terminal`, `reinit_terminal`, `clear_terminal`, and
  `render_kitty_image` (writes kitty graphics-protocol image data).

Overlays and setup:

- `overlays` — HITL, slash palette, model picker, session switcher,
  `OverlayWidget`, `Key`, `OverlayAction`. Connect flows include
  `Overlay::ConnectCustom` (custom provider form: id, base URL, model, API
  key, then a protocol row whose `wire: forge_connect::WireKind` toggles
  `ChatCompletions` / `AnthropicMessages`) and `Overlay::ConnectManualModel`
  (add a model id by hand). The same form edits a user profile or a built-in
  whose `editable` includes `BaseUrl`; while editing, the id and protocol rows
  are locked. `details: Option<[String; 3]>` adds Title, Label and Key env
  names rows (5-7) for a user profile; `None` hides them. The picker emits
  `OverlayAction::RefreshModels`, `AddManualModel`, `SaveManualModel`,
  `AddCustomProvider`, `SaveCustomProvider { .., wire, details }` and
  `EditProvider`, and `RemoveProvider` (`Ctrl-X`). `Overlay::picker_route`
  names the route whose models the picker is showing (the row under the
  cursor while the Providers column has focus).
  `Overlay::ConnectRemoveConfirm { profile_id, lines, choice }` is the
  remove confirm: `RemoveChoice::Cancel` is the default row, ↑↓/Tab toggle,
  and Enter on `Remove` emits `ConfirmRemoveProvider { profile_id }`.
- `command_palette` — palette data and filtering.
- `model_picker` — model-picker data and route grouping.
- `setup` — pre-session setup shell (theme, then directory trust).
- `decision` — literal, scrollable details for human decisions.

Commands, history, effort, launch:

- `commands` — `SlashCommand`, `GoalAction`, `CommandError`, `parse_slash`.
- `history` — `InputHistory`, `HistoryStore`, `MAX_INPUT_HISTORY`.
- `effort` — `ReasoningEffort`, provider-neutral per-session effort.
- `launch` — `decide_launch(theme_committed, trusted_at_start,
  provider_connected) -> LaunchDecision` (`run_theme_setup`,
  `run_trust_setup`, `allow_resume_picker`, `require_connect`,
  `show_ready_placeholder`). Pure; no IO.

Subdirectories:

- [`./app/`](./app/EGM.md) — `TuiApp`, event loop, input routing, persistence.
- [`./widgets/`](./widgets/EGM.md) — render models and ratatui widgets.

## Invariants

- `lib.rs` is the single source of truth for the crate's public surface; a
  module stays private unless `lib.rs` re-exports a symbol from it.
- Rendering is bottom-up and side-effect free: `conversation`, `markdown`,
  `mermaid`, `theme_preview`, and `widgets` compute lines from supplied models
  and paint into a `ratatui::Buffer`. They never touch the live session.
  (`theme` system-appearance detection and `theme_registry` loading do IO;
  see Side Effects.)
- Layout minimums are enforced before anything paints: `is_too_small` uses
  `MIN_WIDTH`/`MIN_HEIGHT`; a frame below them renders the too-small notice
  instead of a squeezed layout.
- Colors are applied only through `theme` helpers, and
  `strip_colors_if_disabled` runs before backend output so no-color terminals
  get structure, not raw SGR effects.
- `terminal::TerminalGuard` must outlive every draw and input call; it restores
  terminal state on all three exit paths (normal, error, panic).
- `file_explorer::safe_path` confines explorer directory listing (including
  symlink targets) and `source_viewer` reads to the workspace. Explorer
  create/rename/delete/move are confined by
  `forge_workspace::file_ops::WorkspaceFileOps` in `app/files.rs` instead.
- `validation` caps work: 128 KB parse buffer, 64 failed tests, 5 displayed
  failures (`MAX_FAILED_DISPLAY`), 200-char test names.
- `interactive_terminal` bounds memory by design: fixed scrollback and a bounded
  output queue; a fast child applies backpressure rather than growing the queue.
- Displayed key hints must match routing: a `hints` table may only advertise
  keys the owning overlay actually handles (for example `PICKER` lists
  `Ctrl-R` refresh, `Ctrl-A` add model, `Ctrl-E` edit, `Ctrl-X` remove because
  the model picker routes them).
- `launch::decide_launch` only offers the resume picker when the theme is
  committed and the directory was trusted at process start.
- Slash parsing is pure: `parse_slash` returns `None` for non-slash input and
  `Some(Err(..))` for a malformed slash command; it does not execute anything.

## Side Effects

- Terminal ownership: `terminal` and `app::shell` mutate process-global terminal
  modes; stdout is exclusively the renderer's while the TUI is live.
- Clipboard: `clipboard` writes OSC 52 sequences to stdout; `clipboard_image`
  reads the OS clipboard by spawning `osascript` and `sips` (macOS),
  `wl-paste` / `xclip` (Linux) or `powershell` (Windows).
- Notifications: `notify` may write OSC 9 or BEL to the terminal and keeps a
  process-wide mode and focus flag.
- Filesystem: `file_explorer` reads directory trees (create/rename/delete/move
  live in `app/files.rs` via `forge_workspace::file_ops`); `source_viewer` and
  the editor read and write workspace files; `history` persists input history
  to disk; `theme_registry` reads `.toml` themes from `<workspace>/.forge/themes`
  and the user config dir's `forge/themes`.
- Processes: `interactive_terminal` spawns a PTY and a child shell; `editor`
  only resolves `$VISUAL`/`$EDITOR` (the launch is in `app/context.rs`);
  `theme` spawns `gsettings` / `defaults` / `reg` to detect the system
  appearance; `diff_view`/workspace code can invoke git.
- Global state: `git_prefs` installs process-wide flags read by other modules.

## Verification

- Run: `cargo test --package forge-tui --locked`
- Targeted: `cargo test --package forge-tui --locked <filter>`
