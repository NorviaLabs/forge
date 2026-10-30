# forge-tui/src/app — Application Core

## Overview

`TuiApp` and everything that drives it: construction, the async event loop,
terminal lifecycle, keyboard/mouse routing, the turn lifecycle, overlays'
actions, session roster and supervisor handling, and UI-state persistence.

The module is split by responsibility, but `TuiApp` is one type. Its fields and
methods are shared across the submodule files (`impl TuiApp` blocks live in
several of them), so a change to a field is a change to every file that reads
it. `mod.rs` declares the submodules and re-exports the public surface.

## Interface

Public API (re-exported by `lib.rs`):

- `TuiApp` — the application state machine. Constructed by
  `new(session, runtime)`, `new_supervised` or
  `new_with_startup_resume_picker`; driven by `draw`, the input handlers, and
  `shell::tick_application`.
- `TuiLaunch` — launch flags: `startup_items: Option<Vec<ResumeSessionItem>>`,
  `onboarding_connect: bool`, `ready_placeholder: bool`.
- `TuiRuntimeConfig` — immutable launch identity: `model_label`, `provider`,
  `cwd`, `version`, `startup_notices`, `file_icons`, `theme_id`,
  `reduced_motion`. `Default` yields empty label, `"native"`, `"."`, `"test"`,
  the default theme and no reduced motion.
- `TuiError` — `Io(io::Error)`, `Loop(LoopError)`, `Other(String)`.
- `ExitSummary` — `exit_code: ExitCode`, `session_id: String`,
  `token_usage: Option<String>`, `quit_failure: Option<String>`.
- `run_tui`, `run_tui_with_launch`, `run_tui_supervised`,
  `run_tui_with_resume_picker` — all `async`, all return
  `Result<ExitSummary, TuiError>`.
- `resume_session_items` — build the resume picker's rows.

Crate-internal surface used by sibling modules and tests: `SelectedRuntime`,
`SessionChromeItem`, `session_needs_attention`, `session_notice_severity`,
`format_exit_token_usage`, `recent_resume_sessions`.

Submodules by responsibility:

| Module | Role |
| --- | --- |
| `new` | `TuiApp` construction (direct and supervised). |
| `shell` | Startup, event loop, terminal lifecycle, `run_tui*` entry points. |
| `render` | `TuiApp::draw` — frame composition (file split only). |
| `input`, `input_route`, `mouse` | Keyboard routing, composer submit destination, pointer routing. |
| `focus` | Keyboard focus and block navigation. |
| `turn` | Turn lifecycle, streaming, message queue. |
| `commands` | Slash and semantic command dispatch. |
| `overlays` | Overlay actions, theme and help rendering. |
| `approvals`, `questions`, `child_approval` | HITL approval, `ask_user_question`, child decisions. |
| `agents`, `tasks_view`, `task_stop` | Child-agent receipts, Jobs/Agents/Queue inspection, named interruption. |
| `files`, `diff`, `github_view`, `workspace`, `watch` | Explorer and filesystem ops, review mode, GitHub state, workspace panes, fs watching. |
| `chrome` | Toasts, feedback, activity summary, status header and footer. |
| `connect` | Provider connection, credentials, model selection, per-route model-catalog refresh (`start_catalog_refresh`, `poll_catalog_refresh`). |
| `context`, `goal`, `scratchpad` | Compaction/external-editor drains, `/goal`, session notes. |
| `persist`, `session_state`, `selection`, `resize`, `quit` | UI-state persistence, session-row helpers, runtime ownership, pane resizing, quit-all sweep. |
| `util`, `types` | Path/footer helpers; all state types including `TuiApp`. |

## Invariants

- `focus: FocusState` is the authoritative keyboard owner. Legacy component
  `focused` flags are synchronised from it for rendering only; never make a
  component's flag authoritative again.
- `session_view` is a per-frame snapshot refreshed at the top of `draw`. Render
  paths read the snapshot, not the live session, so painting never depends on
  who owns the `AgentSession`.
- `transcript_view` is an `Arc` re-copied only when the transcript actually
  changes; it is deliberately separate from `session_view` because it is the
  expensive half.
- The conversation render cache is keyed by `ConversationRenderKey`, which must
  cover every input the transcript paint depends on (lifecycle, theme, width,
  pending HITL/question identity, agent state, per-turn summaries, …). A missing
  key field freezes a surface until an unrelated change rebuilds it.
