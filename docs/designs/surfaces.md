# Surfaces design (TUI + headless)

**Status:** Draft  
**Owner:** Mohit Ranka  
**Last updated:** 22 Jul 2026  
**Phase:** **1 only** (exclusive)  
**PRD:** Multi-surface (Phase 1 slice: TUI + headless)  
**Architecture:** §8  
**Related:** [tui-commands.md](./tui-commands.md), [agent-loop.md](./agent-loop.md)

---

## 1. Problem / context

Phase 1 product surfaces: interactive terminal and CI headless—adapters over one core.

## 2. Goals & non-goals

**Goals**

- TUI and headless are adapters only (no direct model/MCP).  
- Same session control API.  
- Redacted tool display; no secrets in chat.

**Non-goals**

- ACP → [protocol-acp.md](./protocol-acp.md) (Phase 2).  
- Channels → [channels.md](./channels.md) (Phase 3).

## 3. Design

| Surface | Input | Output |
|---------|-------|--------|
| **TUI** | keys, slash cmds | ratatui panels |
| **Headless** | CLI args, prompt | logs, JSON, exit codes |

### Headless exit codes

| Code | Meaning |
|------|---------|
| 0 | success |
| 1 | failed |
| 3 | canceled |
| 4 | config error |

(Exit code `2` reserved for Phase 2 `awaiting_hitl`.)

### Agent events (Phase 1)

`session_status`, `assistant_delta`, `tool_started` / `tool_finished`, `trace_link` (local id).

Phase 2+ events (`hitl_required`, `context_lifecycle`, `evaluator_report`) are not required for Phase 1 surfaces.

## 4. Phase ownership

| Item | Phase |
|------|-------|
| This entire document | **1** |
| Exit | Same session works in TUI and headless |

## Related docs

- [tui-commands.md](./tui-commands.md)  
- [protocol-acp.md](./protocol-acp.md) (Phase 2)  
- [channels.md](./channels.md) (Phase 3)  
