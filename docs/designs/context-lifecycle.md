# Context lifecycle design

**Status:** Draft  
**Owner:** Mohit Ranka  
**Last updated:** 22 Jul 2026  
**Phase:** **2 only** (exclusive)  
**PRD:** CTX-01, CTX-02  
**Architecture:** §4.6, §5.8, §14 Phase 2  
**Related:** [workspace-isolation.md](./workspace-isolation.md), [durable-execution.md](./durable-execution.md) (Phase 1 journal)

---

## 1. Problem / context

Long-horizon tasks rot the context window with tool dumps and history. Pure summarization leaves noise. Forge prioritizes **token budgeting**, **payload offload**, and **hard reset + handoff artifacts**.

## 2. Goals & non-goals

**Goals**

- Offload large tool responses to files; keep URI + short summary in-context (CTX-01).  
- At configurable usage threshold (default 80%), write handoff, clear window, rehydrate (CTX-02).  
- Preserve task alignment across 100+ turns via `progress.json` + `AGENTS.md` + git state.

**Non-goals**

- Perfect semantic memory of every prior token.  
- Vector DB memory as Phase 2 requirement.  
- Unlimited context via provider tricks only.

## 3. Design

### 3.1 Token accounting

- Estimate tokens for system, user, assistant, tool messages (provider tokenizer when available; heuristic fallback).  
- Update on each assembly and after each model `usage` event.  
- Expose usage to surfaces (sidebar meter, `/status`, `/cost`).

### 3.2 Payload offload (CTX-01)

**Trigger:** tool result estimated tokens &gt; `offload_token_threshold` (default 2000), or raw bytes &gt; optional byte cap.

**Actions:**

1. Write body under `.forge/offload/<session>/<id>.txt` (or hash-named).  
2. Record hash, bytes, token estimate, URI.  
3. In-context tool message becomes summary + URI, e.g.:

```text
[offloaded tool output — 12,400 tokens]
uri: file://.forge/offload/a1b2/tool_0x91a.txt
sha256: …
summary: first ~500 chars or model-generated one-liner (optional)
```

4. Journal references the same URI (not full body, or body only in blob store).

**Target:** ≥ 80% reduction in context bloat from large tool responses.

### 3.3 Hard reset + handoff (CTX-02)

**Trigger:** `usage >= reset_usage_ratio * capacity` (default 0.80), or user `/reset`.

**Algorithm:**

1. Build/update `progress.json` (see schema).  
2. Optionally append short operational notes to `AGENTS.md` only if policy allows auto-edit (default: update progress only; AGENTS.md human-owned unless configured).  
3. Journal `context_reset` with artifact pointers + git sha / worktree id.  
4. Clear active conversation messages from the in-memory window (journal retains history).  
5. Re-assemble: system policy + `AGENTS.md` + `progress.json` summary + workspace snapshot + empty/recent tail as configured.  
6. Continue loop.

**Recommendation:** Do not auto-rewrite `AGENTS.md` aggressively; prefer `progress.json` for task state.

### 3.4 progress.json schema

```json
{
  "version": 1,
  "goal": "string",
  "completed": ["..."],
  "in_progress": "string",
  "blockers": ["..."],
  "next_actions": ["..."],
  "workspace_ref": "git_sha_or_worktree_id",
  "session_id": "…",
  "updated_at": "ISO-8601"
}
```

**Default path:** workspace **`.forge/progress.json`** (configurable). Keep `progress.json` out of git unless the team opts in; recommend `.forge/` in `.gitignore` templates.

### 3.5 AGENTS.md loading

| When | Action |
|------|--------|
| Session start | Load if present at workspace root (or configured path) |
| Post-reset | Reload |
| Missing | Continue with empty project memory |

### 3.6 Compaction vs reset

| Strategy | Use |
|----------|-----|
| Offload | Always for large tools |
| Hard reset + handoff | Default long-horizon |
| Summary compaction (`/compact`) | Optional alternate; lower priority |

## 4. Interfaces

```rust
trait ContextEngine {
    fn usage(&self) -> Budget;
    fn assemble(&self, session: &Session) -> Vec<Message>;
    async fn ingest_tool_output(&mut self, out: ToolOutput) -> Ingested;
    async fn maybe_reset(&mut self, session: &mut Session) -> Result<bool, CtxError>;
}
```

## 5. Failure modes & edge cases

| Case | Behavior |
|------|----------|
| Offload disk full | Fail tool ingest; error to model |
| progress.json write fails | Abort reset; keep window; surface error |
| Threshold crossed mid-tool-batch | Finish tool pipeline; reset before next model call |
| Empty progress after reset | Still reload AGENTS.md + workspace |

## 6. Phase ownership

| Item | Phase |
|------|-------|
| This entire document | **2** |
| TUI commands | `/reset`, `/compact`, `/cost` (Phase 2 only) |
| Exit | Offload + handoff metrics met (CTX-01, CTX-02) |

## 7. Open questions

1. Auto-edit policy for `AGENTS.md` beyond “human-owned by default.”  
2. Whether `/compact` summary quality uses a dedicated small model call.  
3. Retention policy for offload blobs across sessions.

## Related docs

- [workspace-isolation.md](./workspace-isolation.md)  
- [tui-commands.md](./tui-commands.md) (`/reset`, `/compact`, `/cost`)  
