# Forge — Product Requirements Document

**Version:** 0.12.0  
**Status:** Draft  
**Owner:** Mohit Ranka  
**Last updated:** 23 Jul 2026  
**Related architecture:** [architecture.md](./architecture.md)  
**Related TUI UI:** [ui.md](./ui.md)  
**Related designs:** [designs/README.md](./designs/README.md)  

---

## 1. Executive summary

Forge is an open-source, enterprise-ready **AI agent harness**: scaffolding around foundation models that makes long-horizon work reliable. It combines a low-abstraction, schema-validated tool experience with durable execution, automated context lifecycle management, open client/tool protocols (MCP + ACP), and zero-trust governance.

Product behavior and acceptance criteria live in this document. Stack, crates, and design decisions live in [architecture.md](./architecture.md).

### Core value propositions

| Proposition | Description |
|-------------|-------------|
| **Zero abstraction tax & multi-surface API** | Flat, schema-validated tool contracts instead of heavy graph/role DSLs. One harness core exposed through terminal TUI, headless CI, IDE clients, and multi-channel gateway transports. |
| **Native durable execution** | Step-level event journals so crashed sessions, long terminal tasks, or background jobs resume without repeating completed tool calls or model steps. |
| **Automated context lifecycle & workspace isolation** | Token budgeting, large-payload offloading, structured handoff artifacts (`progress.json` / `AGENTS.md`), and optional git worktree isolation for experimental file edits. |
| **Universal protocol gateway (MCP + ACP)** | MCP for tool discovery/invocation; ACP for IDE/TUI/CLI clients—plus dynamic credentials, tool-level ACLs, and progressive sandbox/audit depth. |

---

## 2. Problem / opportunity

### 2.1 Structural taxonomy

A raw language model is a reasoning engine, not a reliable autonomous actor. Long-horizon enterprise execution requires surrounding application and infrastructure scaffolding. Industry consensus delineates the agentic stack into four layers:

| Layer | Role | Examples |
|-------|------|----------|
| **Model** | Cognitive core / reasoning engine | Claude, GPT, Grok |
| **Harness** | Application scaffolding: plan, memory, tools, session state, output parsing | LangGraph, Claude Code, Grok Build, OpenCode |
| **Runtime** | Infrastructure execution environment and physical constraints | Containers, sandboxes, git worktrees, serverless environments |
| **Agent** | Full operational system = Model + Harness + Runtime | End-to-end autonomous task executor |

### 2.2 Six functional primitives of an enterprise harness

| Primitive | Responsibility |
|-----------|----------------|
| **Loop control & orchestration** | Plan–act–observe cycles; step boundaries; termination conditions |
| **Context engineering & window management** | Dynamic system prompts, working memory, payload offload, history compaction |
| **State & persistence tracking** | Durable cross-session records outside the model context (handoff files, event logs) |
| **Tool lifecycle & protocol routing** | Tool exposure, argument validation, MCP/ACP dispatch, response normalization |
| **Safety, governance & HITL gating** | Permissions, redaction, approval workflows, sandboxed execution |
| **Observability, verification & feedback sensors** | Telemetry; deterministic checks (linters, tests); independent evaluation; self-correction |

Harness design can materially affect coding-benchmark performance. Design must also manage **model/harness co-evolution**: models trained against specific harness abstractions can overfit and degrade under alternative scaffolding. Guardrails must coexist with architectural portability.

### 2.3 Industry gaps and bottlenecks

1. **Repo harnesses vs. multi-channel gateways** — Terminal coding harnesses excel at synchronous repo work but often lack native multi-channel routing and background scheduling. Gateways handle channels and cron but create security risk when used for unconstrained repo execution.
2. **Protocol fragmentation** — MCP is the de facto tool standard; client–agent transport remains fragmented (proprietary APIs, ACP, custom plugin SDKs).
3. **Context rot & state fragmentation** — Accumulated tool outputs and history degrade attention. Most stacks lack automated token budgeting and structured handoff artifacts for clean resets without losing task state.
4. **Governance & credential security** — API keys often live in config/env; tool security defaults to permissive binary prompts. Enterprise needs dynamic ACLs, vault-injected credentials, and auditable sandboxing.
5. **Framework vs. durable runtime divide** — Agent frameworks often lose state on process crash; durable workflow engines recover process state but lack native LLM context, token budgeting, and compaction awareness.

---

## 3. Competitive landscape