- Composer submit destination is chosen by `selected_input_route`. A direct
  session uses `input_route::classify_input` on the active task; a supervised
  session branches on `pending_question`, `session_view.lifecycle` and
  `selected_turn_running()` (the supervisor snapshot's turn state), never the
  restored `busy` flag. Only for direct sessions does `selected_turn_running()`
  fall back to `busy_state.is_active()`.
- Approval state is owned by `app::approvals`; other areas go through `TuiApp`
  methods instead of touching its collections.
- Child decisions are bound to the displayed parent, child execution and
  request together; a decision may not be applied to a different triple.
- Commands are submitted without awaiting execution and polled each tick
  (`pending_command_completions`), so a busy actor can never block input or
  painting. A completion lands for the session that submitted it even if the
  operator switched away.
- A turn's `Finished` summary is view-only, keyed by session plus user-message
  ordinal, and never persisted; resume rebuilds it, so restored turns carry no
  ephemeral timing.
- `navigator_done_pending` is a set, not a slot: two sessions marked done while
  both are still stopping must both be honored.
- Quit-all failures are carried out in `ExitSummary::quit_failure` — after exit
  there is no frame left to report them in.
- UI state on disk is versioned (`UI_STATE_VERSION = 2`); bumps require a
  migration or a discard path. `WORKSPACE_HISTORY_LIMIT = 32`.
- Model-catalog refresh is explicit and per route: it starts only from the
  model picker (opening it or `OverlayAction::RefreshModels`) for
  `Overlay::picker_route`, and only when that route's credentials are live.
  There is no lazy warm-up from `draw()` or the tick. At most one refresh is
  in flight (`CatalogFetchState::refresh_rx` + `refreshing_profile`); a
  request for another route parks in the single `pending_profile` slot (latest
  wins) and starts only if the picker still shows that route when the first
  one finishes. The refreshed catalog lives in the disk cache, not in a second
  in-memory copy.
- `OverlayAction::EditProvider` opens `Overlay::ConnectCustom` for a user
  profile (`details: Some([title, label, api_key_env.join(", ")])`) or a
  built-in with an editable `BaseUrl` (`details: None`); any other profile
  goes to the OAuth flow or the API-key prompt. On save, `details` becomes
  `forge_connect::ProviderDetails` (key env names split on commas, trimmed,
  empties dropped) and is passed to `update_custom_provider`.
- `OverlayAction::RemoveProvider` (`begin_remove_provider`) acts only with
  the Providers column focused. It refuses a built-in, a running model
  refresh, or a busy session on the provider (supervised `Running`/`Queued`/
  `Waiting` in the roster, or the direct session with a turn, pending prompt,
  approval, or question), then opens `Overlay::ConnectRemoveConfirm` from
  `ConnectService::preview_removal`. `confirm_remove_provider` repeats the
  refresh and busy checks, removes, reloads every session's routes, clears
  `connect.profile` and sets `auth_suspended` when the active provider went
  (so the send path's auto-restore cannot pick another provider), reopens
  the picker, and records one Connect activity entry. Esc on the confirm
  returns to the picker. Saving a new custom provider first clears any key,
  cached models, or saved selection left under its id.
- `unowned_model_gate(GateTarget, text)` runs first on every send path: the
  composer (`dispatch_line`, before the attachment is taken and before the
  auto-restore), `dequeue_and_send_next`, `enqueue_user_message`, and the
  Sessions navigator's reply and `c` continue. When the target session's
  model has a provider prefix no loaded provider owns and some provider is
  connected, nothing is sent: that session is selected, the typed text is
  put back in the composer, and the picker opens (on Models when a provider
  is active, otherwise the full picker on Providers). Empty
  models, ids without a prefix, the mock provider, and "nothing connected"
  fall through to the existing gates.
- Catalog-refresh outcomes are recorded per route in `connect.catalog_errors`;
  an auth failure also marks the route `auth_required` and invalidates the
  connected state. Neither outcome changes the running session's model.
- `TuiApp::draw` lives in `render.rs` by file split only; `TuiApp`'s fields and
  every method signature are owned by `types.rs`/the module that defines them.

## Side Effects

- Terminal: enters raw mode, alternate screen, mouse capture, bracketed paste
  and keyboard enhancement flags; restores all of them on every exit path.
- Persistence: writes and reads UI state (`persist`), session/journal state via
  `forge-session`, and input history via `HistoryStore`.
- Filesystem: explorer create/rename/delete/move through
  `forge_workspace::file_ops`, an fs watcher (`notify`) per watched root, and
  source/editor reads and writes.
- Network: provider connection, credential store and OAuth (`forge-connect`),
  model catalog refreshes (`forge_connect::refresh_profile_catalog`, which
  writes the user model-catalog cache and the credential store's
  `auth_required` flag), custom-provider and manual-model saves
  (`forge_connect::save_custom_provider` / `update_custom_provider` /
  `save_manual_model`), GitHub view data, and `web_search`/`web_fetch` tool
  calls surfaced in the transcript.
- Processes: git sync/status subprocesses, the external editor (terminal is
  suspended and resumed around it), and the PTY-backed interactive terminal.
- Concurrency: the model-catalog refresh, repo header refresh and GitHub view
  loading each run on a plain `std::thread` that reports over a
  `std::sync::mpsc` channel polled each tick. `spawn_blocking` runs the
  terminal-event reader (`shell::TerminalEventSource`) and the model-stream
  relay in `turn`. `IsolatedTask` runs model completions, tool application,
  approved HITL tools, context compaction and `/goal` model calls. Progress
  (`.forge/progress.json`) is read synchronously on a poll interval.
  Supervisor commands go over `SupervisorHandle`.
- Out-of-band notification (`forge-tui::notify`) on background completion.

## Verification

- Run: `cargo test --package forge-tui --locked`
- Targeted: `cargo test --package forge-tui --locked app::`
