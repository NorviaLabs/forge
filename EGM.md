# Forge Blueprint

## System Overview

Forge is a Rust workspace for a terminal AI coding agent, editor, and shell.
`forge-cli` starts the full-screen TUI (`forge-tui`). The TUI drives sessions
that `forge-session` assembles around the `forge-core` agent loop. The agent
loop calls model providers through `forge-model` and runs validated tools from
`forge-tools` and `forge-mcp`. Every tool call passes one governance policy
point and is journaled durably by `forge-durable` before it takes effect.
Workspace services (files, git, search, syntax, context) and provider/config
crates sit underneath. `forge-types` holds the shared wire types. See
[ARCHITECTURE.md](./ARCHITECTURE.md) for the crate dependency diagram.

This file is the EGM contract index. The human-maintained
[README.md](./README.md) is left unchanged.

## Subdirectories & System Boundaries

- crates: [./crates/EGM.md](./crates/EGM.md) - All 19 workspace crates,
  grouped into application, agent runtime, workspace/policy services, and
  durability/shared types.
- install: [./install/EGM.md](./install/EGM.md) - POSIX installer that
  verifies SHA-256 and installs Linux sandbox dependencies, a WSL2-only
  Windows stub, and the installer test harness.

## Global Invariants

- Tool arguments are schema-validated before any handler runs. Every tool call
  is authorized at a single audited policy point (`authorize_tool_call`). An
  ACL deny cannot be overridden, and approve-all only upgrades HITL decisions.
- Provider keys never reach model-triggered child processes (shell, git, read-only
  git); custom profiles' key names are passed to the tool layer per session.
- HTTP clients that attach a credential never follow redirects to another host.
- File writes are confined to the workspace. Tools never write below `.git` or
  `.forge`. Shell confinement is fail-closed, child processes never inherit
  provider credentials, and sandboxed network traffic goes only through the
  deny-by-default egress proxy.
- Journal before side effect. Resume replays the journal deterministically and
  re-runs only idempotent incomplete intents.
- `ActiveTaskState` is the single source of truth for task lifecycle. A task
  completes only on runtime evidence.
- Each repository has at most one owner, enforced by an exclusive lease.
  Runtime data lives under `.forge/local/` (git-excluded) or the platform
  app-data directory.
- Secrets live only in the 0600 credential store and are never displayed or
  exported to the process env. Connecting a provider never selects a model.
- Credential precedence for a key provider: `FORGE_<ID>_API_KEY`, then the
  profile's own env names, then the stored key; an env key also wins over a
  stored OAuth login. Child processes get these names stripped from their
  environment (`strip_credentials`).
- The untrusted project config layer cannot set the workspace root, MCP
  servers, model base URL, or API key. Workspace permissions are deny-only.
- Journal tags and `ExitCode` discriminants are wire/process contracts.

## Global Side Effects

- Terminal takeover (raw mode, alternate screen, mouse) is restored on every
  exit path.
- Per-session SQLite journals, the repository control DB, the lease file,
  offload/progress files, and git worktrees, all under the runtime storage root.
- User-global config under `config_dir/forge`: `config.toml`, `trust.toml`,
  `permissions.toml`, `credentials.toml`, `preferences.toml`,
  `providers.toml`, and the model catalog.
- Outbound HTTPS to model providers, catalogs, OAuth endpoints, web
  fetch/search, and GitHub (`gh`).
- Subprocesses: shells, git, PTYs, MCP servers, external editor, subagents,
  and loopback egress listeners.

## System Verification

- Run: `cargo test --package <crate> --locked` (per crate; do not run the full
  workspace suite)
- Static: `cargo fmt --all -- --check` and
  `cargo clippy --workspace --all-targets --locked -- -D warnings`