### 3.1 Framework paradigms

| Paradigm | Exemplars | Strengths | Weaknesses |
|----------|-----------|-----------|------------|
| **DAG / graph orchestrators** | LangGraph | Explicit state machines, checkpointers, HITL interrupts | High boilerplate; single-process resiliency often needs external durable engines |
| **Role-based multi-agent** | CrewAI | Intuitive org metaphors | Underspecified NL roles; weak low-level control |
| **Code-first, schema-typed** | PydanticAI | Low abstraction tax, high AI-codability | Limited built-in multi-agent graphs or durable cross-session execution |
| **Terminal coding harnesses** | Claude Code, Grok Build, OpenCode | Repo-native tools, memory files, worktrees/subagents | Sync task focus; limited multi-channel/background |
| **Always-on gateways** | OpenClaw | Multi-channel, cron, Markdown identity/memory | Security risk if used for unconstrained repo execution |
| **Durable execution engines** | Temporal, Restate, Kitaru | Crash recovery via event log replay | Opaque to LLM context, compaction, token budgets |

### 3.2 Terminal harness notes

- **Claude Code (Anthropic):** Closed-source reference; project memory files, loops, subagents; isolation and tool-deny configuration patterns.
- **Grok Build (xAI):** Open-source terminal harness; TUI decoupled from runtime (terminal, IDE, headless CI); MCP; local-first inference options.
- **OpenCode (SST):** Open-source, model-agnostic; planning loops, subagents, `AGENTS.md`, plugin SDK, per-agent permission rulesets.

### 3.3 Comparative product matrix

| Dimension | LangGraph / CrewAI | PydanticAI | Claude Code | Grok Build | OpenCode | OpenClaw | Durable engines |
|-----------|--------------------|------------|-------------|------------|----------|----------|-----------------|
| **Primary paradigm** | DAG / role multi-agent | Code-first typed schema | Closed terminal harness | Open terminal harness | Open coding harness | Multi-channel gateway | Event-sourced workflows |
| **State & workspace** | Checkpoints / in-memory | Developer-managed | Memory files, worktrees, session logs | Host FS, VCS, session logs | `AGENTS.md`, session compaction | Markdown + session stores | Immutable event log replay |
| **Tool / client protocols** | Framework wrappers / MCP | Framework tools / MCP | MCP + built-ins | ACP + MCP | Plugin SDK + MCP | Channel adapters / plugins | Opaque activities |
| **Abstraction tax & DX** | High | Minimal | Low | Low | Low | Moderate | High |
| **AI-codability** | Moderate–low | High | High | High | High | Moderate | Low |
| **Context lifecycle** | Manual | Manual | Isolation, compaction, offload | Budgeting / assembly | Summary compaction | Session compaction | None (opaque code) |
| **Security & sandboxing** | Custom middleware | App-level checks | Tool denylist, worktrees | Workspace sandbox patterns | Per-agent path rules | Channel ACLs, containers | Infra policies |
| **Fault recovery** | Checkpoint (manual re-entry) | In-process retry | Project files + git resume | Session resume | Session recovery | Transcripts & gateway retries | Transparent crash replay |

---

## 4. Design principles

### Pattern 1: Context lifecycle — compaction vs. reset with handoff artifacts

| Strategy | Approach | Tradeoff |
|----------|----------|----------|
| **Compaction** | Summarize older history; offload large tool responses to files | Preserves continuity; residual noise can still cause attention drift |
| **Hard reset + handoff** | Persist machine-readable state (`progress.json`, `AGENTS.md`) + Git history; wipe context; re-inject only progress + workspace | Clean window; multi-day continuity without accumulated prompt noise |

Forge prioritizes **token budgeting + payload offload + structured reset** for long-horizon tasks.

### Pattern 2: Decoupled multi-agent separation (Generator vs. Evaluator)

Single agents self-evaluate with confirmation bias. Dual structure:

- **Generator** — Executes the core task in structured, isolated steps.
- **Evaluator** — Independent quality gate with a clean context and specialized checks. Failure logs route back as structured repair tasks until criteria pass.

### Pattern 3: Decaying scaffolding

Every harness component encodes an assumption about what the model cannot yet do. As models improve, rigid intermediate stages become counterproductive. Harness complexity must be **prunable**—reduce latency and token overhead as base capabilities advance.

---

## 5. Users & jobs-to-be-done

