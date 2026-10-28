# forge-cli source

## Overview

The `forge` binary: argument parsing, startup sequencing (theme/trust setup,
repository lease, session open, supervisor handoff), the headless `bench`
subcommand, and the hidden `__egress-connect` TCP-over-CONNECT helper. All
session assembly is delegated to `forge-session`; all rendering to `forge-tui`.

## Interface

`main.rs`:

- `struct Cli` (clap `Parser`): `--resume [SESSION_ID]`, `--continue`, `--fork SESSION_ID` (mutually exclusive), optional `Command`.
- `enum Command::Bench(BenchArgs)`; `BenchArgs { workspace, journal, model, effort ("max"), approve_all }`; `--model` must be `<provider>/<model>`.
- `fn main()` — intercepts `egress_connect::SUBCOMMAND` before clap, installs `tracing_subscriber` (env filter, stderr, no ANSI), builds a multi-thread Tokio runtime, runs `run(cli)`, then `runtime.shutdown_timeout(0)` and `std::process::exit(code.code())`.
- `async fn resolve_target(&Config, Option<Option<SessionId>>) -> anyhow::Result<(SessionTarget, Option<String>)>`.
- `fn config_for_workspace(&Config, &Path) -> Config`.
- `async fn run(Cli) -> anyhow::Result<ExitCode>`, `async fn run_bench(BenchArgs) -> anyhow::Result<ExitCode>`, `async fn run_tui(Cli) -> anyhow::Result<ExitCode>`.
- `fn resolve_profile_id(model: &str) -> anyhow::Result<String>`.

`egress_connect.rs`:

- `pub const SUBCOMMAND: &str = "__egress-connect"`; `pub fn run(args: &[String]) -> i32` (exit codes: 0 tunnel closed cleanly, 1 refused/I-O failure, 2 usage or bad port).
- Internal `Failure::{Refused, Io}` and `connect(proxy, host, port)`.

## Invariants

- `forge` with no subcommand and no error always opens the TUI; startup failures print `error: {e:#}` to stderr and exit `ExitCode::Failed`.
- `--resume <ID>` needs no journal lookup and maps straight to `SessionTarget::Resume`; bare `--resume` picks the most recent journalled session and falls back to `SessionTarget::New` with an explicit notice when none exists.
- Resume/fork/continue are mutually exclusive by clap declaration; resume and fork are ignored (`SessionTarget::New`) when `decide_launch` says the resume picker is disallowed.
- Repository ownership precedes session creation: `RepositoryBootstrap::acquire` is attempted before any `open_session`, so a losing process cannot write session state first.
- The repository lease/bootstrap is skipped (with a startup notice) when `RepositoryRuntimeStorage::new` fails; the TUI then runs in direct mode via `run_tui_with_launch`.
- Resuming or forking a session re-derives config from that session's recorded workspace (`config_for_workspace`); only `New` uses the launch config.
- `bench` requires a non-empty prompt on stdin and overrides `cfg.model.model` from `--model` because config files never select the active model. `--route-id` must name a registered route, otherwise it bails; otherwise the model prefix is matched against route ids/prefixes and ambiguous matches are refused.
- `bench` output is a single JSON object on stdout: either `{"status":"approval_required", "policy":"sandboxed", "approval":{...}}` with exit `Failed`, or `{"model","profile_id","effort","response","notices"}` with exit `Success`.
- `resolve_profile_id` takes the profile id from the model prefix: an old prefix fails with its current name (`old_prefix_hint`), and an unknown prefix fails listing the known provider ids.
- `__egress-connect` rejects target hosts containing whitespace or control bytes, requires HTTP status `200` from the proxy `CONNECT`, and exits before any tunnel copy otherwise. IPv6 hosts are bracketed.
- The `__egress-connect` subcommand never appears in `--help` (matched before clap).

## Side Effects

- Reads stdin (`bench` prompt; `__egress-connect` tunnel bytes) and writes stdout (JSON results, tunnel bytes) and stderr (logs, errors, usage).
- Initializes global tracing, a Tokio runtime, and TUI-global preference installs (`forge_tui::notify::install`, `forge_tui::git_prefs::install`).
- The TUI path acquires the repository-group lease file and opens/creates the control SQLite database via `RepositoryBootstrap`, and rolls back interrupted managed creations. `bench` takes no lease and writes only its session journal.
- Creates, resumes, or forks durable sessions and journals, and may create/remove git worktrees through `forge-session`.
- Opens TCP connections to the egress proxy and copies bytes bidirectionally; spawns one thread for stdin → socket.
- Calls `std::process::exit`, so no destructors run past that point.

## Verification

- Run: `cargo test --package forge-cli --locked`
