# forge-mcp/src

## Overview

Single-file implementation of the MCP stdio client bridge. It exposes the
client (`McpStdioClient`), a manager that spawns servers and registers their
tools (`McpManager`), namespaced tool wrappers (`RemoteMcpTool`,
`StaticMcpTool`), and the `search_tools` discovery meta-tool. Tests in this
file use a scripted fixture server; they are not part of the contract surface.

## Interface

- `McpError`: `Io`, `Json`, `Protocol(String)`, `Timeout(&'static str)`,
  `NotFound(String)`, `Sandbox(String)`.
- `mcp_tool_name(server_id, tool) -> String`, `parse_mcp_tool_name(name) ->
  Option<(&str, &str)>`.
- `StaticMcpTool { server_id, tool_name, description, schema, side_effect_class,
  handler }` and `register_static_mcp(&mut ToolRegistry, server_id,
  Vec<StaticMcpTool>)`: in-process MCP-like tools for tests/demos.
- `McpStdioClient`:
  - `spawn(&McpServerConfig)`,
  - `spawn_in_workspace(&McpServerConfig, &Path)`,
  - `list_tools() -> Result<Vec<McpToolInfo>, McpError>`,
  - `call_tool(name, Value) -> Result<ToolOutput, McpError>`,
  - `call_tool_attributed(name, Value, Option<&str>) -> McpCallOutcome`,
  - `server_id() -> &str`.
- `mcp_egress_label(server_id) -> String` (`mcp:<server>`).
- `McpCallOutcome { result, denied_hosts }` with
  `into_tool_result(server_id) -> Result<ToolOutput, ToolError>`.
- `McpToolInfo { name, description, input_schema }` (serde).
- `RemoteMcpTool`: `Tool` implementation forwarding to the client; carries the
  full namespaced name, remote name, and configured side-effect class.
- `McpManager`: `new`, `with_egress_policy`, `with_egress_shared`,
  `egress_shared`, `connect_all`, `connect_all_in_workspace`,
  `register_into(&mut ToolRegistry)`. `connect_all*` and `register_into`
  return per-server error strings (`"<id>: <error>"`); the session shows
  them as `mcp:` startup notices.
- `SearchToolsTool` and `install_search_tools(&mut ToolRegistry)`.

## Invariants

- Only `transport == "stdio"` is accepted; anything else is a `Protocol` error.
- `initialize` is sent with protocol version `2024-11-05`, then
  `notifications/initialized`, before any other request.
- Requests and notifications share one `request_lock`; ids are monotonically
  increasing per client. `request_inner` reads lines until the matching id,
  returning `Protocol("eof from mcp server")` at EOF and surfacing JSON-RPC
  `error` as `Protocol`.
- `tools/list` entries without a name are skipped; tools whose `inputSchema` is
  not a JSON object are skipped (fail closed).
- Each `tools/call` is bounded by a 10-second timeout and reports refused hosts
  recorded during its window; the label is cleared afterwards and stale
  refusals are drained before the call.
- When a call fails *and* a host was refused, `into_tool_result` converts it to
  `ToolError::SandboxDenied` naming the first host; a successful call is
  returned unchanged.
- `enabled_tools`/`disabled_tools` filter on the server's unnamespaced tool name.
- `register_into` visits servers in id order. A server whose `list_tools`
  fails is skipped and reported; the other servers still register. It
  registers each admitted tool and warns when an explicit `enabled_tools` list
  matches none.
- `search_tools` is registered only when at least one `mcp:` tool exists, uses
  a one-time catalog snapshot, matches case-insensitively on name/description,
  returns at most 20 results, and is a `Read` tool.
- The client's `ProcessGroupGuard` field is declared before the temp directory
  so the server's whole process group is killed before the directory is
  removed.
- Sandboxed launches use the workspace read profile, command/script install
  directories when resolvable, an explicit read-only workspace when
  configured, and fail the spawn (not silently narrow) on an ungrantable
  `sandbox_read_paths` entry.

## Side Effects

- Spawns MCP server subprocesses and writes JSON-RPC to their stdin.
- Kills the child's process group and deletes its private temp directory on
  drop.
- Starts an egress endpoint that can reach the network and logs `mcp:<server>`
  connections; mutates shared egress state (call labels, denied-host queue).
- Mutates the supplied `ToolRegistry` (`register_into`,
  `register_static_mcp`, `install_search_tools`).
- Emits `tracing` warnings on sandbox/egress degradation, skipped tools, and
  empty allowlists.

## Verification

- Run: `cargo test --package forge-mcp --locked`
