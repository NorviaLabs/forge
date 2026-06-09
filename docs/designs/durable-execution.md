# Durable execution design

**Status:** Draft  
**Owner:** Mohit Ranka  
**Last updated:** 22 Jul 2026  
**PRD:** DUR-01, DUR-02, DUR-03  
**Architecture:** §4.5 Event journal, §5.4–5.5  
**Related:** [agent-loop.md](./agent-loop.md), [tool-protocol.md](./tool-protocol.md), [governance.md](./governance.md)

---

## 1. Problem / context

Agent runs cross process lifetimes. Crashes mid-tool or mid-model must not double-execute side effects or lose session state. Classic workflow engines lack LLM/tool semantics; Forge embeds an **LLM-aware event journal**.

## 2. Goals & non-goals

**Goals**

- Append-only journal; **record intent before side effects**.  
- Replay restores state; completed tool/model steps are cached, not re-run.  
- Durable HITL without holding compute.  
- Phase 1: SQLite via sqlx; later shared DB for multi-instance.

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

### 3.2 Event kinds (initial)

| Type | Purpose |
|------|---------|
| `session_created` | Bootstrap metadata |
| `user_message` | Operator/channel input |
| `model_request` | Messages hash / refs + tool list snapshot |
| `model_response` | Content and/or blob refs + usage |
| `tool_intent` | Name + args (redacted secrets) **before** execute |
| `tool_result` | Success/failure payload or offload ref |
| `tool_validation_failed` | Schema rejection |
| `state_patch` | Small non-prompt state |
| `hitl_wait` | Approval payload (redacted) |
| `hitl_resume` | approve/deny + actor |
| `context_reset` | Handoff artifact pointers |
| `session_status` | Status transitions |
| `checkpoint` | Optional compaction marker |

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
5. If last status is `awaiting_hitl`, restore wait; do not auto-approve.  
6. Resume loop at next pending work.

### 3.5 Durable HITL (DUR-03)

1. Policy requires approval → append `hitl_wait` → status `awaiting_hitl`.  
2. Process may exit; no busy wait.  
3. Approval arrives (TUI/ACP/API) → append `hitl_resume` → re-authorize → execute (new `tool_intent` if needed for clarity—or continue original intent if still open; prefer **explicit** resume then execute with journal linkage).  
4. Deny → append result denied; model informed.

**Recommendation:** On approve, journal a linked execute path so audit is clear; never execute without a post-resume authorization check.

### 3.6 Storage

| Phase | Backend |
|-------|---------|
| 1 | SQLite file per session or shared file with `session_id` index (`.forge/sessions/`) |
| 2+ | Postgres option for multi-instance |

Payloads larger than a threshold store as offload blobs; journal keeps URI + hash.

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

## 6. Phase / rollout

| Phase | Scope |
|-------|-------|
| 1 | SQLite journal, replay, session resume IDs (DUR-01, DUR-02) |
| 2 | HITL wait/resume (DUR-03), stronger redaction |
| 3 | Multi-instance backend (Postgres), export hooks |

## 7. Open questions

1. One DB file per session vs single multi-session DB.  
2. How long to retain journals by default.  
3. Whether model_request stores full messages or content-addressed blobs only.

## Related docs

- [agent-loop.md](./agent-loop.md)  
- [context-lifecycle.md](./context-lifecycle.md)  
- [observability.md](./observability.md)  
