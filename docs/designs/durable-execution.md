# Durable execution design

**Status:** Draft  
**Owner:** Mohit Ranka  
**Last updated:** 22 Jul 2026  
**Phase:** **1 only** (exclusive)  
**PRD:** DUR-01, DUR-02  
**Architecture:** §4.5 Event journal, §5.4  
**Related:** [agent-loop.md](./agent-loop.md), [tool-protocol.md](./tool-protocol.md)  
**Not this doc:** Durable HITL (DUR-03) → [durable-hitl.md](./durable-hitl.md) (Phase 2)

---

## 1. Problem / context

Agent runs cross process lifetimes. Crashes mid-tool or mid-model must not double-execute side effects or lose session state. Classic workflow engines lack LLM/tool semantics; Forge embeds an **LLM-aware event journal**.

## 2. Goals & non-goals

**Goals**

- Append-only journal; **record intent before side effects**.  
- Replay restores state; completed tool/model steps are cached, not re-run.  
- Phase 1 storage: SQLite via sqlx.  
- Multi-instance DB backends are out of Phase 1 (future; not Phase 2 HITL).

**Non-goals**

- Exactly-once for non-idempotent external systems beyond “don’t re-call on replay.”  
- Replacing Temporal for arbitrary business workflows outside the agent harness.  
- Mutating historical journal rows (except optional vacuum of blobs by policy).

## 3. Design

### 3.1 Event envelope

| Field | Description |
|-------|-------------|
| `seq` | Monotonic per-session sequence |
| `session_id` | Stable resume key |
| `ts` | UTC timestamp |
| `type` | Event kind |
| `schema_version` | Envelope version |
| `payload` | Typed JSON |
| `trace_id` | Optional OTEL correlation |

### 3.2 Event kinds (Phase 1)

| Type | Purpose |
|------|---------|
| `session_created` | Bootstrap metadata |
| `user_message` | Operator input |
| `model_request` | Messages hash / refs + tool list snapshot |
| `model_response` | Content and/or blob refs + usage |
| `tool_intent` | Name + args (redacted) **before** execute |
| `tool_result` | Success/failure payload |
| `tool_validation_failed` | Schema rejection |
| `state_patch` | Small non-prompt state |
| `session_status` | Status transitions |

**Not in Phase 1** (defined elsewhere): `hitl_wait` / `hitl_resume` → [durable-hitl.md](./durable-hitl.md); `context_reset` → [context-lifecycle.md](./context-lifecycle.md).

### 3.3 Record-before-side-effect (DUR-01)

```text
append tool_intent  →  fsync/commit  →  execute  →  append tool_result
append model_request → commit → call provider → append model_response
```

Write latency target &lt; 5 ms per step (NFR); use SQLite WAL and batched fsync policy tuned carefully (open: durability vs latency tradeoff).

### 3.4 Replay algorithm (DUR-02)

1. Open journal for `session_id`.  
2. Scan events in order; rebuild in-memory session (messages refs, status, cursor).  
3. For each `tool_intent`:  
   - If matching `tool_result` exists → use cached result; **do not execute**.  
   - If no result → **fail-safe**: mark failed, or retry **only if** tool declared `idempotent` and policy allows.  
4. For completed `model_response` → do not re-call LLM.  
5. Resume loop at next pending work (Phase 1 has no HITL wait state).

### 3.5 Storage (Phase 1)

SQLite under `.forge/sessions/` (per-session or multi-session file—open question). Large payloads may be referenced by URI only when Phase 2 offload exists; Phase 1 may truncate or refuse oversized tool bodies.

## 4. Interfaces

```rust
trait Journal {
    async fn append(&self, event: Event) -> Result<Seq, JournalError>;
    async fn replay(&self, session_id: &str) -> Result<ReplayState, JournalError>;
    async fn last_seq(&self, session_id: &str) -> Result<Seq, JournalError>;
}
```

## 5. Failure modes & edge cases

| Case | Behavior |
|------|----------|
| Crash after intent, before result | Fail-safe incomplete intent |
| Crash after result, before model sees it | Replay restores result into context rebuild |
| Disk full | Fail closed; surface error |
| Corrupt tail | Truncate to last valid seq if checksum/length allows; else refuse resume |

## 6. Phase ownership

| Item | Phase |
|------|-------|
| This entire document | **1** |
| Exit | Kill -9 mid-task; resume without re-executing completed tools/model steps |

## 7. Open questions

1. One DB file per session vs single multi-session DB.  
2. Journal retention default.  
3. Full messages vs content-addressed blobs in `model_request`.

## Related docs

- [agent-loop.md](./agent-loop.md)  
- [durable-hitl.md](./durable-hitl.md) (Phase 2 — not implemented here)  
