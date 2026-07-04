# TUI input command history design

**Status:** Draft  
**Owner:** Mohit Ranka  
**Last updated:** 23 Jul 2026  
**Phase:** **7 only** (exclusive)  
**PRD:** TUI-05  
**Architecture:** Phase 7, decision #20  
**Related:** [tui-shell.md](./tui-shell.md) (input bar region), [tui-overlays.md](./tui-overlays.md) (history inactive under overlays)

---

## 1. Problem / context

Phase 4 TUI input supports typing, cursor left/right, and submit (Enter). Operators re-type long prompts and slash commands. Standard terminal UX expects **Up/Down arrow keys** to walk **submitted command history**—same idea as bash/`readline` and the line-mode REPL.

Phase 7 adds in-session (and optionally persisted) history navigation without changing the agent loop.

## 2. Goals & non-goals

**Goals**

- **Up** / **Down** navigate previous / next entries while the main input bar is focused (no modal overlay).  
- History records **submitted** lines (user messages and slash commands), not every keystroke.  
- Editing a recalled line and submitting stores a **new** history entry (or updates per §3.4).  
- Cursor Up/Down must **not** steal keys from overlays (slash palette, model picker, HITL, connect).  
- Unit-testable history model without a TTY.

**Non-goals**

- Full readline (Ctrl-R reverse search, multi-line emacs bindings)—optional later.  
- Chat transcript scroll with Up/Down (conversation scroll remains PageUp/PageDown or dedicated keys from TUI-02).  
- Sharing history across machines / cloud sync.  
- History of secrets (redact or refuse to store lines that look like API keys).

## 3. Design

### 3.1 Data model

```rust
struct InputHistory {
    /// Oldest → newest submitted lines (after trim; empty not stored).
    entries: Vec<String>,
    /// Cap (default 500 in-memory; configurable later).
    max_entries: usize,
    /// None = browsing “live” draft; Some(i) = viewing entries[i].
    browse_index: Option<usize>,
    /// Draft text when user first pressed Up from live input.
    stash: Option<String>,
}
```

| Operation | Behavior |
|-----------|----------|
| `push(line)` | Append non-empty trimmed line; drop oldest if over cap; reset browse to live |
| `up()` | Move to previous entry (or first Up: stash draft, show newest) |
| `down()` | Move toward newer; past newest restores stash and live mode |
| `current()` | Text to place in input bar |

### 3.2 Key binding (main input focus only)

| Key | Action |
|-----|--------|
| **↑ Up** | Older history entry → replace input text; cursor to end |
| **↓ Down** | Newer history entry / restore draft |
| Left / Right | Unchanged (cursor within line) |
| Enter | Submit: `push` line (if non-empty), then existing dispatch |
| Esc | Clear input (existing); also cancel history browse (restore live empty or stash policy: clear wins) |

When any **overlay** is open: Up/Down keep existing overlay selection behavior (Phase 4); history is inactive.

**Phase 8.1:** When the textbox starts with `/` and slash **suggestions** are visible, **↑/↓** move the **suggestion highlight** instead of history—see [tui-slash-autocomplete.md](./tui-slash-autocomplete.md). History Up/Down apply when not in slash-suggest mode. Recalled history text must show a **visible caret** at end of line.

### 3.3 Interaction with slash `/` and connect modals

- Typing `/` opening the palette: history inactive while palette open.  
- Connect API-key overlay: history inactive; character input goes to masked field.  
- After overlay closes, history state remains as before open.

### 3.4 Push policy

| Case | Store? |
|------|--------|
| Non-empty user message | Yes |
| Non-empty slash command | Yes |
| Empty Enter | No |
| Duplicate consecutive same line | Optional dedupe (recommended: skip exact consecutive dup) |
| Line matches secret patterns (`sk-`, `api_key=`, long tokens) | **Do not store**; still submit |

### 3.5 Persistence (optional in Phase 7)

| Mode | Behavior |
|------|----------|
| **Default** | Session-only memory (lost on process exit) |
| **Opt-in** | Append-only `~/.config/forge/input_history` (0600), load last N on TUI start |

Persistence is a stretch goal if timeboxed; **session history is mandatory** for exit.

### 3.6 Conversation scroll vs history

| Keys | Region |
|------|--------|
| Up/Down (no overlay) | **Input history** (this design) |
| PageUp / PageDown | Conversation scroll (TUI-02) if implemented; else no-op |

Do not use Up/Down for chat scroll while input is focused.

### 3.7 Line-mode REPL (optional mirror)

Phase 7 **primary** surface is `forge tui`. REPL may reuse `InputHistory` later; not required for exit.

## 4. Interfaces

```rust
impl InputHistory {
    pub fn new(max_entries: usize) -> Self;
    pub fn push(&mut self, line: &str);
    pub fn up(&mut self, draft: &str) -> Option<String>;
    pub fn down(&mut self) -> Option<String>; // None + clear browse = use stash
    pub fn reset_browse(&mut self);
}

// TuiApp fields
input: InputModel,
history: InputHistory,
```

On Up/Down: set `input.text` and `input.cursor = text.len()`.

## 5. Failure modes

| Case | Behavior |
|------|----------|
| Empty history + Up | No-op (or beep/status “no history”) |
| Mid-edit after Up then type | Stay on that browse index; next Up/Down from edited buffer without re-push until Enter |
| Max entries exceeded | Drop oldest |
| Corrupt history file (if persisted) | Ignore file; start empty; log warn |

## 6. Phase ownership

| Item | Phase |
|------|-------|
| This entire document / TUI-05 | **7** |
| Input bar chrome | Phase 4 [tui-shell.md](./tui-shell.md) |
| Overlay key ownership | Phase 4 [tui-overlays.md](./tui-overlays.md) |

## 7. Acceptance

1. Submit three distinct lines; Up cycles older→newest correctly; Down returns to draft.  
2. Up/Down inside slash palette still move palette selection (no history).  
3. Empty and secret-like lines not stored.  
4. Unit tests for `InputHistory` without TTY.  
5. Manual: `forge tui` arrow history works for prompts and `/status`-style commands.

## Related docs

- [tui-shell.md](./tui-shell.md)  
- [tui-overlays.md](./tui-overlays.md)  
- [tui-conversation.md](./tui-conversation.md)  