| User / persona | Job to be done | Context |
|----------------|----------------|---------|
| Platform engineer | Deploy a reliable agent runtime with crash recovery and auditability | Production / enterprise |
| Application developer | Define tools and agents with clear contracts, without graph boilerplate | Development |
| SRE / security | Enforce least-privilege tools, vault credentials, sandbox policy | Governance |
| Coding agent operator | Run long multi-step coding tasks in repo with worktree isolation | Development / CI |
| CI / automation | Headless resume of interrupted agent jobs | Pipelines |
| Team lead / reviewer | Approve high-risk tool calls; review evaluator failure reports | HITL |
| Multi-channel operator | Route Slack/Telegram/webhook tasks to the same durable agent core | Background automation |
| Debugger | Trace model/tool steps; time-travel via event journal | Operations |

---

## 6. Goals

1. Deliver a **schema-validated, low-abstraction** tool and agent core with high AI-codability (easy for humans and coding models to extend).
2. Provide **native protocol** support: **MCP** for tools and **ACP** for IDE clients (delivered in successive product phases).
3. Embed **durable execution** with process-crash recovery and no duplicate side effects.
4. Automate **context lifecycle**: payload offloading, token budgets, `progress.json` / `AGENTS.md` handoff resets.
5. Support **git worktree isolation** for unapproved or experimental file mutations.
6. Enforce **zero-trust governance**: vault credential injection, dynamic tool ACLs, progressive sandbox depth.
7. Ship **dual-sensor feedback** (deterministic checks + independent Evaluator agent).
8. Expose **multi-surface interfaces**: full-screen terminal TUI, headless CI, IDE via ACP, multi-channel gateway.
9. Emit **standard distributed traces** across model, tool, and step boundaries (OpenTelemetry-compatible).
10. Support **multi-provider models** via configuration only (no application rewrites).  
11. (Phase 5) Reach the **broad provider catalog** via the **LiteLLM Python SDK (library)**, not the LiteLLM Proxy gateway.  
12. (Phase 6) Ship a first-class **`/connect`** flow for productized providers, starting with **xAI Grok** and **OpenCode Go**, without reintroducing dual model-client stacks.  
13. (Phase 7) Support **command history navigation with arrow keys** in the full-screen TUI (Up/Down).  
14. (Phase 8) Allow **top-level slash commands** to be typed and run from the **main TUI textbox** (not only via the command palette).  
15. (Phase 8.1) **Tab autocomplete** for slash commands and a **visible highlight cursor** for the selected suggestion and input/history text.  
16. (Phase 9) Ship a first-class **`web_search` built-in tool** so agents can query the public web under the same schema-validated, journaled, ACL-filtered tool path as workspace built-ins.  
17. (Phase 10) Make the full-screen TUI **operator-visible**: session identity (provider, model, context, profile) always on chrome; failures and activity (e.g. rate limits) always appear on screen—not only in unrendered internal fields.

---

## 7. Non-goals (initial product boundary)

| Non-goal | Rationale |
|----------|-----------|
| Replacing foundation models themselves | Harness is scaffolding around models |
| Heavy DAG/role DSLs as the primary API | Prefer flat, schema-validated tool contracts (decaying scaffolding) |
| Treating durable workflow engines as a full substitute for the harness | They lack LLM-native context management |
| Always-on gateway as the sole code-execution path | Multi-channel ingress must not over-grant repo/tool rights |
| Proprietary single-client lock-in | Prefer open MCP + ACP |
| Opaque “agent as black box” without audit logs | Enterprise requires immutable invocation records |
| **LiteLLM Proxy as required infrastructure** | Phase 5 uses the **LiteLLM library/SDK** inside a Forge-owned worker; an org-wide LLM gateway is optional and out of scope |

Phase-scoped deferrals: multi-channel fleet, SCIM, deep kernel sandboxing, SIEM plugins may ship after core durability and protocols. Universal provider coverage via LiteLLM is **Phase 5**. Productized connect UX for xAI Grok and OpenCode Go is **Phase 6**. Built-in web search is **Phase 9**.

---

## 8. Capability modules

Product capabilities group into five areas (implementation detail in architecture):

| Module | Responsibility |
|--------|----------------|
| **Core & protocols** | Schema-validated tools; MCP (tool servers) and ACP (IDE/TUI/CLI clients) |
| **Durable execution** | Append-only step journal; record-before-side-effect; resume without re-execution |
| **Context & workspace** | Token budgets; payload offload; handoff artifacts; worktree isolation |
| **Governance & sandbox** | Identity-aware tool ACLs, secret injection, audit trail, progressive isolation |
| **Feedback & surfaces** | Deterministic checks + Evaluator; telemetry; TUI / headless / multi-channel status |

