# TUI conversation design

**Status:** Draft  
**Owner:** Mohit Ranka  
**Last updated:** 23 Jul 2026  
**Phase:** **4 only** (exclusive)  
**PRD:** TUI-02  
**Architecture:** Phase 4  
**UI reference:** [../ui.md](../ui.md) screens 02, 03, 05, 10, 12  

---

## 1. Problem / context

Operators must see what the agent said, which tools ran, validation failures, context resets, and evaluator reports—in a scannable chat stream, not a plain log dump.

## 2. Goals & non-goals

**Goals**

- Render **user / assistant / system / tool** roles with distinct labels/colors.  
- **Tool cards**: name, safe args summary, state (running / done / blocked).  
- Banners for context lifecycle, validation errors, evaluator reports.  
- Scrollable history; auto-follow when at bottom.  
- Never show raw secrets (use redacted args from governance).

**Non-goals**

- Shell chrome → [tui-shell.md](./tui-shell.md).  
- Sidebar metrics → [tui-sidebar.md](./tui-sidebar.md).  
- Modal HITL UI → [tui-overlays.md](./tui-overlays.md) (chat may show blocked tool card).  
- True token-by-token streaming from all providers (Phase 1 model client is non-stream complete; UI may show “running…” then fill).

## 3. Design

### 3.1 View model

Map `AgentSession.messages` + `events` + `pending_hitl` into a list of `ChatItem`:

| Item | Source |
|------|--------|
| `SystemLine` | system messages, lifecycle banners |
| `UserBubble` | user messages |
| `AssistantBubble` | assistant text |
| `ToolCard` | tool messages / events (`tool`, `validation`, `hitl_wait`) |
| `EvalBanner` | feedback gate events (when Phase 3 gate wired into session events) |

### 3.2 Tool card states

| State | Visual |
|-------|--------|
| `running` | blue/info border; spinner or `● running` |
| `done` | green; short body or offload URI |
| `blocked` | amber; HITL or policy deny |
| `error` | red; validation or execution error |

Args: **redacted** JSON summary (keys only or governance `redact_args`).

### 3.3 Streaming / run affordance

While `status == Running` and a turn is in flight:

- Status bar shows `running` (shell).  
- Optional “Streaming…” / “Working…” line in chat.  
- Input dimmed or accepts only Esc cancel if supported.

When Phase 1 model returns complete text in one shot, still show intermediate “model step” / tool running from session events.

### 3.4 Scroll

- Mouse wheel / PageUp / PageDown / Ctrl+U / Ctrl+D.  
- Pin to bottom when user is at end; unpin on scroll up.

## 4. Interfaces

```rust
pub struct ConversationWidget { items: Vec<ChatItem>, scroll: u16, follow: bool }
impl ConversationWidget {
    pub fn from_session(session: &AgentSession) -> Self;
    pub fn render(&self, frame, area);
}
```

## 5. Failure modes

| Case | Behavior |
|------|----------|
| Empty session | Idle empty-state copy (ui.md home) |
| Huge tool body | Prefer offload URI line already in message content |
| Redaction missing | Fail closed: show `[redacted]` for unknown sensitive keys if present |

## 6. Phase ownership

| Item | Phase |
|------|-------|
| This entire document | **4** |
| Exit | Mock multi-tool session shows distinct roles + tool cards |

## Related docs

- [tui-shell.md](./tui-shell.md)  
- [agent-loop.md](./agent-loop.md)  
- [../ui.md](../ui.md)  
