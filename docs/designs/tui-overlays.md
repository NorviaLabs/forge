# TUI overlays design

**Status:** Draft  
**Owner:** Mohit Ranka  
**Last updated:** 23 Jul 2026  
**Phase:** **4 only** (exclusive)  
**PRD:** TUI-04  
**Architecture:** Phase 4  
**UI reference:** [../ui.md](../ui.md) screens 04, 07, 08  

---

## 1. Problem / context

HITL approval, command discovery, and model switching need modal UX that does not destroy chat context—keyboard-first, matching mockups.

## 2. Goals & non-goals

**Goals**

- **HITL modal**: tool, redacted args, reason; Approve / Deny / Details.  
- **Slash palette**: filterable list of commands (existing `parse_slash` / help catalog).  
- **Model picker**: list configured providers/models; selection updates session config request (same rules as `/model`).  
- Esc dismisses overlay (HITL may require explicit deny instead of Esc—see open questions).

**Non-goals**

- Mouse-only interactions.  
- New slash commands (owned by Phase 1/2 command catalogs).  
- Vault secret entry in UI (still env/vault only).

## 3. Design

### 3.1 Overlay stack

Shell `mode = Overlay(OverlayKind)`:

| Kind | Trigger | Actions |
|------|---------|---------|
| `Hitl` | `pending_hitl.is_some()` auto-open or focus | `a` approve, `d` deny |
| `SlashPalette` | **Phase 8:** open via **Ctrl+K** (or equivalent)—**not** auto on typing `/` in the main textbox. Phase 4 historically opened on `/`; superseded for primary entry by [tui-slash-inline.md](./tui-slash-inline.md). | ↑↓ Enter Esc |
| `ModelPicker` | `/model` without args or dedicated key | ↑↓ Enter Esc |

Only one overlay at a time. Background chat remains visible (dimmed).

### 3.2 HITL modal content

From `HitlPayload`:

- Tool name  
- Redacted args (pretty JSON, truncated)  
- Reason string  
- Footer: Deny `d` · Approve `a`  

On approve/deny: call `session.resolve_hitl` then close overlay.

### 3.3 Slash palette

- Filter by substring on command name.  
- Source: static list aligned with [tui-commands.md](./tui-commands.md) + Phase 2 commands already in parser.  
- Enter inserts command into input or executes immediately for no-arg commands (`/status`, `/tools`, …)—implementation choice documented in code; default: **execute no-arg**, **insert** for arg-taking.

### 3.4 Model picker

- Rows: configured / common LiteLLM model strings (Phase 5); after Phase 6 `/connect`, include models from connected profiles (Grok, OpenCode Go).  
- Phase 6 also adds a **connect** overlay entry points via `/connect` (see [connect-command.md](./connect-command.md))—not a separate product client.  
- Selection sets provider/model for **next** session or prints “restart to apply” consistent with current CLI behavior until hot-swap exists.

## 4. Interfaces

```rust
pub enum Overlay {
    Hitl { payload: HitlPayload },
    Slash { filter: String, selected: usize },
    Model { selected: usize },
}
pub fn handle_overlay_key(app: &mut TuiApp, key: KeyEvent) -> bool; // consumed?
```

## 5. Failure modes

| Case | Behavior |
|------|----------|
| Approve with no pending HITL | Error toast; close overlay |
| Empty palette filter | Show “no matches” |
| Terminal height too small for modal | Clamp modal; scroll body |

## 6. Phase ownership

| Item | Phase |
|------|-------|
| This entire document | **4** |
| Exit | HITL + `/` palette usable by keyboard alone in `forge` |

## 7. Open questions

1. Esc on HITL = deny vs cancel-dismiss only. **Recommendation:** Esc dismisses focus but keeps `awaiting_hitl` until explicit deny/approve.  
2. Global key for palette (`/` only vs Ctrl+K).

## Related docs

- [durable-hitl.md](./durable-hitl.md)  
- [tui-commands.md](./tui-commands.md)  
- [tui-shell.md](./tui-shell.md)  
- [../ui.md](../ui.md)  
