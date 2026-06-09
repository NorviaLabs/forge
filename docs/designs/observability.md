# Observability design

**Status:** Draft  
**Owner:** Mohit Ranka  
**Last updated:** 22 Jul 2026  
**PRD:** OBS-01  
**Architecture:** §11  
**Related:** [durable-execution.md](./durable-execution.md), [governance.md](./governance.md), [agent-loop.md](./agent-loop.md)

---

## 1. Problem / context

Long agent runs need step-level visibility: model latency, tool failures, context resets, HITL waits—exportable to standard backends without leaking secrets.

## 2. Goals & non-goals

**Goals**

- OpenTelemetry-compatible traces across model, tool, and step boundaries.  
- Metrics for latency, tokens, errors, journal write latency.  
- Structured logs with `session_id` + `trace_id`.  
- Redaction by default for secrets and sensitive args.

**Non-goals**

- Building a proprietary APM UI in Phase 3 core.  
- 100% perfect token accounting across all providers without their usage fields.

## 3. Design

### 3.1 Stack

- Rust: `tracing` + OTEL exporter crates.  
- Config: `otel.enabled`, endpoint, headers via env.

### 3.2 Span taxonomy (initial)

| Span | Parent | Attributes (non-secret) |
|------|--------|-------------------------|
| `session` | root | session_id, surface, model |
| `turn` | session | turn_index |
| `model.complete` | turn | provider, model, input_tokens, output_tokens |
| `tool.execute` | turn | tool_name, side_effect_class, decision, duration_ms |
| `context.reset` | session | usage_before, usage_after |
| `context.offload` | tool | bytes, tokens_saved |
| `hitl.wait` | turn | tool_name |
| `journal.append` | * | event_type, seq (optional sample) |

### 3.3 Metrics

| Metric | Type |
|--------|------|
| `forge.model.latency_ms` | histogram |
| `forge.tool.latency_ms` | histogram |
| `forge.tool.errors` | counter |
| `forge.journal.write_latency_ms` | histogram |
| `forge.context.usage_ratio` | gauge |
| `forge.session.count` | counter |

### 3.4 Redaction rules

Never export by default:

- API keys, `Authorization` headers  
- Vault material  
- Env values matching secret patterns  
- Full tool args when classified sensitive (hash or keys-only)

Audit log may store redacted args separately from OTEL attributes.

### 3.5 Correlation

- Generate `trace_id` per session or per turn (prefer session root + turn children).  
- Journal events store `trace_id` for time-travel with traces.  
- Surfaces may show short trace footer (`trace_link` event).

### 3.6 NFR hooks

Track harness overhead toward &lt; 15 ms and journal write &lt; 5 ms (design targets from PRD).

## 4. Interfaces

- `obs::init(&OtelConfig)`  
- Spans via `tracing::instrument` on core paths  
- `Redactor::redact_value(key, value) -> Value`

## 5. Failure modes & edge cases

| Case | Behavior |
|------|----------|
| Exporter down | Drop or buffer with limit; never block tool path &gt; budget |
| Partial flush on crash | Best-effort; journal remains source of truth |
| User disables OTEL | Zero-cost no-op layer |

## 6. Phase / rollout

| Phase | Scope |
|-------|-------|
| 1 | `tracing` logs + optional local fmt |
| 2 | richer internal spans |
| 3 | OTLP export, SIEM-oriented audit export |

## 7. Open questions

1. Session-level vs turn-level root spans default.  
2. PII scrubbing beyond secrets (emails in tool output).  
3. Standard dashboard templates (Grafana) as docs-only samples.

## Related docs

- [governance.md](./governance.md) audit  
- [durable-execution.md](./durable-execution.md)  