---

## 9. Functional requirements

### 9.1 Requirements table

| Module ID | Requirement name | Specification | Acceptance metric / target | Priority |
|-----------|------------------|---------------|----------------------------|----------|
| **CORE-01** | Schema-validated tool protocol | Every tool has a declared input/output contract. Invalid arguments are rejected **before** side effects; the model is prompted to correct them. Tool listings exposed to models match those contracts. | No unhandled invalid-arg paths to side effects; 100% listed tools have enforceable schemas | P0 (Critical) |
| **CORE-02** | MCP tool protocol | Harness natively discovers and invokes tools via MCP; MCP tools share the same validation and dispatch path as built-ins. | Interoperates with standard MCP servers without custom per-server bridges | P0 (Critical) |
| **CORE-03** | ACP client protocol | Harness serves ACP for IDE (and similar) clients; same agent loop and journal as TUI/headless—no second agent implementation. | ACP-compliant IDE client runs a full session without a custom bridge | P0 (Critical) |
| **DUR-01** | Embedded event journaling | Every model invocation, tool execution, and state transition is recorded in an append-only event log **before** side effects run. | Zero missing state records on abrupt shutdown; journal write latency target &lt; 5 ms per step | P0 (Critical) |
| **DUR-02** | Process crash recovery | On restart, reconstruct session state from the journal; reuse completed tool/model results without re-executing them. | 100% state recovery success; zero duplicate external side effects on replay | P0 (Critical) |
| **DUR-03** | Durable human-in-the-loop | High-risk operations pause without holding active compute; resume when an approval is received, including across process restarts. | No active compute while waiting; seamless resume after restart | P1 (High) |
| **CTX-01** | Dynamic payload offloading | Tool responses above a configurable size/token threshold are stored outside the active prompt and replaced with references. | ≥ 80% reduction in context bloat from large tool responses | P0 (Critical) |
| **CTX-02** | Structured reset & handoff | When context usage crosses a configurable threshold (default 80% of capacity), write handoff state (`progress.json` / `AGENTS.md`), clear active context, and continue from handoff + workspace. | No context overflow failures; task alignment across 100+ turns | P0 (Critical) |
| **CTX-03** | Git worktree workspace isolation | File-editing tools can run in an isolated temporary git worktree; results merge or discard on completion. | Working tree remains isolated during unapproved experimental executions | P1 (High) |
| **SEC-01** | Zero-trust credential broker | Credentials live in a vault (or equivalent); injected at call time by the gateway. Secrets never enter model prompts or ordinary traces. | Zero credentials in conversation histories or default trace attributes | P0 (Critical) |
| **SEC-02** | Dynamic tool ACLs | Tools visible/callable depend on identity, role, and scopes; unauthorized tools are omitted from model tool listings. | Deny lists always enforced; restricted tools never offered to the model | P0 (Critical) |
| **SEC-03** | Behavioral sandboxing | Local tool execution runs under isolation policy that can block unauthorized network egress, syscalls, or file access (depth increases by phase). | Policy breaches blocked; enforcement latency target &lt; 1 ms for kernel-backed profiles when enabled | P1 (High) |
| **EVAL-01** | Dual-sensor feedback loop | Run deterministic checks (linters, tests) alongside an independent Evaluator agent; route failures back to the Generator for repair. | &gt; 40% relative improvement in first-pass quality vs single-pass baseline | P1 (High) |
| **OBS-01** | Distributed tracing | Emit standard traces for model calls, tool latencies, token usage, and step transitions (OpenTelemetry-compatible). | Full step coverage; exportable to common observability backends | P1 (High) |
| **TUI-01** | Full-screen TUI shell | Interactive full-terminal UI (not line-mode REPL only) with status bar, main pane, sidebar, input, and footer matching [ui.md](./ui.md) layout regions. | Operator can complete a session entirely in the TUI without using `repl` line mode | P0 (Critical) |
| **TUI-02** | Conversation & tool presentation | Chat shows user/assistant/system messages and tool cards (running/done/blocked); supports streaming/status while the agent runs; redacts secrets. | Tool cards and message roles distinguishable; no raw secrets in UI | P0 (Critical) |
| **TUI-03** | Live session sidebar | Sidebar shows session id/status, context budget meter, tool ACL summary, and recent journal/events. | Values update after turns without leaving the TUI | P1 (High) |
| **TUI-04** | Overlays & palettes | Modal overlays for HITL approve/deny, slash-command palette (`/`), and model picker; keyboard-first (no mouse required). | HITL and `/` flows completable via keys alone per [ui.md](./ui.md) screens 04, 07, 08 | P0 (Critical) |
| **TUI-05** | Input command history | In full-screen TUI, **Up/Down** arrows recall previously submitted input lines (prompts and slash commands). | Operator can re-run/edit prior lines without retyping; history inactive under overlays | P1 |
| **TUI-06** | Inline slash in main textbox | Type top-level `/commands` in the main input and run with Enter; do not hijack `/` into palette-only flow. | `/status`, `/connect …`, and catalog commands work when typed fully in the textbox | P1 |
| **TUI-07** | Tab autocomplete + highlight | Tab completes the highlighted slash suggestion; caret/highlight shows selected suggestion and input/history position. | `/sta` + Tab → `/status`; ↑↓ move highlight; caret visible | P1 |
| **WEB-01** | Built-in web search tool | First-class `web_search` tool: schema-validated query args; pluggable search backends (e.g. Tavily/Brave/Serper + mock); network side-effect class; keys via env/vault only; same journal + ACL path as other built-ins. | Model can search the public web when enabled; mock path works offline; no API keys in transcripts/traces | P1 |
| **TUI-08** | Always-visible status feedback | Render latest feedback/status line every frame; dual-write model/tool/session errors into conversation error banners; severity styling; never leave operator-critical outcomes only in unrendered fields. | Failed model turn (e.g. rate limit) visible in frame buffer without reading internal state | P0 |
| **TUI-09** | Session identity chrome | Status chrome shows provider · model · context · worktree (and profile/search when space); narrow terminals keep model/ctx without sidebar; `/status` mirrors chrome fields. | Wide and narrow TestBackend frames include model and context cues | P0 |
| **TUI-10** | Activity feed & progressive busy | In-session activity ring buffer in sidebar (wide); progressive busy labels (`running · model` / `running · tool:name`); errors also create feed entries. | After a multi-step turn, activity list shows model/tool/error summaries | P1 |

