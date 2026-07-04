# TUI shell design

**Status:** Draft  
**Owner:** Mohit Ranka  
**Last updated:** 23 Jul 2026  
**Phase:** **4 only** (exclusive)  
**PRD:** TUI-01  
**Architecture:** Phase 4  
**UI reference:** [../ui.md](../ui.md) layout regions, screens 01 / 06 / 11  

---

## 1. Problem / context

Phase 1 ships only a **line-mode REPL**. Operators need a full-screen terminal app that matches the mockup information architecture: always-visible session chrome, chat, sidebar, and input—without a second agent implementation.

## 2. Goals & non-goals

**Goals**

- Full-terminal **ratatui + crossterm** application.  
- Fixed layout regions: **status bar · chat · sidebar · input · footer**.  
- Single event loop: keyboard → commands / agent actions → redraw.  
- Entry point: `forge` (same `AgentSession` as `repl` / headless).

**Non-goals**

- OS window chrome / traffic lights from HTML mockups.  
- Conversation rendering details → [tui-conversation.md](./tui-conversation.md).  
- Sidebar content → [tui-sidebar.md](./tui-sidebar.md).  
- Modals/palettes → [tui-overlays.md](./tui-overlays.md).  
- Input **command history** (Up/Down) → Phase 7 [tui-input-history.md](./tui-input-history.md).  
- New harness protocols.

## 3. Design

### 3.1 Layout (normative)

```text
┌ status bar ──────────────────────────────────────────────────┐
├ main (chat) ──────────────────────────┬ sidebar ─────────────┤
│                                       │                      │
│                                       │                      │
├ input ────────────────────────────────┴──────────────────────┤
└ footer ──────────────────────────────────────────────────────┘
```

| Region | Contents (minimum) |
|--------|-------------------|
| Status | Brand `FORGE`, status pill, session id, model, ctx %, worktree flag |
| Main | Reserved for conversation widget (TUI-02) |
| Sidebar | Reserved for sidebar widget (TUI-03) |
| Input | `❯` prompt, editable line, key hints |
| Footer | version, cwd, provider |

### 3.2 App state (shell-owned)

| Field | Role |
|-------|------|
| `mode` | `Normal` \| `Overlay` (overlay type owned by TUI-04) |
| `input` | Current input buffer + cursor |
| `should_quit` | Exit flag |
| `session` | Handle to core `AgentSession` (or async job handle) |

### 3.3 Event loop

1. Draw frame.  
2. Poll crossterm events (timeout for agent progress redraws).  
3. On Enter: if input starts with `/` → slash handler; else submit user message to core.  
4. On Esc: cancel turn if running; else dismiss overlay if any.  
5. On Ctrl+C / Ctrl+D: quit (confirm optional).  
6. **Phase 8:** typing `/` stays in the main textbox (inline slash)—see [tui-slash-inline.md](./tui-slash-inline.md); palette is opt-in (e.g. Ctrl+K).

Agent work runs without blocking the UI forever: either short poll of async task or redraw on interval while status is `running`.

### 3.4 Theme tokens (from ui.md)

| Token | Use |
|-------|-----|
| Teal accent | Brand, focus, primary buttons |
| Green | ok / idle-success |
| Amber | warn / HITL / compacting |
| Red | error / deny |
| Blue | info / session pills |
| Dim | muted labels |

### 3.5 Crate layout (suggested)

```text
forge-tui/
  src/
    lib.rs          # ExitCode, re-exports
    commands.rs     # existing slash parse (Phase 1–3)
    app.rs          # App + run()  (this design)
    layout.rs       # rect splits
    widgets/
      status.rs
      input.rs
      footer.rs
```

## 4. Interfaces

```rust
pub struct TuiApp { /* ... */ }
pub async fn run_tui(session: AgentSession, cfg: TuiRuntimeConfig) -> Result<ExitCode, TuiError>;
```

CLI: `forge [--resume UUID] [--mock] [--worktree]`.

## 5. Failure modes

| Case | Behavior |
|------|----------|
| Terminal too small | Show minimum-size message; refuse draw |
| Agent error | Banner in chat region; status pill `failed` |
| Resize | Relayout on next frame |

## 6. Phase ownership

| Item | Phase |
|------|-------|
| This entire document | **4** |
| Exit | Full-screen app runs; input submits to core; layout regions visible |

## 7. Open questions

1. Async runtime bridging (tokio + ratatui) preferred pattern.  
2. Whether `repl` remains default interactive or `tui` becomes default.

## Related docs

- [tui-conversation.md](./tui-conversation.md)  
- [tui-sidebar.md](./tui-sidebar.md)  
- [tui-overlays.md](./tui-overlays.md)  
- [../ui.md](../ui.md)  
