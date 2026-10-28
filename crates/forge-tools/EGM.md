# forge-tools

## Overview

Schema-validated tool protocol crate. It provides the `Tool`/`ToolRegistry`
abstraction used by the agent loop and the built-in implementations for file
access, shell execution, patching, search, web access, skills, sandboxing, and
egress control. The crate contains no direct source files at this level; code
lives in `src/`.

## Subdirectories & Boundaries

- [src](./src/EGM.md) — tool protocol, registry, built-ins, sandbox, and egress.

`tests/` is integration-test code and is intentionally not documented as a
contract directory.

## Interface

- Child component contract: [`./src/EGM.md`](./src/EGM.md) — `Tool` trait,
  `ToolRegistry`/`ToolContext`, every built-in tool, validation, sandbox
  (`SandboxPolicy`), shell spawn (`ShellSpawnSpec`), and egress
  (`EgressPolicy`, `EgressProxy`, `CallGateway`).
- Nested contract: [`./src/web_search/EGM.md`](./src/web_search/EGM.md) —
  `web_search` tool and `SearchBackend` extension point.

## Invariants

- Tool arguments are schema-validated in `ToolRegistry` before any handler
  runs; validation failures never execute a tool.
- File tools are confined to the canonical workspace root (plus an optional
  session temp directory) and never write below `.git` or `.forge`.
- Shell confinement is fail-closed, child processes never inherit provider
  credentials, and network access is routed through the domain-filtered egress
  proxy/gateway.

## Side Effects

- Workspace file reads/writes, subprocess spawning (shells, git, PTYs,
  background jobs), loopback egress listeners and Unix sockets, per-session
  temp directories, and outbound network for `web_fetch`, `web_search`, and
  network `git`. See the child contract for detail.

## Verification

- Run: `cargo test --package forge-tools --locked`
