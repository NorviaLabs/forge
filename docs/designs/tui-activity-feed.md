# TUI activity feed & progressive busy design

**Status:** Shipped (product)  
**Owner:** Mohit Ranka  
**Last updated:** 23 Jul 2026  
**Phase:** **10 only** (exclusive)  
**PRD:** **TUI-10** (primary)  
**Architecture:** Phase 10, decision #26  
**Related:** [tui-status-feedback.md](./tui-status-feedback.md), [tui-session-chrome.md](./tui-session-chrome.md), [tui-sidebar.md](./tui-sidebar.md), [tui-conversation.md](./tui-conversation.md), [../ui.md](../ui.md)

---

## 1. Problem / context

Operators cannot reconstruct “what just happened” from the TUI:

- Sidebar “RECENT EVENTS” truncates to ~36 characters and only some `TurnEvent`s.  
- Busy state is binary (`busy` → status `running`, input dimmed) without phase (`model` vs `tool:web_search`).  
- Rate limits and worker failures do not form a scrollable timeline.  
- Tool cards help mid-turn but do not replace a global activity log.

Phase 10 adds an **activity feed** and **progressive busy** labels so failures and steps remain scannable.

---

## 2. Goals & non-goals

**Goals**

- Maintain an in-session **activity feed** (ring buffer) of operator-relevant steps.  
- Show feed in sidebar (wide) and optionally a compact “last activity” in feedback strip (narrow).  
- Progressive busy: `running · model` / `running · tool:{name}` / `running · connect`.  
- Integrate with TUI-08 so errors always create feed entries **and** chat banners.  
- Unit tests for feed push/cap and busy label formatting.

**Non-goals**

- Full journal browser / SQL UI.  
- Real-time OTEL flame graphs.  
- Cross-session persistent activity file (optional stretch).  
- Mouse-driven log filtering.

---

## 3. Design

### 3.1 Activity item

```rust
// illustrative
struct ActivityItem {
    ts: DateTime<Utc>,      // or Instant + display clock
    kind: ActivityKind,     // Model, Tool, Connect, Slash, System, Error
    summary: String,        // single line, redacted
    detail: Option<String>, // optional longer text for chat/banner
    severity: Severity,     // Info, Warn, Error, Ok
}

enum ActivityKind {
    Model,
    Tool,
    Connect,
    Slash,
    System,
    Error,
    Hitl,
    Context,
}
```

### 3.2 When to push

| Event | kind | severity | summary example |
|-------|------|----------|-----------------|
| User message accepted | System | Info | `user message accepted` |
| Model call start | Model | Info | `model call started` |
| Model call ok | Model | Ok | `model ok · 1.2s` |
| Model error / 429 | Error | Error | `model error · rate limited (429)` |
| Tool start | Tool | Info | `tool web_search started` |
| Tool done | Tool | Ok | `tool web_search done` |
| Tool error | Tool | Error | `tool bash failed` |
| HITL wait | Hitl | Warn | `hitl waiting · git push` |
| Context reset | Context | Warn | `context handoff reset` |
| Connect success | Connect | Ok | `connected xai` |
| Slash command | Slash | Info | `/status` |

Sources: wrap `dispatch_line`, `run_user_message` result paths, overlay connect completion, and existing session events at draw/merge time.

### 3.3 Ring buffer

| Parameter | Default |
|-----------|---------|
| Max items | 50 |
| Sidebar show | last 10–12 |
| Truncation per line | 48–60 chars in sidebar; full in detail/banner |

Drop oldest on overflow. Secrets redacted with existing redactors when present.

### 3.4 UI placement

**Wide (≥80):** Sidebar section title `ACTIVITY` (replaces or upgrades “RECENT EVENTS”):

```text
ACTIVITY
12:04 model ok
12:04 tool web_search done
12:05 model error · 429
```

Show newest at bottom (chronological) or top—**newest at bottom** matching chat mental model.

**Narrow (&lt;80):** No sidebar → last error/info already on **feedback strip** (TUI-08); optional status pill subtitle uses progressive busy only.

### 3.5 Progressive busy (normative)

```rust
enum BusyPhase {
    Idle,
    Model,
    Tool { name: String },
    Connect,
    Other(String),
}
```

Status pill / chrome:

| Phase | Label |
|-------|--------|
| Idle | `idle` (or session status) |
| Model | `running · model` |
| Tool | `running · tool:web_search` |
| Connect | `running · connect` |

Chat assistant header may show `ASSISTANT · working · model` while streaming/waiting.

Set phase at start of work; clear to Idle on turn complete or error (error also pushes feed + banner).

### 3.6 Relationship to TUI-08 / TUI-09

| Concern | Owner |
|---------|--------|
| Must not be invisible | TUI-08 dual-write |
| Status pill + provider/model | TUI-09 chrome |
| Timeline + busy phase | **TUI-10** (this doc) |

Error path sequence:

1. Classify error (TUI-08 copy)  
2. `activity.push(Error, …)`  
3. `set_feedback(Error, …)`  
4. `push_error_banner(…)`  
5. `busy_phase = Idle`  

### 3.7 Testing

| Test | Assertion |
|------|-----------|
| Unit | 60 pushes → len ≤ 50; oldest dropped |
| Unit | Busy phase formats `running · tool:x` |
| TestBackend | After forced error, sidebar or frame contains activity summary substring |
| Redaction | Activity summary never contains synthetic API key |

---

## 4. Interfaces (crate sketch)

| Piece | Location |
|-------|----------|
| `ActivityFeed` ring buffer | `forge-tui/src/activity.rs` |
| `BusyPhase` on `TuiApp` | `app.rs` |
| Sidebar render | `sidebar.rs` |
| Hooks in dispatch / run paths | `app.rs` |

---

## 5. Failure modes

| Case | Behavior |
|------|----------|
| High-frequency tool spam | Coalesce consecutive identical tool names optional v1.1; v1 keep all until cap |
| Missing timestamps | Show relative `now` / omit clock |
| Overlay open | Feed still updates under modal; visible after close |

---

## 6. Phase ownership

| Item | Phase |
|------|-------|
| This entire document | **10** |
| Feedback strip / banners | [tui-status-feedback.md](./tui-status-feedback.md) |
| Session chrome fields | [tui-session-chrome.md](./tui-session-chrome.md) |
| Base sidebar layout | Phase 4 [tui-sidebar.md](./tui-sidebar.md) |

---

## 7. Open questions

1. Persist last N activity lines into journal for resume (default: no).  
2. Coalescing strategy for streaming tool spam.

---

## Related docs

- [tui-status-feedback.md](./tui-status-feedback.md)  
- [tui-session-chrome.md](./tui-session-chrome.md)  
- [../ui.md](../ui.md) screen 16  
- [../prd.md](../prd.md) TUI-10  
