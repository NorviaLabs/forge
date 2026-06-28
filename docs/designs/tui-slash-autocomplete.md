# TUI Tab autocomplete & highlight cursor design

**Status:** Draft  
**Owner:** Mohit Ranka  
**Last updated:** 23 Jul 2026  
**Phase:** **8 only** (exclusive)  
**Revision:** **8.1**  
**PRD:** TUI-07  
**Architecture:** §14 Phase 8.1, decision #22  
**Related:** [tui-slash-inline.md](./tui-slash-inline.md) (Phase 8 base), [tui-input-history.md](./tui-input-history.md) (Phase 7), [tui-overlays.md](./tui-overlays.md), [../ui.md](../ui.md)

---

## 1. Problem / context

Phase 8 lets operators type `/commands` in the main textbox. Without **Tab autocomplete** and a clear **highlight** of the active suggestion (or history entry), discovery is slower and the caret is hard to see—especially when cycling history with Up/Down.

Phase **8.1** makes autocomplete and highlight **normative** product behavior (not stretch).

## 2. Goals & non-goals

**Goals**

1. **Tab-based autocomplete** for top-level slash commands while the main textbox content starts with `/`.  
2. **Visible highlight** of:
   - the **selected suggestion** in the suggestions panel, and  
   - the **input caret** (and, when browsing history, the recalled line as active input).  
3. **↑/↓** move the suggestion highlight when slash suggestions are visible; otherwise ↑/↓ remain history (Phase 7).  
4. Testable without a real TTY (unit + TestBackend visual frames).

**Non-goals**

- Fuzzy multi-word NLP completion.  
- Completing model chat messages (non-`/` text).  
- Replacing Ctrl+K full palette.  
- Mouse click selection (keyboard-first).

## 3. Design

### 3.1 Suggestions panel (inline, not full modal)

When **no blocking overlay** is open and `input.text` starts with `/`:

```text
┌ suggestions · Tab complete · ↑↓ ─────────────────────────┐
│ ▶ /status      Session status     ← highlighted (selected)│
│   /tools       List tools                                │
│   /stop…                                                 │
└──────────────────────────────────────────────────────────┘
┌ input ───────────────────────────────────────────────────┐
│ ❯ /sta█                      ← caret (reversed / block)  │
└──────────────────────────────────────────────────────────┘
```

| Rule | Behavior |
|------|----------|
| Filter | Substring match on command name + description (case-insensitive), using text after leading `/` |
| Order | Catalog order (stable); prefer prefix matches first if implemented |
| Max rows | ~6 visible; scroll selection within filtered list |
| Empty filter `/` | Show top catalog commands |
| No matches | Hide panel or show “no matches” |

### 3.2 Tab autocomplete (normative)

| Key | Behavior |
|-----|----------|
| **Tab** | Replace input with **selected** suggestion’s `cmd` (append trailing space if command takes args optional; default: `cmd` + space) |
| Tab with empty selection / no panel | No-op |
| Tab when input already equals selected cmd | No-op or cycle next match (implementation may cycle) |
| Shift-Tab | Optional: previous suggestion (nice-to-have) |

Enter still **runs** the current line (Phase 8). If the line is a **strict prefix** of the selected suggestion and has no spaces, Enter may first complete then require second Enter for arg-taking commands (optional; document in code). Preferred: Tab completes; Enter always dispatches current buffer.

### 3.3 Highlight semantics

#### A. Suggestion list highlight

- Selected row uses **brand/teal bold** (or reverse video) vs muted non-selected rows.  
- Leading marker `▶` or reverse full-row is acceptable.  
- Selection index clamped when filter shrinks.

#### B. Input caret (always when focused)

- Caret at `input.cursor` rendered as **reversed character** or block `█` at EOL.  
- Hint text (empty buffer) is dim; no fake caret over hint required.

#### C. History browse highlight

When Phase 7 history is active (`browse_index = Some`):

- Input bar shows recalled text with **normal caret at end**.  
- Optional status hint: `history 2/12` (not required for exit).  
- Suggestions panel **hidden** while browsing history unless input still starts with `/` after recall (if recalled line is `/status`, suggestions may reappear—OK).

### 3.4 Key priority (no overlay)

```text
if input starts with '/' AND suggestions non-empty:
    Up/Down → move suggestion highlight
    Tab     → complete selected cmd into textbox
else:
    Up/Down → InputHistory (Phase 7)
Tab when not in slash mode → no-op (or indent never)
```

With **overlay** open (palette, connect, HITL): existing overlay keys win; autocomplete panel hidden.

### 3.5 Ctrl+K palette vs inline panel

| Mechanism | Role |
|-----------|------|
| Inline suggestions | Lightweight complete-as-you-type in textbox |
| Ctrl+K palette | Full modal discovery list (Phase 4/8) |

Both use the same command catalog source (`default_palette_items` / help catalog).

### 3.6 Visual acceptance (ui.md)

See [../ui.md](../ui.md) screen **13 — Slash autocomplete** for layout reference. Implementation need not be pixel-identical but must show:

1. Suggestions panel with one highlighted row  
2. Input with visible caret  
3. Optional history note when Up recalls a prior slash line  

## 4. Interfaces

```rust
// TuiApp
slash_suggest_idx: usize,
fn slash_suggestions(&self) -> Vec<PaletteItem>;
fn complete_slash_suggestion(&mut self); // Tab
fn clamp_slash_suggest(&mut self);

// InputModel
fn set_text(&mut self, text: impl Into<String>); // caret → end
// render: reverse-video caret at cursor
```

## 5. Failure modes

| Case | Behavior |
|------|----------|
| Filter becomes empty | Hide panel; reset index |
| Tab with secrets in buffer | Complete still only inserts catalog cmd, never env secrets |
| Busy agent | Ignore Tab/Up/Down on input (unchanged busy policy) |

## 6. Phase ownership

| Item | Phase |
|------|-------|
| This document / TUI-07 | **8** (rev **8.1**) |
| Inline `/` entry + Enter | Phase 8 [tui-slash-inline.md](./tui-slash-inline.md) |
| History Up/Down baseline | Phase 7 [tui-input-history.md](./tui-input-history.md) |

## 7. Acceptance

1. Type `/sta` → panel shows `/status` highlighted → **Tab** fills `/status` (or `/status `).  
2. **↑/↓** change which suggestion is highlighted (visual).  
3. Input caret visible at edit position.  
4. History Up on non-slash text still recalls prior lines with caret at end.  
5. Overlay open: no inline suggestion panel; overlay owns arrows.  
6. Automated tests: Tab complete, highlight index, visual TestBackend frame contains selected cmd.  

## Related docs

- [tui-slash-inline.md](./tui-slash-inline.md)  
- [tui-input-history.md](./tui-input-history.md)  
- [../ui.md](../ui.md)  
