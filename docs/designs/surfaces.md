# Surfaces design

**Status:** Draft  
**Owner:** Mohit Ranka  
**Last updated:** 22 Jul 2026  
**PRD:** Multi-surface interfaces  
**Architecture:** §8 UI / multi-surface  
**Related:** [tui-commands.md](./tui-commands.md), [agent-loop.md](./agent-loop.md), [protocols-mcp-acp.md](./protocols-mcp-acp.md)

---

## 1. Problem / context

Forge must present one harness through TUI, headless CI, ACP IDE, and later channels—without forking agent logic.

## 2. Goals & non-goals

**Goals**

- Surfaces are **adapters**: render events, collect input, map transport.  
- One core loop, journal, tool path, governance.  
- Clear rules for what surfaces must not do.

**Non-goals**

- Pixel-identical UX across surfaces.  
- Always-on multi-tenant gateway as Phase 1 product shape (CLI binary first).

## 3. Design

### 3.1 Surface matrix

| Surface | Phase | Input | Output | Notes |
|---------|-------|-------|--------|-------|
| **TUI** | 1 | stdin keys, slash cmds | ratatui panels, modals | Primary interactive |
| **Headless** | 1 | CLI args, prompt file | logs, JSON, exit code | CI resume |
| **ACP** | 2 | ACP messages | ACP streams | IDE clients; first Phase 2 deliverable |
| **Channels** | 3 | Slack/TG/webhook | channel messages | Restricted ACL default |

### 3.2 Hard rules

1. Surfaces **must not** call model providers or MCP servers directly.  
2. Surfaces **must not** write tool side effects bypassing the journal.  
3. Secrets **must not** be collected into chat history; use env/vault.  
4. Display tool args **redacted**; large payloads show URI + summary.  
5. Channel surfaces default to **restricted** tool principals (architecture trust boundary).

### 3.3 Agent events (surface-facing)

| Event | TUI | Headless | ACP |
|-------|-----|----------|-----|
| `session_status` | status pill | log + exit mapping | map to protocol |
| `assistant_delta` | stream pane | log stream | stream |
| `tool_started` / `tool_finished` | tool cards | log lines | notifications |
| `context_lifecycle` | banner | log | notification |
| `hitl_required` | modal | exit/wait or poll API | prompt |
| `evaluator_report` | panel | log/artifact | notification |
| `trace_link` | footer | log field | metadata |

### 3.4 Headless exit codes (proposal)

| Code | Meaning |
|------|---------|
| 0 | completed success |
| 1 | failed / error |
| 2 | awaiting_hitl (needs human) |
| 3 | canceled |
| 4 | usage / config error |

### 3.5 TUI layout regions

See [ui.md](../ui.md). Implementation: ratatui + crossterm; status / chat / sidebar / input / modal layers.

### 3.6 Shared session control API

All surfaces ultimately call the same core methods: create/resume session, submit message, cancel, hitl decision, shutdown.

## 4. Interfaces

```rust
trait Surface: Send {
    async fn run(&mut self, handle: AgentHandle) -> Result<ExitStatus, SurfaceError>;
}
```

`AgentHandle` is cloneable, session-scoped, exposes subscribe(event stream) + control methods.

## 5. Failure modes & edge cases

| Case | Behavior |
|------|----------|
| TUI resize | Reflow; no session impact |
| Headless SIGTERM | Cooperative cancel + journal |
| ACP disconnect | Configurable: keep session vs fail |
| Channel abuse | Rate limit + restricted ACL |

## 6. Phase / rollout

| Phase | Deliver (fixed) |
|-------|------------------|
| 1 | TUI + headless only |
| 2 | ACP (required) |
| 3 | Channels |

## 7. Open questions

1. Can multiple surfaces attach to one live session simultaneously in Phase 1? (**Recommendation:** single surface per process in Phase 1.)  
2. JSON schema for headless event sink.  
3. HITL in CI: block with exit 2 vs webhook callback.

## Related docs

- [tui-commands.md](./tui-commands.md)  
- [../ui.md](../ui.md)  
- [protocols-mcp-acp.md](./protocols-mcp-acp.md)  
