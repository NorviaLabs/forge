# Agent loop design

**Status:** Shipped (product)  
**Owner:** Mohit Ranka  
**Last updated:** 22 Jul 2026  
**Phase:** **1 only** (exclusive)  
**PRD:** CORE-01 (with tools), DUR-01/02 (journal hooks)  
**Architecture:** §5 Runtime flows (Phase 1 path)  
**Related:** [tool-protocol.md](./tool-protocol.md), [durable-execution.md](./durable-execution.md), [model-providers.md](./model-providers.md)

---

## 1. Problem / context

Drive a reliable plan–act–observe cycle for the Phase 1 coding agent: assemble context, call model, execute tools, terminate cleanly—without graph DSLs.

## 2. Goals & non-goals

**Goals**

- Single Generator loop for Phase 1 sessions.  
- Journal-before-side-effect for model and tool steps.  
- Sequential tool calls within a turn.  
- Termination via max turns, cancel, or final assistant message without tools.

**Non-goals (other phases own these)**

- Context budget hard-reset → [context-lifecycle.md](./context-lifecycle.md) (Phase 2).  
- Durable HITL pause → [durable-hitl.md](./durable-hitl.md) (Phase 2).  
- Evaluator gate → [feedback-evaluator.md](./feedback-evaluator.md) (Phase 3).  
- Surfaces implementing their own loops.

Phase 1 may leave **extension points** (hooks) that later phases register; Phase 1 must run with hooks no-op.

## 3. Design

### 3.1 Phase 1 loop

```text
while session.status == running:
  messages = context.assemble()          # simple window; no Phase 2 handoff
  journal(model_request)
  outcome = model.complete(messages)
  journal(model_response)
  if outcome.tool_calls:
    for call in tool_calls:              # sequential
      validate → journal tool_intent → execute → journal tool_result
  if terminal_condition: break
```

### 3.2 Statuses (Phase 1)

| Status | Meaning |
|--------|---------|
| `running` | Active loop |
| `completed` | Success terminal |
| `failed` | Error terminal |

`awaiting_hitl` is **Phase 2** only.

### 3.3 Extension points (no-op in Phase 1)

Later phases may register:

- `before_model`: context reset (Phase 2)  
- `after_tools`: eval gate (Phase 3)  
- `on_tool_policy`: HITL (Phase 2)  

Phase 1 binary must not require these modules at runtime.

### 3.4 Feedforward constraints

- `max_turns`  
- Simple deny-tool name list (optional config)  
- Full ACL/vault is Phase 2  

## 4. Interfaces

- `submit_user_message`, `cancel`, drive-until-terminal (headless)

## 5. Failure modes

| Case | Behavior |
|------|----------|
| Tool failure | Journal failure; model sees error; continue unless fatal |
| Cancel mid-tool | Cooperative cancel; incomplete intent fail-safe |
| Provider down | Session failed |

## 6. Phase ownership

| Item | Phase |
|------|-------|
| This entire document | **1** |
| Exit | Multi-turn coding task completes in TUI/headless with tools + journal |

## 7. Open questions

1. Explicit `end_turn` vs implicit final message.  
2. Parallel tools (post–Phase 1 only).

## Related docs

- [durable-execution.md](./durable-execution.md)  
- [tool-protocol.md](./tool-protocol.md)  
