# Protocols design (MCP + ACP)

**Status:** Draft  
**Owner:** Mohit Ranka  
**Last updated:** 22 Jul 2026  
**PRD:** CORE-02  
**Architecture:** §2, §8, decisions #13  
**Related:** [tool-protocol.md](./tool-protocol.md), [surfaces.md](./surfaces.md), [governance.md](./governance.md)

---

## 1. Problem / context

Tool ecosystems standardize on **MCP**. Client/IDE transport is fragmenting; **ACP** is the open direction for IDE/TUI-shaped clients. Forge natively speaks both: MCP for tools, ACP for clients—without proprietary lock-in.

## 2. Goals & non-goals

**Goals**

- Discover and call MCP tools through the same registry/ACL/validation path as built-ins.  
- Serve ACP clients with the same agent loop and journal (no second agent implementation).  
- Phase 1: **MCP** (required). Phase 2: **ACP** (required; first Phase 2 deliverable). CORE-02 is complete only after Phase 2. Matches PRD §13 and architecture decision #13.

**Non-goals**

- Implementing every MCP transport variant on day one.  
- Making the channel gateway (Slack/etc.) use ACP (Phase 3 has its own adapters).  
- Forking MCP/ACP specs long-term.

## 3. Design

### 3.1 Split of concerns

| Protocol | Role in Forge |
|----------|----------------|
| **MCP** | Tool servers: list + call external capabilities |
| **ACP** | Client protocol: IDE (and similar) drive sessions, stream agent events |
| **In-process** | Built-in tools + TUI/headless surfaces |

```text
IDE --ACP--> surfaces/acp --> core loop --> tools (built-in | MCP)
TUI --------> surfaces/tui  --> core loop ----^
```

### 3.2 MCP

**Config (declarative):**

```toml
[[mcp.servers]]
id = "browser"
transport = "stdio"   # stdio | http (phase)
command = "npx"
args = ["-y", "some-mcp-server"]
```

**Lifecycle**

1. Spawn/connect server.  
2. `tools/list` → convert to registry entries with JSON Schema.  
3. ACL filter before model sees tools.  
4. `tools/call` only after validate + authorize + journal intent.  
5. Map MCP errors to `tool_result` failures.

**Security**

- MCP servers run with least privilege; do not pass vault secrets into server env unless explicitly mapped.  
- Tool results subject to offload and redaction rules.

### 3.3 ACP

- ACP adapter is a **surface**: translates ACP messages ↔ core session APIs and agent events.  
- Does not call model or MCP directly.  
- Session resume IDs and HITL must be expressible over ACP (map to protocol constructs as the ACP surface matures).

### 3.4 Phase depth (deterministic)

| Phase | Protocol depth | Exit |
|-------|----------------|------|
| **1** | Built-ins + loop + journal + **MCP** bridge | MCP list/call through registry |
| **2** | **ACP** server/session (first Phase 2 item) | IDE client drives same loop; CORE-02 complete |

## 4. Interfaces

- `McpManager::refresh() -> Vec<ToolRegistration>`  
- `McpManager::call(name, args) -> ToolOutput`  
- `AcpServer::serve(core: AgentHandle)`  

## 5. Failure modes & edge cases

| Case | Behavior |
|------|----------|
| MCP server crash mid-call | tool failure; journal result error |
| MCP tool schema missing | skip or expose as unvalidated? **Fail closed: do not expose** |
| ACP client disconnect | session may continue headless or pause per config |
| Duplicate tool names across servers | namespace by server id |

## 6. Phase / rollout

See table in §3.4. Channel protocols are **not** ACP (Phase 3 adapters).

## 7. Open questions

1. HTTP MCP auth patterns (OAuth) timeline.  
2. ACP feature subset for Phase 2 (streaming, permissions, HITL mapping).  
3. Whether Forge also **exposes** an MCP server facade of its built-ins (out of scope initially).

## Related docs

- [tool-protocol.md](./tool-protocol.md)  
- [surfaces.md](./surfaces.md)  
- [../architecture.md](../architecture.md)  
