# MCP protocol design

**Status:** Draft  
**Owner:** Mohit Ranka  
**Last updated:** 22 Jul 2026  
**Phase:** **1 only** (exclusive)  
**PRD:** CORE-02  
**Architecture:** §2, §14 Phase 1  
**Related:** [tool-protocol.md](./tool-protocol.md), [surfaces.md](./surfaces.md)

---

## 1. Problem / context

Tool ecosystems standardize on **MCP**. Phase 1 product must call external tool servers through the same registry/validation path as built-ins.

## 2. Goals & non-goals

**Goals**

- Discover and call MCP tools via the shared tool registry.  
- Declarative MCP server config.  
- Fail closed on missing schemas.

**Non-goals (other phases / docs)**

- ACP clients → [protocol-acp.md](./protocol-acp.md) (Phase 2).  
- Channel adapters → [channels.md](./channels.md) (Phase 3).  
- Enterprise ACL/vault detail → [governance.md](./governance.md) (Phase 2); Phase 1 may use allow-all local principal.

## 3. Design

### 3.1 Role

```text
TUI / headless --> core loop --> built-ins | MCP tools
```

### 3.2 Config

```toml
[[mcp.servers]]
id = "browser"
transport = "stdio"   # http later if needed within Phase 1 scope as optional transport
command = "npx"
args = ["-y", "some-mcp-server"]
```

### 3.3 Lifecycle

1. Spawn/connect server.  
2. `tools/list` → registry entries + JSON Schema.  
3. Optional local deny list (simple); full dynamic ACL is Phase 2.  
4. `tools/call` after validate + journal intent (DUR-01).  
5. Map MCP errors to `tool_result` failures.

### 3.4 Naming

Namespace remote tools by server id to avoid collisions (e.g. `mcp:<server_id>:<tool>`).

## 4. Interfaces

```rust
trait McpManager {
    async fn refresh(&self) -> Result<Vec<ToolRegistration>, McpError>;
    async fn call(&self, name: &str, args: Value) -> Result<ToolOutput, McpError>;
}
```

## 5. Failure modes

| Case | Behavior |
|------|----------|
| Server crash mid-call | tool failure; journal error result |
| Schema missing | **Do not expose** tool |
| Duplicate names | Namespace by server id |

## 6. Phase ownership

| Item | Phase |
|------|-------|
| This entire document | **1** |
| Exit | MCP list/call works with ≥1 server in TUI/headless |

## 7. Open questions

1. HTTP MCP in Phase 1 vs stdio-only.  
2. Exposing Forge built-ins as an MCP server (out of Phase 1).

## Related docs

- [tool-protocol.md](./tool-protocol.md)  
- [durable-execution.md](./durable-execution.md)  
- [protocol-acp.md](./protocol-acp.md) (Phase 2 — do not implement here)  
