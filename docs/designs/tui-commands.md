# TUI slash commands (canonical catalog)

**Status:** Draft  
**Owner:** Mohit Ranka  
**Last updated:** 22 Jul 2026  
**PRD:** Multi-surface interfaces (capability-level only)  
**Architecture:** §5.11  
**UI mockups:** [../ui.md](../ui.md)  
**Related:** [surfaces.md](./surfaces.md), [configuration.md](./configuration.md), [durable-execution.md](./durable-execution.md)

---

## 1. Problem / context

Operators need session control that must **not** depend on the LLM (resume, approve, status, model switch). Slash commands are the TUI affordance for surface-local control.

This document is the **canonical command list**. The PRD does not enumerate commands. [ui.md](../ui.md) shows visuals only.

## 2. Goals & non-goals

**Goals**

- Discoverable palette (`/`) with stable names.  
- No model invocation for these commands.  
- Map each command to PRD capabilities / req IDs.  
- Safe defaults (destructive actions confirm when needed).

**Non-goals**

- Plugin-defined slash commands in Phase 1.  
- Shell execution via `/` (use tools under policy instead).  
- Replacing CLI flags for headless CI (headless may mirror a subset as flags).

## 3. Design

### 3.1 Behavior rules

1. Parsed entirely in the surface (TUI); dispatched to core session APIs.  
2. Failures show inline errors; do not crash the session.  
3. Commands that need args show usage on missing args.  
4. During `model_stream` / `tool_running`, only `/cancel` (and maybe `/status`) are active unless noted.

### 3.2 Catalog

| Command | Args | Behavior | Phase | Req / capability |
|---------|------|----------|-------|------------------|
| `/help` | `[cmd]` | List commands or detail one | 1 | DX |
| `/status` | — | Session, model, budget, journal cursor, worktree, HITL | 1 | Observability UX |
| `/resume` | `<session_id>` | Open journal, replay, continue (DUR-02) | 1 | DUR-02 |
| `/cancel` | — | Cancel current turn / stream; journal cancel | 1 | Loop control |
| `/model` | `[provider] [model]` | Interactive picker or set; config-only switch | 1 | Multi-provider |
| `/reset` | — | Force handoff write + clear window + rehydrate | 2 | CTX-02 |
| `/compact` | — | Request compaction path (summary) if enabled; else alias guidance to `/reset` | 2 | CTX |
| `/approve` | — | Approve pending HITL tool | 2 | DUR-03 |
| `/deny` | — | Deny pending HITL tool | 2 | DUR-03 |
| `/worktree` | `status \| merge \| discard` | Inspect or finish isolated worktree | 2 | CTX-03 |
| `/journal` | `[tail n]` | Show recent journal events (redacted) | 1 | DUR / debug |
| `/tools` | — | List tools visible under current ACL | 1 | SEC-02 UX |
| `/cost` | — | Token usage summary for session | 1 | CTX budget UX |
| `/quit` | — | Exit TUI (session remains on disk) | 1 | DX |

### 3.3 Detailed notes

#### `/resume <session_id>`

- Loads journal; runs replay algorithm.  
- Incomplete non-idempotent intents → fail-safe (see durable design).  
- If `awaiting_hitl`, lands in approval UX.

#### `/model`

- Without args: picker of configured providers.  
- Does not accept API keys in chat; keys via env/vault only.  
- Open: applies to current session mid-flight vs next user message only—see model-providers open questions.

#### `/approve` · `/deny`

- Only valid in `awaiting_hitl`.  
- Equivalent to modal buttons in UI mockup.  
- Journals `hitl_resume` with actor `tui:<user>`.

#### `/worktree`

| Subcommand | Effect |
|------------|--------|
| `status` | Paths, branch, dirty file count |
| `merge` | Merge worktree changes into target branch / primary per policy |
| `discard` | Confirm, then drop worktree |

#### `/reset` vs `/compact`

- **`/reset`**: hard reset + `progress.json` / `AGENTS.md` handoff (preferred long-horizon path).  
- **`/compact`**: optional summary compaction; may be deferred or thinner in Phase 2.

### 3.4 Headless mirrors (not slash)

| CLI / flag | Analog |
|------------|--------|
| `forge run --resume <id>` | `/resume` |
| `forge approve --session <id>` | `/approve` |
| exit codes | terminal status |

## 4. Interfaces

```text
TuiCommand = { name, args: Vec<String> }
Surface parses input → TuiCommand → core.session_control(...)
```

Palette filters by prefix; fuzzy match optional later.

## 5. Failure modes & edge cases

| Case | Behavior |
|------|----------|
| Unknown command | Suggest `/help` |
| `/approve` with no pending HITL | Error, no-op |
| `/resume` missing file | Error |
| `/worktree discard` | Require confirmation (`/worktree discard --yes` or prompt) |

## 6. Phase / rollout

See Phase column in catalog. Phase 1 ships control/debug commands even if HITL/worktree land in Phase 2 (commands can no-op with “not enabled” until then).

## 7. Open questions

1. Confirmation UX standard for destructive commands.  
2. Whether `/model` needs `/model list` subcommand only.  
3. Namespacing future plugin commands (`/ext:…`).

## Related docs

- [../ui.md](../ui.md) — visuals for palette, HITL, status  
- [surfaces.md](./surfaces.md)  
- [durable-execution.md](./durable-execution.md)  
