# forge-tui/src/widgets — Render Models

## Overview

The presentation layer of the TUI. Every widget here takes a fully resolved
model — strings, counts, booleans, pre-computed labels — and paints it into a
`ratatui::Buffer`. None of them reads the live session, the overlay stack, or
the focus state machine; the application resolves those and passes the result
in. This is what keeps transcript and chrome painting cheap and testable.

`mod.rs` re-exports the public set; `navigator`, `status`, `footer`, and `input`
also expose crate-internal helpers shared with pointer hit-testing.

## Interface

Re-exported from `mod.rs`:

- `bottom_panel::{BottomPanel, BottomPanelModel, BottomPanelState}` —
  `BottomPanelState { open, focused }`; the model carries `busy_phase`,
  `activity`, `terminal_content`, `terminal_running`, `terminal_shell`,
  `terminal_cursor`.
- `feedback::{classify_operator_error, FeedbackModel, FeedbackSeverity}` —
  `FeedbackSeverity { Info, Warn, Error, Ok }`; `FeedbackModel { text, severity }`
  with `is_empty`/`info`/`warn`/`error`/`ok`; `classify_operator_error` maps raw
  provider/IO errors to operator copy (429/401/403, timeouts, etc.).
- `footer::{footer_short_model_id, FooterActivity, FooterBar, FooterFocus,
  FooterModel}` — `FooterFocus { Llm, Effort }`; `FooterActivity` is counts-only
  (`jobs_*`, `agents_*`, `queued`); `footer_short_model_id` shortens
  `provider/model` for the chip.
- `input::{composer_cursor_position, composer_text_area_width, InputBar,
  InputModel}` — `InputModel { text, cursor, dimmed, hint, history_browse,
  not_connected, waiting }` plus `insert`, `insert_newline`, `insert_paste`.
- `navigator::{NavigatorRowStop, NavigatorTab, PeekPanel, SessionList,
  SessionRow, SessionRowState}` and crate-internal `TEXT_COL`.
- `panel::background_focused` (re-exported) and `panel::title` (reached as
  `widgets::panel::title`) — focus marker suppression while a modal is open,
  and the shared pane title row built on it.
- `queued_messages::QueuedMessages` — `{ messages, selected, hover }` plus
  `message_index_at` and `visible_window` (max 3 rows).
- `status::{session_chrome_rows, BusyPhase, StatusBar, StatusModel,
  TurnLifecycle}` — `BusyPhase { Idle, Model, Tool{name}, Connect, Other }`,
  `TurnLifecycle { Ready, Working, Waiting, Completed, Failed, Cancelled,
  Interrupted }`, `StatusModel` (model/provider/effort/ctx percentile/branch/
  dirty/cwd plus activity and waiting details).
- `task_strip::{TaskStrip, TaskStripItem}` — `TaskStripState { Idle, Running,
  Waiting, Completed, Failed, Interrupted, Unavailable }`,
  `From<TaskLifecycle>` mapping.
- `toasts::ToastStack` — single-slot auto-expiring overlay, `TOAST_TIMEOUT` of
  2s, `toast_type_for(FeedbackSeverity)`.
- `turn_line::{phase_verb, turn_line, TurnLineModel}`; `INTERRUPT_HINT` is
  not re-exported (reached as `widgets::turn_line::INTERRUPT_HINT`).

Not re-exported: `start` (`pub(crate)`, home splash starters) and
`git_changes` (`pub mod`, `GitChangeRow`, `GitChangesList`), used through their
module paths. `widgets` itself is a private module of the crate; only the
symbols `lib.rs` re-exports (`classify_operator_error`, `session_chrome_rows`,
`BusyPhase`, `FeedbackModel`, `FeedbackSeverity`, `FooterModel`, `InputModel`,
`StatusModel`, `TurnLifecycle`) leave the crate.

## Invariants

- Widgets are pure paint: they read their model and write to the supplied
  `Buffer`. No filesystem, network, or session access. Animated surfaces take a
  step counter or `millis` from the caller; only `ToastStack` keeps its own
  expiry timestamp.
- Zero-size areas render nothing and return early; several widgets also return a
  cursor/terminal position instead of panicking.
- The turn line's marker is exactly one cell wide in every frame — braille
  spinner, ASCII fallback when color is disabled, and `*` under reduced motion —
  so the row never shifts horizontally. The interrupt hint is dropped, not
  wrapped, when the pane is too narrow.
- `panel::background_focused(pane_focused, modal_open)` is the only definition of
  "visible focus"; `pane_title`-based titles use it so at most one keyboard owner
  is painted.
- `SessionRowState` gives every state its own glyph; two states sharing a marker
  previously rendered `failed` as `○`. `TurnLifecycle` is the authoritative
  lifecycle and mirrors `TaskLifecycle` 1:1; `BusyPhase` is activity detail only
  and must not be used as lifecycle.
- `SessionList` reserves all five prefix cells (`TEXT_COL`) on every row, so
  selection, hover, or expansion never moves another row's label column. The
  spinner step is advanced by the event loop, never by wall-clock time.
- `InputModel::insert_paste` compacts payloads over 1,000 characters into an
  atomic placeholder while retaining the full text for submission; paste
  normalization happens before insertion.
- `QueuedMessages` shows at most 3 rows and keeps the selection inside the
  window; row 0 is the title and is never hittable.
- `FooterActivity` all-zero means the second row paints blank; the footer is
  full-window width and both halves must fit at the enforced minimum width.
- `ToastStack` holds at most one toast; a new push replaces the old one rather
  than stacking, and toasts never take focus or block input.

## Side Effects

- Writes only into the `ratatui::Buffer`/`Frame` it is given.
- `FooterBar` records the x-ranges of the model, effort and notes chips into
  its `ChipSink` (`RefCell<Option<[(u16,u16);3]>>`) during paint, so pointer
  hit-testing in the app can use last frame's geometry.
- `InputBar` writes a private probe color (`Color::Indexed(255)`) at the caret to
  locate it; the real draw replaces that style before backend output, so the
  probe never reaches the terminal.
- `ToastStack` mutates its own engine and shown-at timestamp as time passes;
  it is the only widget with internal time-driven state.
- No widget performs IO, spawns tasks, or mutates application state.

## Verification

- Run: `cargo test --package forge-tui --locked`
- Targeted: `cargo test --package forge-tui --locked widgets::`
