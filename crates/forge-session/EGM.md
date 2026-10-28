# forge-session

## Overview

Crate root for frontend-independent session assembly and repository-level
multi-session supervision. No source code at this level: everything lives in
`src/`.

## Subdirectories & Boundaries

- `src/`: [./src/EGM.md](./src/EGM.md) — `open_session`, journal resolution, credential injection, SQLite control roster + exclusive lease, headless turn driver, snapshots, and the supervisor command/event API.

## Interface

- Session assembly: `SessionTarget`, `OpenedSession`, `open_session`,
  `resolve_journal_dir`, `connect_credentials`.
- Repository control: `RepositoryControl`, `RepositoryLease`,
  `RepositorySession`, and related lifecycle enums.
- Headless driver: `run_headless`, `ApprovalPolicy`, `ApprovalRequired`.
- Snapshots: `SessionSnapshot`, `SessionDetailsSnapshot`,
  `TranscriptSnapshot`, `replayed_transcript`.
- Supervisor: `RepositoryBootstrap`, `RepositorySupervisor`,
  `SupervisorHandle`, `SupervisorCommand`, `SupervisorEvent`.
- Full list: [src/EGM.md](./src/EGM.md).

## Invariants

- One Forge process owns a repository group through an exclusive file lease.
- Credentials are injected into the model client and never exported into
  `std::env`.
- Repo-scope `allow` pattern rules are never honored; deny always wins.
- Headless approve-all never approves sandbox escalation.

## Side Effects

- SQLite control database and lease file under the repository control
  directory; session journal directories.
- Starts MCP server processes; runs git worktree operations; spawns
  per-session supervisor tasks.
- Reads user config, permission files, and credential stores.

## Notes

- This crate sits above `forge-core` because assembly needs `forge-mcp` and `forge-connect`, which `forge-core` deliberately does not depend on.
- It is the only place that may compose governance from workspace permissions and adopt a shared egress state for MCP servers.

## Verification

- Run: `cargo test --package forge-session --locked`
