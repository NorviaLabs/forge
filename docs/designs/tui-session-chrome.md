# TUI session identity chrome design

**Status:** Shipped (product)  
**Owner:** Mohit Ranka  
**Last updated:** 23 Jul 2026  
**Phase:** **10 only** (exclusive)  
**PRD:** **TUI-09** (primary)  
**Architecture:** Phase 10, decision #25  
**Related:** [tui-status-feedback.md](./tui-status-feedback.md), [tui-activity-feed.md](./tui-activity-feed.md), [tui-shell.md](./tui-shell.md), [tui-sidebar.md](./tui-sidebar.md), [connect-command.md](./connect-command.md), [../ui.md](../ui.md)

---

## 1. Problem / context

Operators need **ambient** session identity: provider, model, context budget, worktree, connect profile, and related selection state. Today those facts are **split**:

| Fact | Today |
|------|--------|
| Model | Status bar |
| Provider | Footer only (dim) |
| Context % | Status + sidebar gauge (no absolute size) |
| Connect profile | `/connect status` only |
| Web search backend | Config only; not in chrome |
| Tool inventory | Sidebar **count** only |

On terminals **&lt; 80 columns** the sidebar vanishes and most ambient data disappears with no compact fallback.

---

## 2. Goals & non-goals

**Goals**

- Single **session identity strip** (status bar, possibly 1–2 rows) with provider · model · ctx · profile · worktree (and optional search mode).  
- **Narrow layout**: never require sidebar to know model/provider/ctx.  
- Richer context presentation: % plus optional estimate / warn thresholds.  
- After `/model` or `/connect`, chrome updates immediately to the active selection.  
- Footer remains secondary (cwd, version, key hints)—not the only place for provider.

**Non-goals**

- Live cloud billing dashboards.  
- Editing ACL policy in the sidebar.  
- Replacing `/status` (it should **mirror** chrome fields).  
- Pixel-perfect multi-column density beyond terminal wrapping rules.

---

## 3. Design

### 3.1 Status bar information architecture (normative)

**Row 1 (required):**

```text
FORGE │ {status_pill} │ sess {short_id} │ {provider} · {model} │ ctx {pct}% │ {wt}
```

| Token | Source | Notes |
|-------|--------|-------|
| `status_pill` | session + `busy` | `idle` / `running` / `awaiting_hitl` / `failed` / `completed` |
| `short_id` | session uuid prefix | 8 chars |
| `provider` | `runtime.provider` | e.g. `litellm`, `mock` |
| `model` | `runtime.model_label` | LiteLLM model string; truncate mid with `…` if needed |
| `ctx {pct}%` | `context_usage_ratio` | Color: ok &lt;70%, warn ≥70%, danger ≥90% |
| `wt` | worktree on/off or short path | `wt off` / `wt on` |

**Row 2 (optional when width ≥ 100 or always if height allows):**

```text
profile {connect_profile|—} │ search {web_search_provider|off} │ tools {n}
```

| Token | Source |
|-------|--------|
| `connect_profile` | Active connect profile id if any (`xai`, `opencode_go`, …) |
| `web_search_provider` | From config / session: `mock` / `tavily` / … / `off` if tool not registered |
| `tools {n}` | `list_tools().len()` |

If only one status row is available, pack **provider · model · ctx** on row 1 and drop profile/search to overflow (`…`) rather than footer-only.

### 3.2 Truncation rules

Priority order when width is tight (drop first from lowest priority):

1. Keep: brand, status pill, model (or short model), ctx %  
2. Keep if possible: provider, session id  
3. Drop/compact: worktree text → `wt`  
4. Drop: profile, search, tools count (sidebar/row2)  

Never drop **both** model and ctx on a usable (≥40 wide) terminal.

### 3.3 Narrow layout (&lt; 80 cols)

| Element | Behavior |
|---------|----------|
| Sidebar | Hidden (unchanged threshold OK) |
| Status strip | **Must** still show model + ctx (+ provider if space) |
| Feedback strip | Still rendered (TUI-08) |
| Footer | cwd may truncate; provider **may** remain as backup but is not primary |

### 3.4 Context presentation

Minimum:

- `ctx {pct}%`

Stretch (if token estimate available from core without new protocols):

- `ctx {pct}% ~{used}k/{cap}k`  
- Or `ctx {pct}% · handoff@80%`

Color thresholds (normative):

| Ratio | Style |
|-------|--------|
| &lt; 0.70 | muted/ok |
| 0.70–0.89 | warn |
| ≥ 0.90 | danger |

### 3.5 Selection refresh

| Operator action | Chrome update |
|-----------------|---------------|
| `/model` pick | `model` (+ provider label if litellm) updates before next draw |
| `/connect` success | `profile` + model defaults update |
| `/connect disconnect` | profile → `—` |
| Config-only restart fields | Documented; no live edit of forge.toml required in v1 |

`/status` notices **must** list the same fields as the chrome (single source of truth helper: `SessionChromeModel`).

### 3.6 Sidebar relationship

Sidebar remains **detail** (TUI-03):

- Gauge (visual)  
- Event tail / activity feed (TUI-10)  
- Tool ACL counts  

It must not be the **only** place for provider/model/ctx.

### 3.7 Data model

```rust
// illustrative
struct SessionChromeModel {
    status_label: String,
    session_short: String,
    provider: String,
    model: String,
    ctx_pct: f64,
    worktree_on: bool,
    connect_profile: Option<String>,
    web_search_label: Option<String>, // "mock" | "tavily" | "off"
    tools_visible: usize,
    busy: bool,
}
```

Built in `TuiApp::refresh_chrome()` from `session` + `runtime` + connect/web_search config snapshots held on the app.

### 3.8 Testing

| Test | Assertion |
|------|-----------|
| Unit | Chrome model includes provider and model from runtime |
| TestBackend wide | Frame contains provider and model substrings |
| TestBackend narrow (width 60) | Frame still contains model or ctx (sidebar absent) |
| After mock model switch | Chrome model field updates |

---

## 4. Interfaces (crate sketch)

| Piece | Location |
|-------|----------|
| `SessionChromeModel` + `StatusBar` rewrite | `widgets/status.rs` |
| Optional second status row | `layout.rs` status height 1–2 |
| Connect profile id on app | `TuiApp` / connect store read |
| Web search label | From config snapshot at session start (+ refresh on config if any) |

---

## 5. Failure modes

| Case | Behavior |
|------|----------|
| Unknown profile | Show `profile —` |
| Web search disabled | `search off` or omit token |
| Extremely long model id | Truncate middle: `openai/…/mini` |

---

## 6. Phase ownership

| Item | Phase |
|------|-------|
| This entire document | **10** |
| Sidebar panels | Phase 4 [tui-sidebar.md](./tui-sidebar.md) (dependency) |
| Status feedback strip | [tui-status-feedback.md](./tui-status-feedback.md) |
| Activity feed content | [tui-activity-feed.md](./tui-activity-feed.md) |

---

## 7. Open questions

1. Whether status bar grows to 2 rows always vs only when width ≥ 100.  
2. Live refresh of web_search registration without restart (default: restart or session recreate).

---

## Related docs

- [tui-status-feedback.md](./tui-status-feedback.md)  
- [../ui.md](../ui.md) screen 15  
- [../prd.md](../prd.md) TUI-09  
