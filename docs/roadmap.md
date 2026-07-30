# Forge — Roadmap

**Status:** Planned and partial work. **Nothing on this page is implemented unless
marked "partial", and partial means what it says.**
**Last updated:** 30 Jul 2026
**Related:** [architecture.md](./architecture.md) · [prd.md](./prd.md)

This page exists because these subsystems were previously described in
`architecture.md` in the present tense, as though they shipped. They did not. A
contributor reading that document went looking for crates that do not exist, and an
evaluator could reasonably have concluded that OTEL export and a Postgres journal
were available.

The split is the point: **[architecture.md](./architecture.md) is what the code
does. This is what it does not do yet.**

---

## Not built

### Generator / Evaluator feedback loop

The design called for an optional second agent that runs in a clean context,
evaluates the Generator's output against criteria, and returns structured repair
tasks — combined with deterministic sensors such as lint and test runners.

**State:** no crate, no orchestration, one incidental mention of "evaluator" in the
whole codebase. The `forge-feedback` crate named in earlier drafts was never
created.

**Why it is still wanted:** self-grading is unreliable, and a clean-context
evaluator avoids it. Worth revisiting once the recovery path below is finished,
since an evaluator that spawns sessions depends on session lifecycle being solid.

### Observability and OTEL export

Planned: OTEL spans for session step, model call, tool call, context reset and
approval wait; metrics for latency, tokens, tool error rates and journal write
latency; OTLP export.

**State:** not built. There are zero `opentelemetry` dependencies in the workspace.
`tracing` is used for structured logging in five crates, which is useful locally but
is not a telemetry pipeline. The `forge-obs` crate named in earlier drafts does not
exist.

**Prerequisite worth deciding first:** whether Forge should emit telemetry off the
machine at all by default. For a local developer tool the answer is probably no, and
that decision shapes the whole design.

### Postgres journal backend

Planned: SQLite for single-node, Postgres for multi-instance, selected by DSN.

**State:** not built, and not merely unimplemented — `SqlitePool` is the concrete
type on `Journal`, there is no storage trait to swap, and the `sqlx` dependency
enables the `sqlite` feature only. A Postgres backend means introducing the
abstraction first.

**Note:** Forge is a single-user terminal tool. Multi-instance journalling has no
current use case, so this may be worth dropping rather than building.

### Sandbox profiles

Planned: container and eBPF execution profiles, non-root, restricted egress,
read-only root filesystem.

**State:** not built. Tool execution is confined to the workspace by path
resolution — canonicalised, with escapes and writes into `.git` refused — which
addresses path traversal but is not process isolation. A tool that runs a shell
still runs it with the user's full privileges.

**This is the widest gap between the security story and the implementation.** The
approval prompt, not the runtime, is what currently stands between a model-proposed
shell command and the machine.

### Vault / IdP credential injection

Planned: secrets held in a vault, injected per call, with OAuth2 scope evaluation
against a principal.

**State:** not built. Credentials come from the environment or the local 0600
store. Relatedly, the ACL accepts a `Principal` and a `SideEffectClass` and uses
neither — it matches on tool name — so there is currently no identity for a scope
check to evaluate against.

### Parallel tool execution

Tool calls within a model turn run sequentially, which keeps journal ordering
simple. Parallelism needs an ordering model for interleaved `tool_intent` /
`tool_result` pairs before it is safe.

---

## Partial

These have working code that stops short of the guarantee. They are the highest
-value items on this page, because each is close and each currently reads as
finished from the outside.

### Replay without double execution

`cached_tool_result` exists in `forge-durable` and is tested. `AgentSession::resume`
does not call it. Conversation history rebuilds correctly, but nothing prevents a
resumed session from re-executing a tool whose result is already journaled.

**Remaining work:** consult the cache in the resume path. The infrastructure is
already there; this is roughly one call site plus a test that proves a resumed tool
is served from the journal rather than re-run.

### Crash-recovery policy

Replay identifies `tool_intent` records with no matching result. The policy is to
mark them failed, or retry only when the tool declares itself idempotent.

**State:** incomplete intents are found and logged as a warning. Neither branch is
taken. `ToolDescriptor::idempotent` is declared on every tool and is never read
anywhere in the loop.

**Remaining work:** write a synthetic failed `tool_result` for non-idempotent
intents, and gate retry on the flag that already exists.

### Approval that survives a restart

`hitl_wait` and `hitl_resume` are journaled, and replay restores `AwaitingHitl`
along with the pending payload. But `resolve_hitl` takes a decision as an in-process
argument: there is no endpoint, no polling of the journal, no IPC. Nothing outside
the running process can approve a waiting call.

So the accurate description is **journaled in-process approval**, not approval that
outlives the controller. The replay machinery would support the durable version;
the out-of-process signalling path is what is missing.

**Also noted in the code:** the approve path re-executes with redacted arguments
rather than the originals, which is a known weakness of the current implementation.

### Journal schema versioning

Rows carry a `schema_version` column. It is written as `1` and read into the event
struct, but replay branches only on event type, so the field cannot yet gate payload
parsing — which is the one job a schema version exists to do.

**Remaining work:** branch on it in replay, so a future layout change has somewhere
to hook.

---

## Removed from the plan

| Was planned | Why it is gone |
|-------------|----------------|
| `forge repl`, `forge tui`, `--mock` product flags | The TUI is the default surface; mock is selected by config for tests, not by a user-facing flag |
| Dual native + worker model clients | One native Rust client. A second production client was explicitly rejected |
| Python model proxy | Native Rust HTTP/SSE transports replaced it |
| IDE, chat and CI surface adapters | Out of scope; the terminal is the product |

---

## A note on the deleted design documents

`architecture.md` previously delegated most normative detail to nine documents under
`docs/designs/` — covering the agent loop, durable execution, durable HITL, context
lifecycle, the TUI command catalogue, the web-search tool, workspace isolation and
the connect command. **That directory no longer exists**, so every one of those
references was dead, and the detail they were supposed to carry was simply
unavailable.

Rather than leave pointers to missing files, the surviving normative content has
been folded into [architecture.md](./architecture.md) directly, and the unbuilt
portions are described above. If per-subsystem design documents return, they should
be linked from the relevant section rather than replacing it — a top-level document
that only points elsewhere is what allowed the drift in the first place.
