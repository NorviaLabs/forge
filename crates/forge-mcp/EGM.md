# forge-mcp

## Overview

MCP (Model Context Protocol) client bridge for Forge. The crate spawns MCP
servers over stdio, speaks minimal JSON-RPC (`initialize`, `tools/list`,
`tools/call`), registers their tools into a `ToolRegistry` under namespaced
names, and confines sandboxed servers with the same sandbox/egress machinery
the shell tool uses.

All code lives in `src/lib.rs`.

## Subdirectories & System Boundaries

- `src`: Component package with the client, manager, namespacing helpers,
  static test tools, and the `search_tools` meta-tool. Contract:
  [`./src/EGM.md`](./src/EGM.md).
- Depends on `forge-config` for `McpServerConfig`, `forge-tools` for
  `Tool`/`ToolRegistry`/sandbox/egress, and `forge-types` for tool/wire types.

## Interface

- Child component contract: [`./src/EGM.md`](./src/EGM.md) — the public
  symbols, invariants, and side effects implemented by this crate.
- Crate-level façade exports and verification target are documented there.

## Invariants

- MCP tool wire names are always `mcp:<server_id>:<tool>`; parsing rejects
  names without the prefix or without a second `:`.
- `sandbox = true` fails closed: if the host cannot confine, the profile cannot
  be built, or the private temp directory cannot be created, the server is not
  started and no unconfined fallback exists.
- A sandboxed server gets network only through the session egress endpoint; if
  that endpoint cannot start, the server runs with no network.
- Requests to one server are strictly sequential (one request on the wire at a
  time) and bounded by a 10-second I/O timeout.
- Tool side-effect class comes from the server's configured class, never
  assumed `Read`; unknown config falls back to `Exec`.
- No secrets are read or written by this crate; server env comes from config.

## Side Effects

- Spawns external MCP server processes (stdio) and kills their whole process
  group on drop of the client.
- Creates and removes a private temp directory per sandboxed server.
- Listens on a per-server egress proxy endpoint while a sandboxed server lives
  and records denied hosts in the shared session egress state.
- Registers tools into the caller's `ToolRegistry` and sends JSON-RPC over the
  child's stdin/stdout.

## Verification

- Run: `cargo test --package forge-mcp --locked`
