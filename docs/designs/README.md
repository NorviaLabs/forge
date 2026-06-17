# Forge — Design documents

**Status:** Draft  
**Last updated:** 23 Jul 2026  

| Layer | Document | Owns |
|-------|----------|------|
| Product | [../prd.md](../prd.md) | Outcomes, req IDs, **exclusive phase map** |
| Architecture | [../architecture.md](../architecture.md) | System context, flows, stack |
| **Design** | **this folder** | One concern **and one phase** per file |
| UI mockups | [../ui.md](../ui.md) | Visuals only |

## Rules

1. **Exclusive phase ownership** — Each design doc’s header **Phase:** field is a single number (1–4). No multi-phase owners.  
2. **Exclusive req ownership** — Each PRD req ID appears in exactly one design doc as primary owner.  
3. **Product-complete phases** — See [prd.md](../prd.md) §13: Phase 1 coding agent, Phase 2 enterprise harness, Phase 3 fleet/quality, Phase 4 full-screen TUI.  
4. **Cross-phase references** are allowed as *dependencies* (“builds on Phase 1 journal”) but must not re-specify the other phase’s design.

## Index by phase

### Phase 1 — Coding agent

| Document | PRD reqs | Summary |
|----------|----------|---------|
| [tool-protocol.md](./tool-protocol.md) | CORE-01 | Schemas, registry, validation retry |
| [agent-loop.md](./agent-loop.md) | loop (CORE-01 path) | Plan–act–observe (no HITL/eval/handoff) |
| [model-providers.md](./model-providers.md) | multi-provider | Unified client, 3 adapters |
| [durable-execution.md](./durable-execution.md) | DUR-01, DUR-02 | Journal + crash recovery |
| [protocol-mcp.md](./protocol-mcp.md) | CORE-02 | MCP discovery/call |
| [configuration.md](./configuration.md) | config NFR | Phase 1 TOML/env keys only |
| [tui-commands.md](./tui-commands.md) | TUI control | Phase 1 slash catalog only |
| [surfaces.md](./surfaces.md) | TUI + headless | Phase 1 surfaces only |

### Phase 2 — Enterprise long-horizon harness

| Document | PRD reqs | Summary |
|----------|----------|---------|
| [protocol-acp.md](./protocol-acp.md) | CORE-03 | ACP IDE surface |
| [context-lifecycle.md](./context-lifecycle.md) | CTX-01, CTX-02 | Offload + handoff reset |
| [workspace-isolation.md](./workspace-isolation.md) | CTX-03 | Git worktree |
| [durable-hitl.md](./durable-hitl.md) | DUR-03 | HITL wait/resume |
| [governance.md](./governance.md) | SEC-01, SEC-02, SEC-03 | Vault, ACL, sandbox |

### Phase 3 — Quality, ops & fleet

| Document | PRD reqs | Summary |
|----------|----------|---------|
| [feedback-evaluator.md](./feedback-evaluator.md) | EVAL-01 | Generator / Evaluator |
| [observability.md](./observability.md) | OBS-01 | OTEL export |
| [channels.md](./channels.md) | CH-01 | Multi-channel ingress |
| [fleet-plugins.md](./fleet-plugins.md) | FLEET-01 | SCIM + SIEM plugins |

### Phase 4 — Full-screen terminal TUI

| Document | PRD reqs | Summary |
|----------|----------|---------|
| [tui-shell.md](./tui-shell.md) | TUI-01 | ratatui app loop, status/input/footer layout |
| [tui-conversation.md](./tui-conversation.md) | TUI-02 | Messages, tool cards, banners |
| [tui-sidebar.md](./tui-sidebar.md) | TUI-03 | Session / budget / ACL / journal panels |
| [tui-overlays.md](./tui-overlays.md) | TUI-04 | HITL modal, slash palette, model picker |

Visual source of truth: [../ui.md](../ui.md). Phase 1 `surfaces` / line-mode `repl` remain; Phase 4 is the operator-grade full-screen surface.

## Reading order

**Phase 1:** tool-protocol → agent-loop → model-providers → durable-execution → protocol-mcp → configuration → surfaces → tui-commands  

**Phase 2:** protocol-acp → context-lifecycle → workspace-isolation → durable-hitl → governance  

**Phase 3:** feedback-evaluator → observability → channels → fleet-plugins  

**Phase 4:** tui-shell → tui-conversation → tui-sidebar → tui-overlays  

## Template

```markdown
**Phase:** **N only** (exclusive)
**PRD:** <req IDs owned by this doc only>
…
## 6. Phase ownership
| This entire document | **N** |
```

## Maintainer checklist

| Check | Rule |
|-------|------|
| Phase header | Exactly one of 1 / 2 / 3 |
| Req IDs | No shared primary ownership across design docs |
| No multi-phase catalogs | Slash commands Phase 1 vs Phase 2 commands live in phase designs |
| Removed | `protocols-mcp-acp.md` (split into protocol-mcp + protocol-acp) |

**Last restructure:** 22 Jul 2026  
