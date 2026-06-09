# ACP protocol design

**Status:** Draft  
**Owner:** Mohit Ranka  
**Last updated:** 22 Jul 2026  
**Phase:** **2 only** (exclusive)  
**PRD:** CORE-03  
**Architecture:** §8, §14 Phase 2  
**Related:** [surfaces.md](./surfaces.md) (Phase 1 surfaces), [durable-hitl.md](./durable-hitl.md)

---

## 1. Problem / context

IDE clients need an open transport to the same durable agent core used by TUI/headless—without a second agent implementation.

## 2. Goals & non-goals

**Goals**

- ACP adapter is a **surface**: ACP messages ↔ core session APIs + agent events.  
- Full sessions: stream, tools visibility, resume IDs, HITL mapping.  
- No direct model or MCP calls from the ACP layer.

**Non-goals**

- MCP (Phase 1) → [protocol-mcp.md](./protocol-mcp.md).  
- Channel protocols (Phase 3) → [channels.md](./channels.md).  
- Replacing TUI/headless.

## 3. Design

```text
IDE --ACP--> forge-acp --> core loop --> tools (built-in | MCP)
```

| Concern | Behavior |
|---------|----------|
| Session | Create/resume same `session_id` / journal |
| Stream | Map agent events to ACP stream constructs |
| HITL | Map durable wait to ACP approval UX ([durable-hitl.md](./durable-hitl.md)) |
| Secrets | Never collect long-lived keys in IDE chat |

## 4. Interfaces

```rust
// Phase 2 crate forge-acp
async fn serve_acp(handle: AgentHandle) -> Result<(), AcpError>;
```

## 5. Failure modes

| Case | Behavior |
|------|----------|
| Client disconnect | Config: keep session vs fail (default: keep journaled session) |
| Protocol version skew | Clear error; no silent partial protocol |

## 6. Phase ownership

| Item | Phase |
|------|-------|
| This entire document | **2** (first Phase 2 build step) |
| Exit | One ACP client completes a multi-tool durable session |

## 7. Open questions

1. Minimal ACP feature subset for v1 (streaming, permissions, HITL).  
2. Multi-client attach to one session (default: single client).

## Related docs

- [durable-hitl.md](./durable-hitl.md)  
- [governance.md](./governance.md)  
- [../architecture.md](../architecture.md) §8  
