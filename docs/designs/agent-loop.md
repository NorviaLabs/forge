# Agent loop design

**Status:** Draft  
**Owner:** Mohit Ranka  
**Last updated:** 22 Jul 2026  
**PRD:** CORE-01 (tools), DUR-*, CTX-*, EVAL-01 (gates)  
**Architecture:** §5 Runtime flows  
**Related:** [tool-protocol.md](./tool-protocol.md), [durable-execution.md](./durable-execution.md), [context-lifecycle.md](./context-lifecycle.md)

---

## 1. Problem / context

The harness must drive a reliable plan–act–observe cycle: assemble context, call the model, execute tools, manage budget/HITL/eval gates, and terminate cleanly—without graph DSLs.

## 2. Goals & non-goals

**Goals**

- Single clear control loop for Generator sessions (Evaluator is a separate session; see feedback design).  
- Journal-before-side-effect for model and tool steps.  
- Composable gates: context reset, HITL, evaluation, max turns.  
- Prunable complexity (decaying scaffolding): gates are config-optional where possible.

**Non-goals**

- Multi-agent graph runtime as the primary API.  
- Holding compute during durable HITL waits.  
- Surfaces implementing their own loops.

## 3. Design

### 3.1 Loop sketch

```text
while session.status == running:
  if context_usage >= threshold:
    handoff_reset()                    # CTX-02
  messages = context.assemble()
  journal(model_request)
  outcome = model.complete(messages)   # stream to surface
  journal(model_response metadata + content refs)
  if outcome.tool_calls:
    for call in tool_calls:
      run_tool_pipeline(call)          # validate → ACL → HITL? → exec
      if session.status == awaiting_hitl: return
  if eval_gate_enabled and at_boundary:
    feedback.run_gate()                # may enqueue repairs
  if terminal_condition: break
```

### 3.2 Session statuses

| Status | Meaning |
|--------|---------|
| `running` | Active loop |
| `awaiting_hitl` | Paused; no compute reservation |
| `completed` | Success terminal |
| `failed` | Error terminal |
| `compacted` | Optional marker after hard reset mid-task (still `running` afterward) |

### 3.3 Termination conditions

Any of:

- Model returns final assistant message with **no** tool calls and task considered done (heuristic: no further user message; optional success criteria later).  
- `max_turns` reached (config).  
- Unrecoverable error (provider down, cancel).  
- User `/cancel` or surface cancel.  
- Explicit success criteria (Phase 3+ with Evaluator).

### 3.4 Tool pipeline (per call)

1. Schema validate ([tool-protocol](./tool-protocol.md)).  
2. Journal `tool_intent` (**before** side effects).  
3. Governance: ACL + policy class (allow / deny / hitl).  
4. If HITL → journal `hitl_wait`, set `awaiting_hitl`, **return** (process may exit).  
5. Vault inject → sandbox execute.  
6. Journal `tool_result` (or failure).  
7. Context ingest or offload.

### 3.5 Ordering guarantees

- No model call or tool execute without a prior journal record of intent (DUR-01).  
- Tool calls in one model turn run **sequentially** in Phase 1 (simpler reasoning about journal order). Parallel tools are an open question for later.  
- Context assembly is the only rewriter of the next prompt payload.

### 3.6 Feedforward constraints

Applied before listing tools / calling model:

- `disallowed_tools` / ACL deny sets  
- `max_turns`, optional cost/token caps  
- sandbox profile for the session  

## 4. Interfaces

Surface → core:

- `submit_user_message(session_id, text)`  
- `cancel(session_id)`  
- `resume_hitl(session_id, decision)`  
- `tick` / drive until pause or terminal (headless)

Core emits **agent events** (architecture §4.4) for surfaces; does not know about ratatui widgets.

## 5. Failure modes & edge cases

| Case | Behavior |
|------|----------|
| Stream interrupted mid-model | Journal partial/error; fail or resume policy (open) |
| Tool fails | Journal failure; model sees error tool message; loop continues unless fatal |
| Nested cancel during tool | Cooperative cancel; journal incomplete intent |
| Eval gate fails open? | Never; sensor failure → treat as fail + report |

## 6. Phase / rollout

| Phase | Scope |
|-------|-------|
| 1 | Loop + built-ins + journal + streams; sequential tools |
| 2 | Handoff reset integration, durable HITL, worktree paths |
| 3 | Eval gate default-off; multi-channel ingress still uses same loop |

## 7. Open questions

1. Parallel tool execution within a turn.  
2. Whether “final answer” is explicit (`end_turn`) or implicit.  
3. Mid-stream crash: treat as failed model step vs retry.

## Related docs

- [durable-execution.md](./durable-execution.md)  
- [tool-protocol.md](./tool-protocol.md)  
- [feedback-evaluator.md](./feedback-evaluator.md)  
- [../architecture.md](../architecture.md) §5  
