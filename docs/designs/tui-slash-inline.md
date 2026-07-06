# TUI inline slash commands design

**Status:** Shipped (product)  
**Owner:** Mohit Ranka  
**Last updated:** 23 Jul 2026  
**Phase:** **8 only** (exclusive)  
**PRD:** TUI-06  
**Architecture:** Phase 8, decision #21  
**Related:** [tui-slash-autocomplete.md](./tui-slash-autocomplete.md) (Phase **8.1** Tab + highlight), [tui-shell.md](./tui-shell.md), [tui-overlays.md](./tui-overlays.md), [tui-commands.md](./tui-commands.md), [tui-input-history.md](./tui-input-history.md)

---

## 1. Problem / context

Phase 4 TUI currently treats `/` at the start of the input bar as an immediate open of the **slash palette** and **clears** the textbox. Operators cannot type a full command inline (e.g. `/status`, `/connect list`, `/worktree status`) in the main field the way the line-mode REPL does.

Phase 8 makes the **main textbox** the primary place to enter **top-level slash commands**: type `/…` then **Enter** to run via the existing `parse_slash` / `dispatch_line` path. The palette stays a **discovery** aid, not a hijack of `/`.

## 2. Goals & non-goals

**Goals**

- Typing `/` **inserts** `/` into the input bar (does **not** auto-open palette or clear text).  
- **Enter** on a line starting with `/` dispatches through the same slash handler as today (`dispatch_line` / `parse_slash`).  
- All registered top-level slash commands (Phases 1–6+) work when typed fully in the textbox.  
- Optional: open palette explicitly (e.g. **Ctrl+K**, or empty input + a dedicated key)—documented and testable.  
- History (Phase 7) continues to store submitted slash lines.  
- Unit tests: key path does not open overlay on `/` alone; Enter runs `/status`-style commands.  
- **Phase 8.1:** Tab autocomplete + highlight — see [tui-slash-autocomplete.md](./tui-slash-autocomplete.md) (TUI-07).

**Non-goals**

- Redefining the slash catalog (still owned by phase-specific command docs).  
- Removing the slash palette (it remains for discovery).  
- Fuzzy NLP beyond catalog filter (8.1 defines filter + Tab).  
- Nested/subcommand UIs beyond existing parse rules.

## 3. Design

### 3.1 Normative input behavior (no overlay)

| Input | Behavior (Phase 8) |
|-------|---------------------|
| Char `/` | Insert `/` into textbox (even when text is empty) |
| Further chars | Normal insert (`status`, space, args, …) |
| Enter | If line starts with `/` → `parse_slash` + dispatch; else user message to agent |
| Empty Enter | No-op (unchanged) |

**Removed (Phase 4 default):**

```text
// DELETE this behavior
Key '/' when empty → open palette + clear input
after insert when text == "/" → clear + open palette
```

### 3.2 Palette discovery (retained)

| Trigger | Behavior |
|---------|----------|
| **Ctrl+K** (recommended) | Open slash palette overlay with empty filter |
| Optional: `/help` then palette | Unchanged |
| From palette Enter | Unchanged: execute no-arg cmds or insert template into textbox |

Palette Up/Down/Enter/Esc remain as Phase 4. History Up/Down inactive while palette open (Phase 7 rule).

### 3.3 Dispatch path (unchanged semantics)

```text
Enter → take line → history.push(line) → dispatch_line(line)
  dispatch_line:
    if parse_slash(line).is_some() → slash handler
    else → session.run_user_message(line)
```

No second command parser. Args work as in REPL: `/connect list`, `/worktree discard --yes`, `/resume <uuid>`.

### 3.4 UX copy

- Input hint may mention: `Type a task or /command · Ctrl+K commands`  
- Footer optional hint (not required for exit).

### 3.5 Edge cases

| Case | Behavior |
|------|----------|
| `/` only then Enter | Parse unknown/empty → usage or unknown command error in status (same as parser) |
| Leading spaces then `/cmd` | Trim before parse (match REPL `trim`) |
| Mid-line `/` (not leading) | Not a slash command; treat as normal user text on Enter |
| Overlay open | `/` and typing go to overlay handler if applicable; main textbox not primary |

### 3.6 Relationship to other phases

| Phase | Interaction |
|-------|-------------|
| 4 palette | Secondary discovery only |
| 6 `/connect` | Typed in textbox; TUI may still open connect modals after parse (OAuth / API key) |
| 7 history | Stores full `/cmd …` lines after Enter |

## 4. Interfaces

```rust
// TuiApp::handle_key — illustrative
KeyCode::Char('/') => {
    // Phase 8: always insert; do not open palette
    if !busy { input.insert('/'); }
}
KeyCode::Char('k') if ctrl => {
    overlay = Some(Overlay::slash_open(""));
}
KeyCode::Enter => {
    let line = input.take();
    history.push(&line);
    dispatch_line(&line).await?; // already handles /
}
```

## 5. Failure modes

| Case | Behavior |
|------|----------|
| Unknown `/foo` | Status/error from `CommandError::Unknown` (unchanged) |
| Usage error | Status shows usage string |
| Busy (agent running) | Ignore input (unchanged) |

## 6. Phase ownership

| Item | Phase |
|------|-------|
| This entire document / TUI-06 | **8** |
| Slash catalog contents | Phases 1/2/6 command docs |
| Palette widget | Phase 4 [tui-overlays.md](./tui-overlays.md) |
| Input history | Phase 7 |

## 7. Acceptance

1. In `forge`, type `/status` in the main textbox and press Enter → status updates (no forced palette).  
2. Typing a lone `/` leaves `/` visible in the textbox.  
3. Ctrl+K (or documented key) still opens the command palette.  
4. `/connect list` and other multi-token commands work when typed fully.  
5. Unit/integration tests cover “no auto-palette on `/`” and “Enter dispatches slash”.  
6. Phase 7 history still records slash submits.

## Related docs

- [tui-overlays.md](./tui-overlays.md)  
- [tui-shell.md](./tui-shell.md)  
- [tui-commands.md](./tui-commands.md)  