### 9.2 Priority summary

- **P0 (Critical):** CORE-01, CORE-02, CORE-03, DUR-01, DUR-02, CTX-01, CTX-02, SEC-01, SEC-02, TUI-01, TUI-02, TUI-04, TUI-08, TUI-09  
- **P1 (High):** DUR-03, CTX-03, SEC-03, EVAL-01, OBS-01, TUI-03, WEB-01, TUI-10  

---

## 10. Non-functional and enterprise governance requirements

### 10.1 Security & isolation

- Network paths between harness, protocol proxies, and execution sandboxes **must** use modern TLS (TLS 1.3).
- Code execution **must** support isolated environments with least privilege (non-root, restricted egress, minimal writable filesystem as policy allows).
- Immutable **audit log** of tool invocations, argument payloads (with redaction), model responses, and policy decisions.
- Audit/telemetry export suitable for SIEM ingestion (e.g. OTLP).

### 10.2 Performance & capacity

| Metric | Target |
|--------|--------|
| Harness overhead on model/tool loops | &lt; 15 ms |
| Event journal throughput | Up to 10,000 concurrent sub-agent log operations/sec (design target) |
| Active controller memory per session | &lt; 256 MB (inactive state offloaded to durable storage) |
| Event log write latency (DUR-01) | &lt; 5 ms per step |

### 10.3 Model provider portability

