# Feedback & Evaluator design

**Status:** Shipped (library only — not CLI)  
**Owner:** Mohit Ranka  
**Last updated:** 22 Jul 2026  
**Phase:** **3 only** (exclusive)  
**PRD:** EVAL-01  
**Architecture:** §4.7, §5.9, Phase 3  
**Related:** [agent-loop.md](./agent-loop.md) (Phase 1 hooks)

---

## 1. Problem / context

Single agents self-grade with confirmation bias. A **Generator / Evaluator** split (plus deterministic sensors) improves first-pass quality. Pattern is **opt-in** (decaying scaffolding).

## 2. Goals & non-goals

**Goals**

- Deterministic sensors (tests, linters) + optional independent Evaluator agent.  
- Evaluator uses a **clean context** and limited tools.  
- Failures become structured **repair tasks** for the Generator.  
- Default: Generator-only.

**Non-goals**

- Mandatory dual-agent cost on every turn.  
- Human-quality formal verification.  
- Replacing CI systems entirely.

## 3. Design

### 3.1 Gate placement

After Generator tool batches / implementation milestones (config: every N turns, on `test` tool request, or explicit “done” signal):

```text
run deterministic sensors
if fail and evaluator_enabled:
  spawn Evaluator session (clean context)
  collect structured report
enqueue repair tasks → Generator user/system message
continue Generator loop
```

### 3.2 Deterministic sensors

| Sensor | Example |
|--------|---------|
| `command` | `cargo test`, `npm test`, `ruff check` |
| `exit_code` | non-zero → fail |
| `artifact` | junit/log offload URI |

Sensors run through the same sandbox/journal path as tools (or a privileged meta-tool with audit).

### 3.3 Evaluator session

| Aspect | Design |
|--------|--------|
| Context | Clean: criteria + artifact pointers + offload URIs; **no** full Generator history |
| Tools | Read-only + test runners; no broad write unless configured |
| Output | Structured report: findings[], severity, suggested_repairs[] |
| Lifecycle | Short-lived session id; journaled independently |

### 3.4 Repair task shape

```json
{
  "source": "evaluator",
  "sensor": "cargo test -p …",
  "summary": "public_api_returns_429 failed",
  "details_uri": "file://.forge/offload/…",
  "suggested_steps": ["attach middleware to public nest", "re-run tests"]
}
```

Injected into Generator as a user or system message clearly labeled so the model prioritizes fixes.

### 3.5 Success metric (PRD)

&gt; 40% relative improvement in first-pass quality vs single-pass baseline on the harness benchmark suite—measured offline, not a runtime gate.

## 4. Interfaces

```rust
trait Sensor: Send + Sync {
    async fn run(&self, ctx: &SensorContext) -> SensorReport;
}

struct FeedbackGate { sensors: Vec<Box<dyn Sensor>>, evaluator: Option<EvaluatorConfig> }
```

## 5. Failure modes & edge cases

| Case | Behavior |
|------|----------|
| Sensor infra failure | Treat as gate failure; do not pass silently |
| Evaluator loops forever | Max eval rounds per gate (e.g. 3) then surface to human |
| Generator ignores repairs | Re-enqueue until max rounds; then fail session or HITL |

## 6. Phase ownership

| Item | Phase |
|------|-------|
| This entire document | **3** |
| Exit | EVAL-01 quality target on benchmark suite |

## 7. Open questions

1. Default sensors per language ecosystem detection.  
2. Whether Evaluator may edit files (recommendation: **no** by default).  
3. Benchmark suite contents for the 40% claim.

## Related docs

- [agent-loop.md](./agent-loop.md)  
- [../ui.md](../ui.md) evaluator screen  
