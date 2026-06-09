# Durable human-in-the-loop design

**Status:** Draft  
**Owner:** Mohit Ranka  
**Last updated:** 22 Jul 2026  
**Phase:** **2 only** (exclusive)  
**PRD:** DUR-03  
**Architecture:** §5.5, §14 Phase 2  
**Related:** [durable-execution.md](./durable-execution.md) (Phase 1 journal), [governance.md](./governance.md)

---

## 1. Problem / context

High-risk tools need human approval without holding compute and without losing session state across process restarts. Journal infrastructure is Phase 1; **HITL policy and wait/resume** are Phase 2.

## 2. Goals & non-goals

**Goals**

- `hitl_wait` / `hitl_resume` journal events.  
- Status `awaiting_hitl`; process may exit.  
- Approve/deny via TUI, ACP, or API; re-authorize before execute.

**Non-goals**

- Journal/replay basics → [durable-execution.md](./durable-execution.md) (Phase 1).  
- Which tools are high-risk → [governance.md](./governance.md) policy classify.

## 3. Design

1. Governance classifies tool call as HITL → append `hitl_wait` (redacted payload).  
2. Set `awaiting_hitl`; release compute.  
3. On approve → append `hitl_resume` → re-authorize → execute → `tool_result`.  
4. On deny → journal denial; model sees structured denial.

**TUI commands (Phase 2 only):** `/approve`, `/deny` (not in Phase 1 command catalog).

## 4. Interfaces

```rust
async fn request_hitl(session: &Session, payload: HitlPayload) -> Result<(), HitlError>;
async fn resolve_hitl(session_id: &str, decision: HitlDecision) -> Result<(), HitlError>;
```

## 5. Failure modes

| Case | Behavior |
|------|----------|
| Resume without pending wait | Error no-op |
| Process restart mid-wait | Replay restores `awaiting_hitl` |
| Approve then policy now denies | Fail closed; do not execute |

## 6. Phase ownership

| Item | Phase |
|------|-------|
| This entire document | **2** |
| Depends on | Phase 1 journal (DUR-01/02) |

## Related docs

- [durable-execution.md](./durable-execution.md)  
- [governance.md](./governance.md)  
- [protocol-acp.md](./protocol-acp.md)  
