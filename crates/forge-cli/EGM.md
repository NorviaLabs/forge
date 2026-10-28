# forge-cli

## Overview

Crate root for the `forge` binary target. No source code at this level: the
entrypoint lives in `src/`, integration tests in `tests/` (no contract there).

## Interface

- Binary target `forge` (`[[bin]] path = "src/main.rs"`, `default-run`); no
  library target, so nothing here is importable by other crates.
- Command line: `forge [--resume [SESSION_ID] | --continue | --fork SESSION_ID]`
  opens the TUI; `forge bench` runs one headless turn and prints JSON; the
  hidden `forge __egress-connect` helper tunnels stdio through the egress
  proxy. Details in [./src/EGM.md](./src/EGM.md).
- Process exit codes are `forge_tui::ExitCode` values (`0` success, `1`
  failed, `2` awaiting HITL, `3` canceled, `4` config error); `__egress-connect`
  uses its own `0/1/2`.
- Workspace dependencies: `forge-config`, `forge-connect`, `forge-session`,
  `forge-tui`, `forge-tools`, `forge-types`.

## Subdirectories & Boundaries

- `src/`: [./src/EGM.md](./src/EGM.md) — clap CLI, TUI launch/startup sequencing, `bench` headless mode, `__egress-connect` proxy helper.

## Notes

- `tests/` holds integration tests only and is intentionally undocumented.
- This crate owns process-level concerns only (argv, exit codes, tracing, runtime, stdio). Session assembly belongs to `forge-session`, rendering to `forge-tui`.

## Invariants

- The binary is a thin composition layer: it must not reimplement session assembly, governance, or rendering.
- Repository ownership is acquired before the primary session is created, and both are handed to `forge-session` together.

## Side Effects

- Process-wide effects only at startup: tracing installation, Tokio runtime construction, TUI preference installs, and `std::process::exit`.
- Delegated state changes (journals, control DB, worktrees) happen through `forge-session`.

## Verification

- Run: `cargo test --package forge-cli --locked`
