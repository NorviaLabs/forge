# forge-cli source

## Overview

The `forge` binary: open the desktop app, run one prompt headless with
`forge bench`, or dispatch the hidden `__egress-connect` helper. Session
assembly is delegated to `forge-session`; there is no terminal UI.

## Interface

`main.rs`:

- `struct Cli` (clap `Parser`): optional positional `PATH` (folder for the desktop app) and optional `Command`.
- `enum Command::Bench(BenchArgs)`; `BenchArgs { workspace, journal, model, effort ("max"), approve_all }`; `--model` must be `<provider>/<model>`.
- `enum ExitCode { Success = 0, Failed = 1 }` (private; the process contract).
- `fn main()` — intercepts `forge_tools::egress_connect::SUBCOMMAND` before clap; without `bench`, checks `launch::display_available()` and exits with `launch::run(PATH)`; with `bench`, installs `tracing_subscriber` (env filter, stderr, no ANSI), builds a multi-thread Tokio runtime, runs `run_bench`, then `runtime.shutdown_timeout(0)` and `std::process::exit`.
- `async fn run_bench(BenchArgs) -> anyhow::Result<ExitCode>`, `fn resolve_profile_id(model: &str) -> anyhow::Result<String>`.

`launch.rs`:

- `pub fn run(workspace: Option<&Path>) -> i32`: starts `forge-desktop` (with `workspace` as its argument when given) and returns its exit code.
- `pub fn display_available() -> bool`: on Linux, either `DISPLAY` or `WAYLAND_DISPLAY` must be set.
- Discovery order: `FORGE_DESKTOP_BIN`, the directory containing the running `forge`, `Forge.app` in `~/Applications` then `/Applications` (macOS), then `PATH`. A missing app prints the installer command.

The `__egress-connect` helper lives in `forge_tools::egress_connect` (shared with the desktop app, which git's routing also invokes); `main` only dispatches to it.

## Invariants

- `forge` with no subcommand opens the desktop app and never starts a session itself. `bench` never launches the app.
- `bench` requires a non-empty prompt on stdin and applies `--model` with `session.set_active_model` and the resolved profile with `set_active_profile_id`, because config files never select the active model. It takes no repository lease and writes only its session journal.
- `bench` output is a single JSON object on stdout: either `{"status":"approval_required", "policy":"sandboxed", "approval":{...}}` with exit `Failed`, or `{"model","profile_id","effort","response","notices"}` with exit `Success`.
- `resolve_profile_id` takes the profile id from the model prefix via one case-insensitive `registry.get(prefix)`: an old prefix fails with its current name (`old_prefix_hint`), and an unknown prefix fails listing the known provider ids.
- The `__egress-connect` subcommand never appears in `--help` (matched before clap).

## Side Effects

- Reads stdin (`bench` prompt; `__egress-connect` tunnel bytes) and writes stdout (JSON results, tunnel bytes) and stderr (logs, errors, usage).
- Spawns and waits for `forge-desktop` in the default path.
- `bench` initializes global tracing and a Tokio runtime, creates a durable session journal, and runs one turn through `forge-session::run_headless`.
- Calls `std::process::exit`, so no destructors run past that point.

## Verification

- Run: `cargo test --package forge-cli --locked`
