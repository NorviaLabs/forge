# TUI always-visible status feedback design

**Status:** Shipped (product)  
**Owner:** Mohit Ranka  
**Last updated:** 23 Jul 2026  
**Phase:** **10 only** (exclusive)  
**PRD:** **TUI-08** (primary)  
**Architecture:** Phase 10, decision #24  
**Related:** [tui-session-chrome.md](./tui-session-chrome.md), [tui-activity-feed.md](./tui-activity-feed.md), [tui-conversation.md](./tui-conversation.md) (Phase 4), [tui-shell.md](./tui-shell.md) (Phase 4), [../ui.md](../ui.md)

---

## 1. Problem / context

The full-screen TUI writes many operator-facing outcomes into `TuiApp.status_message` (slash results, connect errors, model/loop failures). **`draw()` does not render `status_message`**, so rate limits, failed model calls, and one-line slash results often **never appear on screen**. Notices help only when a handler fills `notices`; chat banners exist only for a few event kinds (HITL, context reset, validation).

Phase 10 closes this **silent failure** gap: every operator-relevant outcome must land in at least one **visible** surface.

---

## 2. Goals & non-goals

**Goals**

- Always render the latest status/feedback line (or multi-line notices) in the layout.  
- On model/tool/session **errors**, append a durable **error banner** in the conversation (not only an ephemeral string).  
- Severity styling: info / warn / error.  
- Dual-write policy: durable chat banner for failures; status line for “latest”; notices when multi-line is needed.  
- Unit + TestBackend coverage that a failed turn leaves visible text on the frame.

**Non-goals**

- Redesigning slash autocomplete or history (Phases 7–8).  
- Full observability/OTEL UI (Phase 3).  
- Changing agent loop semantics or journal schema (may *consume* richer error strings from core).  
- Toast animations beyond terminal constraints.

---

## 3. Design

### 3.1 Surfaces

| Surface | Role | Lifetime |
|---------|------|----------|
| **Feedback strip** | 1–2 lines between chat and input; always reserved when non-empty | Latest message; overwritten by next feedback |
| **Notices panel** | Multi-line (`/help`, connect list) above feedback strip | Until cleared (Esc or next submit policy) |
| **Chat error/info banner** | Durable `ChatItem::Banner` in conversation | Remains in scrollback |

### 3.2 Feedback strip (normative)

Layout (extends Phase 4 shell):

```text
[ status bar ]
[ chat (+ sidebar) ]
[ notices? ]          // existing multi-line
[ feedback strip ]    // NEW — 1 line (2 if wrapped / severity icon)
[ input ]
[ footer ]
```

Rules:

1. If `status_message` is non-empty **or** last feedback severity is error, render strip.  
2. Content = `status_message` (primary). Prefer redaction-safe text only.  
3. Style:
   - `Info` → muted/info  
   - `Warn` → amber  
   - `Error` → danger + optional `!` prefix  
4. Empty feedback: strip height 0 (reclaim space).  
5. Overlays may cover the strip; when overlay closes, strip still shows last message.

### 3.3 Dual-write policy

| Event | Feedback strip | Chat banner | Notices |
|-------|----------------|-------------|---------|
| Slash one-liner (`/cost`) | Yes | No | Optional |
| Slash multi-line (`/help`) | Yes (first line) | No | Yes (full) |
| Model/loop `Err` (e.g. 429) | Yes (full human message) | **Yes** Error banner | Optional detail |
| Tool hard failure already in tool card | Yes (short) | Prefer tool card; banner if no card | No |
| Connect success | Yes | Optional Info banner | List if needed |
| HITL | Modal owns UX | Existing warn banner OK | No |

**Invariant:** For `run_user_message` / model client errors, **never** only set `status_message` without a chat banner.

### 3.4 Error classification (operator copy)

Map common failures to short labels (string match / structured error when available):

| Signal | Operator line (example) |
|--------|-------------------------|
| rate limit / 429 | `Model error: rate limited (HTTP 429). Wait and retry, or /model.` |
| auth / 401 / 403 | `Model error: authentication failed. Check /connect or API key.` |
| timeout | `Model error: request timed out. Retry or check network/worker.` |
| worker down | `Model error: LiteLLM worker unavailable. Check worker process.` |
| other | `Model error: {redacted_message}` |

Do **not** print API keys, full env dumps, or raw secret-bearing URLs.

### 3.5 Conversation model

Extend banner usage (Phase 4 `BannerKind::Error` already exists):

```rust
// illustrative
fn push_system_error(conv_or_session_events, msg: String) {
    // ChatItem::Banner { text: msg, kind: BannerKind::Error }
}
```

Prefer appending via session `TurnEvent` with `kind = "ui_error"` or direct UI-only banner list on `TuiApp` if journal should not grow—**default: UI-only durable list on `TuiApp` + render into conversation model** so journal schema stays unchanged unless core already emits events.

**Recommended v1:** `TuiApp.activity_banners: Vec<Banner>` merged into `ConversationModel` at draw time (no journal change). Phase 10 activity feed ([tui-activity-feed.md](./tui-activity-feed.md)) may later unify.

### 3.6 Clearing rules

| Action | Feedback strip | Notices | Chat banners |
|--------|----------------|---------|--------------|
| New user submit | Keep last error until new outcome | Clear (current behavior OK) | Keep |
| Esc (no overlay) | Clear strip if info; **keep** error until next success | Clear | Keep |
| Successful model turn | Replace strip with short ok/idle hint optional | Clear | Keep |
| Overlay open | Unchanged under modal | Hidden while overlay | Keep in chat |

### 3.7 Testing

| Test | Assertion |
|------|-----------|
| Unit | After simulated model error, `status_message` non-empty **and** banner list non-empty |
| TestBackend | Frame buffer contains `"rate limited"` or `"Model error"` substring |
| Regression | `/status` still populates strip and/or notices |
| Redaction | Synthetic key material never appears in strip text |

---

## 4. Interfaces (crate sketch)

| Piece | Location |
|-------|----------|
| `FeedbackSeverity`, `FeedbackModel` | `forge-tui` widgets or app |
| `FeedbackBar` widget | `widgets/feedback.rs` |
| Layout region `feedback` | `layout.rs` |
| Dual-write helpers | `app.rs` (`set_feedback`, `push_error_banner`) |
| Draw path | `TuiApp::draw` |

---

## 5. Failure modes

| Case | Behavior |
|------|----------|
| Extremely long error | Truncate strip with `…`; full text in chat banner (wrapped) |
| Empty error string | Show generic `Operation failed` |
| Concurrent busy | Strip shows `running…` or last progress (activity feed owns detail) |

---

## 6. Phase ownership

| Item | Phase |
|------|-------|
| This entire document | **10** |
| Chat item types / tool cards | Phase 4 [tui-conversation.md](./tui-conversation.md) (dependency) |
| Shell regions | Phase 4 [tui-shell.md](./tui-shell.md) (extended here) |
| Session chrome fields | [tui-session-chrome.md](./tui-session-chrome.md) (Phase 10 sibling) |
| Activity feed | [tui-activity-feed.md](./tui-activity-feed.md) (Phase 10 sibling) |

---

## 7. Open questions

1. Whether error banners should also append a system `Message` for ACP/headless parity (default: TUI-only).  
2. Auto-dismiss timer for info-level strip (default: no timer; Esc clears info).

---

## Related docs

- [tui-session-chrome.md](./tui-session-chrome.md)  
- [tui-activity-feed.md](./tui-activity-feed.md)  
- [../ui.md](../ui.md) screens 15–16  
- [../prd.md](../prd.md) TUI-08  
