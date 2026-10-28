# forge-tui — Component Package

## Overview

The full-screen ratatui terminal UI for Forge, plus the slash-command grammar that
the headless/CLI paths share. `forge-cli` builds an `AgentSession` and hands it to
this crate, which owns the terminal (raw mode, alternate screen, event loop,
rendering) until the operator quits or the app exits for a reason.

The crate is a library (`lib.rs`), not a binary. Only `design`, `git_prefs`,
`links`, and `notify` are public modules; the application types are exposed
through `lib.rs` re-exports.

## Interface

- Crate root: `forge-tui` (`lib.rs`), consumed by `crates/forge-cli`.
- `ExitCode` — process exit codes: `Success = 0`, `Failed = 1`,
  `AwaitingHitl = 2`, `Canceled = 3`, `ConfigError = 4`, via `ExitCode::code()`.
- Runtime entry points: `run_tui`, `run_tui_with_launch`, `run_tui_supervised`,
  `run_tui_with_resume_picker`, all returning `Result<ExitSummary, TuiError>`.
- App surface: `TuiApp`, `TuiLaunch`, `TuiRuntimeConfig`, `TuiError`,
  `ExitSummary`, `resume_session_items`.
- Slash commands: `parse_slash`, `SlashCommand`, `CommandError`.
- Transcript model: `ConversationModel`, `ConversationRender`,
  `ConversationViewOpts`, `ChatItem`, `BannerKind`, `StreamWaitPhase`,
  `ToolCardState`.
- Layout: `is_too_small`, `split_areas`, `split_areas_ex`, `split_areas_full`,
  `LayoutRegions`, `MIN_WIDTH`, `MIN_HEIGHT`.
- Overlays: `Overlay`, `OverlayAction`, `OverlayKey` (`Key`),
  `handle_overlay_key`, `filter_palette`, `default_palette_items`,
  `models_from_catalog`, `ResumeSessionItem`.
- Environment/services: `FileExplorer`, `FileExplorerWidget`, `HistoryStore`,
  `InputHistory`, `MAX_INPUT_HISTORY`, `ActivityFeed`, `ActivityItem`,
  `ActivityKind`, `ReasoningEffort`, `LaunchDecision`, `decide_launch`,
  `run_setup`, `SetupRequest`, `SetupResult`.
- Validation: `is_cargo_test_command`, `validation_command_text`,
  `CargoTestSummary`, `ValidationParseState`, `ValidationSnapshot`,
  `ValidationStatus`, `MAX_FAILED_DISPLAY`.
- Widgets facade re-exported at crate root: `classify_operator_error`,
  `session_chrome_rows`, `BusyPhase`, `FeedbackModel`, `FeedbackSeverity`,
  `FooterModel`, `InputModel`, `StatusModel`, `TurnLifecycle`.
- Source tree: [`./src/EGM.md`](./src/EGM.md) — modules, transcript, themes,
  terminal guard.
- Build script: `build.rs` generates `$OUT_DIR/builtin_themes.rs` defining
  `const BUILTIN_THEMES: &[(&str, &str)]` (file name, TOML contents), which
  `src/theme_registry.rs` iterates (see Invariants).
- Not documented here: `tests/` (integration tests only) and `themes/`
  (TOML data assets consumed by `build.rs`).

## Invariants

- `ExitCode` discriminants are part of the process contract: `0/1/2/3/4` for
  success, failure, awaiting HITL, cancel, config error. Locked by a unit test.
- `build.rs` is std-only and declares no `[build-dependencies]`; adding one
  changes the build graph for every downstream crate.
- Theme embedding is compile-time: every `themes/*.toml` becomes an
  `include_str!` entry in `builtin_themes.rs`, sorted by file name, so the
  embedded order is deterministic and independent of directory iteration order.
- A theme added to `themes/` requires only a rebuild; a theme removed must not
  leave a stale `theme_id` referenced by config
  (`ThemeRegistry::resolve_startup_id` falls back to the default theme for an
  unknown id).
- Public API is exactly what `lib.rs` re-exports. Everything else in `src/` is
  crate-private on purpose; do not widen a module to `pub` to reach it from
  another crate.

## Side Effects

- Build time: reads `themes/` from `CARGO_MANIFEST_DIR` and writes
  `$OUT_DIR/builtin_themes.rs`; `cargo:rerun-if-changed=themes`.
- Run time (through `src/`): takes over the process terminal — raw mode,
  alternate screen, mouse capture, bracketed paste, keyboard enhancement flags
  — and restores it on quit, error, and panic. While the TUI is live, stdout is
  owned by the renderer.
- Run time: reads and writes session/journal state on disk through
  `forge-session`, and UI/runtime state through `forge_storage`
  (`LocalRuntimeStorage` / `RuntimeStorage`).
- Run time: persists user-level input history to
  `forge_storage::forge_data_dir()/input-history.json`; the
  `FORGE_INPUT_HISTORY_PATH` env var overrides that path.
- Run time: may spawn PTYs, subprocesses, and background async tasks on the
  operator's behalf; see [`./src/EGM.md`](./src/EGM.md) for the ownership split.

## Verification

- Run: `cargo test --package forge-tui --locked`
- Targeted: `cargo test --package forge-tui --locked <filter>`
- Static: `cargo clippy --package forge-tui --all-targets --locked -- -D warnings`