- Unified interface for major providers (cloud APIs and local inference servers).
- Switching providers requires **configuration only**—no changes to tool definitions, agent logic, or durable state schemas.
- **Phase 1 (historical):** Thin native adapters (OpenAI-compatible, Anthropic, xAI) behind `ModelClient`.
- **Phase 5 (MDL-01):** **Single production path** — **LiteLLM Python SDK** (library, not Proxy) for **all** providers, including those formerly served by native adapters. Dual native+LiteLLM stacks are removed so operators and code maintain one client. **Mock** remains for offline CI only.
- **Phase 6 (CONN-01, PROV-01, PROV-02):** **`/connect`** onboarding for **xAI Grok** and **OpenCode Go**; still uses the Phase 5 LiteLLM path.  
- **Phase 6.1:** **xAI Grok connects via OAuth** (not API-key paste). **OpenCode Go TUI must explicitly prompt for API key** when connecting.
- **Phase 7 (TUI-05):** Full-screen TUI **command history** via **Up/Down** arrow keys on the input bar.
- **Phase 8 (TUI-06):** Top-level **slash commands** run from the **main textbox** (`/cmd …` + Enter); palette remains optional discovery.
- **Phase 8.1 (TUI-07):** **Tab** autocomplete for slash suggestions; **highlighted** selected command and **visible caret** (including history recall).
- **Phase 9 (WEB-01):** Built-in **`web_search`** tool with pluggable backends and secure key handling.
- **Phase 10 (TUI-08…10):** Operator-visible TUI — feedback strip, session chrome, activity feed / progressive busy.

---

## 11. Success criteria

| # | Criterion | How measured |
|---|-----------|--------------|
| 1 | Invalid tool args are rejected and recover via validation prompts | Schema/compliance tests |
| 2a | MCP servers interoperate without custom bridges | Integration tests with ≥1 real MCP server |
| 2b | ACP IDE client runs full sessions on the same core | Integration test with one ACP-compliant client |
| 3 | Process kill mid-task resumes with no duplicate side effects | Chaos/crash recovery tests against event journal |
| 4 | Large tool payloads do not blow context; handoff resets preserve alignment at 100+ turns | Token accounting + long-horizon harness |
| 5 | No secrets in transcripts or default trace attributes | Redaction/security tests |
| 6 | Unauthorized tools never appear in model tool lists | ACL unit + integration tests |
| 7 | Generator/Evaluator improves first-pass quality vs single-pass baseline | Benchmark suite (target &gt; 40% relative) |
| 8 | Full step coverage in distributed traces | Trace completeness checks |
| 9 | Provider switch is config-only | Multi-provider smoke matrix |
| 10 | Web search tool is schema-validated and journaled | Unit + mock integration; live smoke optional with API key |
| 11 | Operator can see session identity and last failure in the TUI | TestBackend frames; no reliance on unrendered `status_message` alone |

---

## 12. Assumptions

1. Foundation models remain external services (or local inference servers); Forge does not train base models.
2. Host environments can provide isolation (containers and/or stronger sandbox profiles) for later security phases.
3. Single-node deployments use a local durable store; multi-instance enterprise deployments may use a shared database backend.
4. `AGENTS.md` / `progress.json` are first-class handoff artifacts stored with or alongside the workspace as policy allows.
5. Identity providers and secret vaults (or compatible mocks) are available for SEC-01/SEC-02 in Phase 2+.
6. Open MCP and ACP specifications remain stable enough for dual-protocol support without permanent forks.

---

## 13. Strategic takeaways

The agent ecosystem is moving from rapid-prototype abstractions (loose roles, heavy graphs, unmonitored single-process runtimes) toward **production-grade harness engineering**. Long-horizon reliability depends less on prompt tweaks and more on:

- Flat, schema-validated APIs  
- Durable execution with LLM-aware recovery  
- Active context lifecycle and workspace isolation  
- Open dual-protocol support (MCP + ACP)  
- Governance, auditability, and observability  
- Operator-grade terminal UX (full-screen TUI) without sacrificing headless CI  
- Broad model portability through a **single** LiteLLM SDK path (Phase 5)  
- Guided **`/connect`** onboarding for key product providers (Phase 6: xAI Grok, OpenCode Go)  
- Terminal ergonomics: **command history** with arrow keys in the full-screen TUI (Phase 7)  
- Inline **slash commands** in the main TUI textbox (Phase 8)  
- **Tab autocomplete** and **highlight cursor** for slash suggestions and history (Phase 8.1)  
- **Web search as a first-class tool** for public knowledge beyond the repo (Phase 9)  
- **Operator-visible TUI**: ambient session chrome, always-on feedback, activity timeline (Phase 10)  

Forge is specified to occupy that intersection: low abstraction tax, enterprise durability, portable model/client integration, and a first-class terminal surface.

---

## Related docs

- Architecture (implementation decisions, Rust stack, crate layout): [architecture.md](./architecture.md)  
- TUI UI reference (screens & workflows): [ui.md](./ui.md)  
- Design docs (contracts, algorithms, catalogs): [designs/README.md](./designs/README.md)  
