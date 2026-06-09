# Forge — Design documents

**Status:** Draft  
**Last updated:** 22 Jul 2026  

Design docs sit between the product contract and the system overview:

| Layer | Document | Owns |
|-------|----------|------|
| Product | [../prd.md](../prd.md) | Outcomes, req IDs, acceptance metrics |
| Architecture | [../architecture.md](../architecture.md) | Context, modules, flows, stack decisions |
| **Design** | **this folder** | Contracts, algorithms, schemas, edge cases, open questions |
| UI mockups | [../ui.md](../ui.md) | TUI visuals; not the canonical command catalog |

## Index

| Document | PRD reqs | Phase | Summary |
|----------|----------|-------|---------|
| [tool-protocol.md](./tool-protocol.md) | CORE-01 | 1 | Schema-validated tools, registry, validation retry |
| [agent-loop.md](./agent-loop.md) | CORE-01, loop | 1 | Plan–act–observe control, termination, gates |
| [model-providers.md](./model-providers.md) | multi-provider | 1 | Unified client, stream events, config switch |
| [durable-execution.md](./durable-execution.md) | DUR-01, DUR-02, DUR-03 | 1–2 | Journal, replay, durable HITL |
| [protocols-mcp-acp.md](./protocols-mcp-acp.md) | CORE-02 | 1 (MCP) · 2 (ACP) | MCP Phase 1; ACP Phase 2 (fixed) |
| [configuration.md](./configuration.md) | portability NFRs | 1 | TOML + env, workspace root, defaults |
| [tui-commands.md](./tui-commands.md) | multi-surface | 1 | **Canonical** slash-command catalog |
| [surfaces.md](./surfaces.md) | multi-surface | 1–3 | TUI / headless / ACP adapter rules |
| [context-lifecycle.md](./context-lifecycle.md) | CTX-01, CTX-02 | 2 | Budget, offload, handoff reset |
| [workspace-isolation.md](./workspace-isolation.md) | CTX-03 | 2 | Git worktree isolation |
| [governance.md](./governance.md) | SEC-01, SEC-02, SEC-03 | 2 | Vault, ACL, sandbox, audit |
| [feedback-evaluator.md](./feedback-evaluator.md) | EVAL-01 | 3 | Generator / Evaluator dual-sensor |
| [observability.md](./observability.md) | OBS-01 | 3 | OTEL traces, metrics, redaction |

**Deferred (not drafted yet):** multi-channel gateway, SCIM, SIEM plugins — see PRD Phase 3.

### Priority vs phase

PRD **P0/P1** = severity. **Phase** = hard delivery assignment (see [prd.md](../prd.md) §13). No capacity-based deferral: CORE-02 MCP is Phase 1; CORE-02 ACP is Phase 2; CTX/SEC/DUR-03 are Phase 2; EVAL/OBS/channels are Phase 3.

## Suggested reading order

1. tool-protocol → agent-loop → model-providers  
2. durable-execution  
3. protocols-mcp-acp  
4. configuration · tui-commands · surfaces  
5. context-lifecycle · workspace-isolation · governance  
6. feedback-evaluator · observability  

## Template (for new design docs)

```markdown
# <Title>

**Status:** Draft | **Owner:** … | **Last updated:** …
**PRD:** <req IDs> · **Architecture:** §…
**Related:** …

## 1. Problem / context
## 2. Goals & non-goals
## 3. Design
## 4. Interfaces
## 5. Failure modes & edge cases
## 6. Phase / rollout
## 7. Open questions
## Related docs
```

## Rules of thumb

- Prefer **open questions** over invented precision.  
- Do not put full slash-command tables in the PRD — use [tui-commands.md](./tui-commands.md).  
- Architecture diagrams stay in [architecture.md](../architecture.md); designs may add small Mermaid for local algorithms only.  

## Cross-validation checklist (maintainers)

When editing docs, verify:

| Check | Rule |
|-------|------|
| Req IDs | Every CORE/DUR/CTX/SEC/EVAL/OBS id appears in PRD + ≥1 design doc |
| Phases | PRD §13 ↔ architecture §14 ↔ designs README phase column |
| ACP | MCP Phase 1; ACP Phase 2 only (decision #13) |
| progress.json | Schema + default path `.forge/progress.json` match architecture §6.3 and context-lifecycle |
| Journal events | Architecture table ⊆ durable-execution kinds |
| Slash commands | Canonical only in [tui-commands.md](./tui-commands.md); ui/architecture point here |
| Stack | Rust + serde/schemars + Tokio + sqlx/SQLite Phase 1; no Python/Pydantic as product stack |
| Surfaces | Surfaces never call model/MCP directly (surfaces + architecture rules) |

**Last full cross-check:** 22 Jul 2026.
