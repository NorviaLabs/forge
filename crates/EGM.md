# crates

## Overview

Every Rust crate in the Forge workspace (the 19 members listed in the root
`Cargo.toml`). Each crate directory holds `Cargo.toml` and `src/`, plus a few
non-Rust extras: `forge-tui` has a `build.rs` and `themes/` (the built-in
`forge-dark.toml` and `forge-light.toml`), and `forge-connect` has
`providers/` (the five built-in provider definitions embedded with
`include_str!`). Each crate links to the contract
for its `src/` directory. `tests/` directories hold integration tests only and
have no contracts.

## Subdirectories & Boundaries

Application boundary:

- forge-cli: [./forge-cli/EGM.md](./forge-cli/EGM.md) - The `forge` binary.
  Handles clap argv, resume/continue/fork, headless `bench`, and the hidden
  `__egress-connect` helper. The TUI acquires the repository lease before
  opening the primary session, falling back to a lease-less session when
  runtime storage or lease setup fails; `bench` writes only a journal and
  takes none.
- forge-tui: [./forge-tui/EGM.md](./forge-tui/EGM.md) - The full-screen
  ratatui UI: `TuiApp`, the event loop, overlays, connect/model picker, editor,
  explorer, and widgets. `ExitCode` 0..4 is a process contract.
- forge-session: [./forge-session/EGM.md](./forge-session/EGM.md) - Session
  assembly (`open_session`), the exclusive repository lease and SQLite control
  DB, `RepositorySupervisor`, headless runs, and transcript snapshots.

Agent runtime:

- forge-core: [./forge-core/EGM.md](./forge-core/EGM.md) - `AgentSession`
  agent loop. `ActiveTaskState` lifecycle, evidence-based completion, the HITL
  and grant flow, a single audited `authorize_tool_call`, the queue, background
  tasks, subagents, and egress runtime.
- forge-transcript: [./forge-transcript/EGM.md](./forge-transcript/EGM.md) -
  Pure, deterministic projection of messages and turn events into chat items
  and presentation structs.
- forge-tools: [./forge-tools/EGM.md](./forge-tools/EGM.md) - Tool trait,
  registry, schema validation, built-in tools, the sandbox (Seatbelt and
  bubblewrap), the shell spawn spec, egress proxy/gateway, and `web_search`.
- forge-mcp: [./forge-mcp/EGM.md](./forge-mcp/EGM.md) - MCP stdio client.
  Tools are registered as `mcp:<server>:<tool>`, servers are sandboxed and fail
  closed, and the crate provides the `search_tools` meta-tool.
- forge-model: [./forge-model/EGM.md](./forge-model/EGM.md) - `ModelClient`
  trait, copy-on-write `SharedMessages`, wire normalization, prompt caching,
  and native Anthropic/Codex/OpenAI-compatible transports.

Workspace and policy services:

- forge-context: [./forge-context/EGM.md](./forge-context/EGM.md) -
  `ContextEngine`, which handles tool-output offload, the progress document,
  AGENTS.md and skill discovery. It also contains the pure compaction planner.
- forge-workspace: [./forge-workspace/EGM.md](./forge-workspace/EGM.md) -
  Workspace-confined file ops, attachments, git status/review/sync, the GitHub
  read adapter, and the `gh` mutations and merge gate.
- forge-search: [./forge-search/EGM.md](./forge-search/EGM.md) -
  `WorkspaceIndex` file/content search and Quick Open scoring. Read-only.
- forge-syntax: [./forge-syntax/EGM.md](./forge-syntax/EGM.md) - Tree-sitter
  language detection and highlighting, with a bounded global LRU cache.
- forge-governance: [./forge-governance/EGM.md](./forge-governance/EGM.md) -
  ACL, pattern rules, HITL gating, session grants, argument redaction, and the
  in-memory audit log.
- forge-connect: [./forge-connect/EGM.md](./forge-connect/EGM.md) - Provider
  profiles and registry, a 0600 credential store, OAuth, model catalog, and
  routes. Connecting never selects a model.
- forge-config: [./forge-config/EGM.md](./forge-config/EGM.md) - Layered TOML
  config with an untrusted project layer, deny-only workspace permissions,
  trust, themes, and pane layout.

Durability and shared contracts:

- forge-durable: [./forge-durable/EGM.md](./forge-durable/EGM.md) -
  Append-only per-session SQLite journal, deterministic replay with
  checkpoints, and the audit table.
- forge-storage: [./forge-storage/EGM.md](./forge-storage/EGM.md) - Resolves
  where runtime data lives (`.forge/local/` or app-data), manages the git
  exclude block and legacy migration, and handles the worktree lifecycle.
- forge-types: [./forge-types/EGM.md](./forge-types/EGM.md) - Shared message,
  tool, journal, governance, and plan/question types, IDs, and image sniffing.
  Journal tags are wire contracts.
- forge-test-support: [./forge-test-support/EGM.md](./forge-test-support/EGM.md)
  - Dev-only fixtures: a scripted HTTP mock and git repository helpers.
