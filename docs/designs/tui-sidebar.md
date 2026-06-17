# TUI sidebar design

**Status:** Draft  
**Owner:** Mohit Ranka  
**Last updated:** 23 Jul 2026  
**Phase:** **4 only** (exclusive)  
**PRD:** TUI-03  
**Architecture:** §14 Phase 4  
**UI reference:** [../ui.md](../ui.md) home/status screens, sidebar sections  

---

## 1. Problem / context

Operators need continuous awareness of session identity, context pressure, what tools the model can see, and recent journal activity—without leaving the chat flow.

## 2. Goals & non-goals

**Goals**

- Fixed-width right sidebar with sections: **Session**, **Context budget**, **Tools (ACL)**, **Recent journal / events**.  
- Values refresh after each turn / event.  
- Context budget as ratio + bar (from `context_usage_ratio()`).

**Non-goals**

- Editing ACL policy in UI (config-only for Phase 4).  
- Full journal DB browser (tail only).  
- Chat rendering → [tui-conversation.md](./tui-conversation.md).

## 3. Design

### 3.1 Sections

| Section | Fields |
|---------|--------|
| Session | id (short), status, surface=`tui`, role=`generator` |
| Context budget | used %, bar; optional token estimate if available |
| Tools (ACL) | allowed count, MCP count if known, denied/hidden count if governance present |
| Recent journal | last N `TurnEvent`s (kind + detail truncated) |

### 3.2 Data sources (no new backend)

| Field | API |
|-------|-----|
| session id / status | `AgentSession` |
| context % | `session.context_usage_ratio()` |
| tools | `session.list_tools()` |
| events | `session.events` |
| worktree | `session.worktree_status()` (optional line under session) |

### 3.3 Width

Default ~28–32 columns; collapse to hidden if terminal width &lt; 80 (chat takes full width). Optional toggle key `Tab` or `Ctrl+B` (open).

## 4. Interfaces

```rust
pub struct SidebarModel { /* snapshot fields */ }
impl SidebarModel {
    pub fn from_session(session: &AgentSession) -> Self;
}
pub fn render_sidebar(frame, area, model: &SidebarModel);
```

## 5. Failure modes

| Case | Behavior |
|------|----------|
| No governance | Show tool count only; denied = 0 or “n/a” |
| No events | “—” empty journal |

## 6. Phase ownership

| Item | Phase |
|------|-------|
| This entire document | **4** |
| Exit | After a turn, sidebar budget/tools/events change without restart |

## Related docs

- [tui-shell.md](./tui-shell.md)  
- [context-lifecycle.md](./context-lifecycle.md)  
- [governance.md](./governance.md)  
