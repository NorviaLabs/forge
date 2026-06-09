# TUI slash commands (Phase 1 catalog)

**Status:** Draft  
**Owner:** Mohit Ranka  
**Last updated:** 22 Jul 2026  
**Phase:** **1 only** (exclusive)  
**PRD:** Multi-surface (TUI control)  
**Architecture:** §5.11  
**UI mockups:** [../ui.md](../ui.md)  
**Related:** [surfaces.md](./surfaces.md)

---

## 1. Problem / context

Phase 1 operators need non-LLM session control. **This catalog is Phase 1 commands only.** Phase 2+ commands are specified in their owning designs (not here).

## 2. Goals & non-goals

**Goals**

- Discoverable `/` palette; no model invocation.  
- Map to Phase 1 capabilities only.

**Non-goals**

- Phase 2: `/approve`, `/deny` → [durable-hitl.md](./durable-hitl.md); `/reset`, `/compact`, `/cost` → [context-lifecycle.md](./context-lifecycle.md); `/worktree` → [workspace-isolation.md](./workspace-isolation.md).  
- Plugin commands.

## 3. Phase 1 catalog

| Command | Args | Behavior | Req |
|---------|------|----------|-----|
| `/help` | `[cmd]` | List or detail Phase 1 commands | DX |
| `/status` | — | Session, model, journal cursor (no Phase 2 budget/HITL fields required) | DX |
| `/resume` | `<session_id>` | Replay journal (DUR-02) | DUR-02 |
| `/cancel` | — | Cancel current turn | Loop |
| `/model` | `[provider] [model]` | Config-only model switch | Multi-provider |
| `/journal` | `[tail n]` | Recent journal events (redacted) | DUR |
| `/tools` | — | List registered tools (built-in + MCP) | CORE-01/02 |
| `/quit` | — | Exit TUI; session remains on disk | DX |

### Headless mirrors

| CLI | Analog |
|-----|--------|
| `forge run --resume <id>` | `/resume` |

## 4. Phase ownership

| Item | Phase |
|------|-------|
| This entire document | **1** |
| Unknown command that is Phase 2-only | Error: “requires Phase 2” (not a silent no-op) |

## 5. Open questions

1. `/model` mid-session vs next message.  
2. Fuzzy palette matching.

## Related docs

- [../ui.md](../ui.md)  
- [durable-hitl.md](./durable-hitl.md) (Phase 2 commands)  
- [context-lifecycle.md](./context-lifecycle.md) (Phase 2 commands)  
- [workspace-isolation.md](./workspace-isolation.md) (Phase 2 commands)  
