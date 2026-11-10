# forge-cli

## Overview

Crate root for the `forge` binary target. No source code at this level: the
entrypoint lives in `src/`, integration tests in `tests/` (no contract there).

## Interface

- Binary target `forge` (`[[bin]] path = "src/main.rs"`, `default-run`); no
  library target, so nothing here is importable by other crates.
- Command line: `forge [PATH]` opens the desktop app (on `PATH` when given);
  `forge bench` runs one headless turn and prints JSON; the hidden
  `forge __egress-connect` helper tunnels stdio through the egress proxy.
  Details in [./src/EGM.md](./src/EGM.md).
- Process exit codes: `0` success, `1` failed; the desktop launch returns the
  app's own exit code; `__egress-connect` uses its own `0/1/2`. Clap exits on
  its own first: `2` on an argument error and `0` for `--help`/`--version`. A
  Tokio runtime build failure panics.
- Workspace dependencies: `forge-config`, `forge-connect`, `forge-session`,
  `forge-tools`, `forge-types`.

## Subdirectories & Boundaries

- `src/`: [./src/EGM.md](./src/EGM.md) — clap CLI, desktop-app discovery, `bench` headless mode, `__egress-connect` dispatch.

## Notes

- `tests/` holds integration tests only and is intentionally undocumented.
- This crate owns process-level concerns only (argv, exit codes, tracing, runtime, stdio). Session assembly belongs to `forge-session`, the interactive frontend to `forge-desktop`.

## Invariants

- The binary is a thin composition layer: it must not reimplement session assembly, governance, or rendering.
- `bench` never takes the repository lease; it writes only its session journal.

## Side Effects

- Process-wide effects only at startup: tracing installation and Tokio runtime
  construction (`bench` only), desktop-app launch, and `std::process::exit`.
- Delegated state changes (journals) happen through `forge-session`.

## Verification

- Run: `cargo test --package forge-cli --locked`
