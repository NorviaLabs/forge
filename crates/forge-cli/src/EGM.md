# forge-cli source

## Overview

The `forge` binary: argument parsing, desktop-app discovery by default, startup
sequencing for `--tui` (`forge_session::LaunchPlan::prepare` for ownership,
theme/trust setup screens, `LaunchPlan::launch`, supervisor handoff), the
headless `bench` subcommand, and the hidden `__egress-connect` TCP-over-CONNECT
helper. All session assembly is delegated to `forge-session`; all rendering to
`forge-tui` or the desktop app.

## Interface

`main.rs`:

- `struct Cli` (clap `Parser`): `--tui`, `--resume [SESSION_ID]`, `--continue`, `--fork SESSION_ID` (mutually exclusive), optional `Command`.
- `enum Command::Bench(BenchArgs)`; `BenchArgs { workspace, journal, model, effort ("max"), approve_all }`; `--model` must be `<provider>/<model>`.
- `fn main()` — intercepts `egress_connect::SUBCOMMAND` before clap, then routes a bare launch to `launch::run`; otherwise installs `tracing_subscriber` (env filter, stderr, no ANSI), builds a multi-thread Tokio runtime, runs `run(cli)`, then `runtime.shutdown_timeout(0)` and `std::process::exit(code.code())`.
- `fn needs_tui(&Cli) -> bool`: `--resume`, `--continue` or `--fork` without `--tui` is refused with exit 2 rather than silently ignored.
- `fn launch_target(&Cli, allow_resume_picker: bool) -> LaunchTarget`: `--fork` → `Fork`, `--continue` → `Continue`, `--resume <ID>` → `Resume`, bare `--resume` → `Latest`, none → `New`; always `New` when the resume picker is disallowed.
- `async fn bounded_shutdown(shutdown, timeout) -> Option<String>`: what to print on stderr after a supervisor shutdown that failed or timed out.
- `async fn run(Cli) -> anyhow::Result<ExitCode>`, `async fn run_bench(BenchArgs) -> anyhow::Result<ExitCode>`, `async fn run_tui(Cli) -> anyhow::Result<ExitCode>`.
- `fn resolve_profile_id(model: &str) -> anyhow::Result<String>`.

`launch.rs`:

- `pub fn run(workspace: Option<&Path>) -> i32`: starts `forge-desktop` and returns its exit code.
- `pub fn display_available() -> bool`: on Linux, either `DISPLAY` or `WAYLAND_DISPLAY` must be set.
- Discovery order: `FORGE_DESKTOP_BIN`, the directory containing the running `forge`, `Forge.app` in `~/Applications` then `/Applications` (macOS), then `PATH`.
- Missing app prints the installer command plus `forge --tui` as the terminal alternative.

`egress_connect.rs`:

- `pub const SUBCOMMAND: &str = "__egress-connect"`; `pub fn run(args: &[String]) -> i32` (exit codes: 0 tunnel closed cleanly, 1 refused/I-O failure, 2 usage or bad port).
- Internal `Failure::{Refused, Io}` and `connect(proxy, host, port)`.

## Invariants

- `forge` with no subcommand opens the desktop app; `--tui` selects the terminal. TUI startup failures print `error: {e:#}` to stderr and exit `ExitCode::Failed`. `bench` never launches either frontend.
- `run_tui` order: `LaunchPlan::prepare` (**read-only ownership probe first**: a launch another Forge owns exits 1 before any setup screen and before any trust or theme write; writes nothing; also reads the trust decision) → `decide_launch` → `run_setup` screens → `LaunchPlan::launch` (trust re-check, then the lease, recovery, target resolution and session open; see forge-session `launch.rs`) → supervisor. Ownership refusals: `AlreadyOwned` names the owning pid; any other ownership failure in a git repository (unreadable lock file, control database error) is also fatal. Only a workspace with no repository storage (non-git) runs one lease-less session via `run_tui_with_launch`. `run_bench` never takes the lease.
- `--continue` reopens the workspace's **selected** session (control DB), falling back to the most recent journal, with a notice when the selected session is archived or its journal is gone. Before S2 it always opened the most recent journal; this is a deliberate behaviour change (N-M3). Bare `--resume` still opens the most recent journal. Resume/fork/continue are mutually exclusive by clap declaration, and are ignored (`New`) when `decide_launch` disallows the resume picker.
- After a supervised TUI run returns, `run_tui` calls `RepositorySupervisor::shutdown` (bounded by `SUPERVISOR_SHUTDOWN_TIMEOUT`, 10 s), so the lease is released explicitly before exit, not only by process death. A failure or timeout is reported on stderr and does not change the exit code.
- `bench` requires a non-empty prompt on stdin and applies `--model` with `session.set_active_model` and the resolved profile with `set_active_profile_id`, because config files never select the active model.
- `bench` output is a single JSON object on stdout: either `{"status":"approval_required", "policy":"sandboxed", "approval":{...}}` with exit `Failed`, or `{"model","profile_id","effort","response","notices"}` with exit `Success`.
- `resolve_profile_id` takes the profile id from the model prefix via one case-insensitive `registry.get(prefix)`: an old prefix fails with its current name (`old_prefix_hint`), and an unknown prefix fails listing the known provider ids.
- `__egress-connect` rejects target hosts containing whitespace or control bytes, requires HTTP status `200` from the proxy `CONNECT`, and exits before any tunnel copy otherwise. IPv6 hosts are bracketed.
- The `__egress-connect` subcommand never appears in `--help` (matched before clap).

## Side Effects

- Reads stdin (`bench` prompt; `__egress-connect` tunnel bytes) and writes stdout (JSON results, tunnel bytes) and stderr (logs, errors, usage). After the TUI exits, `run_tui` prints the token-usage summary and a `To continue this session, run forge --resume <id>` hint to stdout, and `quit all sessions: ...` to stderr on a quit failure.
- Spawns and waits for `forge-desktop` in the default path.
- Initializes global tracing, a Tokio runtime, and TUI-global preference installs (`forge_tui::notify::install`, `forge_tui::git_prefs::install`) only for `--tui`/`bench`.
- The TUI path acquires the repository-group lease file and opens/creates the control SQLite database via `RepositoryBootstrap`, and rolls back interrupted managed creations. `bench` takes no lease and writes only its session journal.
- Creates, resumes, or forks durable sessions and journals, and may create/remove git worktrees through `forge-session`.
- Opens TCP connections to the egress proxy and copies bytes bidirectionally; spawns one thread for stdin → socket.
- Calls `std::process::exit`, so no destructors run past that point.

## Verification

- Run: `cargo test --package forge-cli --locked`
