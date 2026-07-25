# TUI polish pass design

**Status:** Draft
**Owner:** Forge
**Last updated:** 2026-08-01
**Phase:** TUI polish
**Related:** [../ui.md](../ui.md), [tui-shell.md](./tui-shell.md), [tui-session-chrome.md](./tui-session-chrome.md), [tui-status-feedback.md](./tui-status-feedback.md), [tui-conversation.md](./tui-conversation.md), [tui-sidebar.md](./tui-sidebar.md)

---

## 1. Problem

Forge is already functional, but the TUI reads as utility-first rather than polished. The desired feel is closer to a modern terminal workspace: compact, calm, high-contrast at the right points, and easier to scan.

## 2. Goals

- Make the interface feel premium without adding clutter.
- Improve hierarchy between chrome, transcript, input, and side panels.
- Keep terminal readability strong at narrow widths.
- Preserve current information density and workflows.

## 3. Visual direction

- Dark surfaces with restrained borders.
- Bright accent only for focus, selection, and active state.
- Card-like message blocks with subtle role-specific backgrounds.
- Stronger top chrome and composer framing.
- Softer separators and fewer competing labels.

## 4. Component rules

### 4.1 Status chrome

- Present provider, model, effort, context, and connection as compact chips.
- Show busy state with a clear active indicator.
- Keep `/status` aligned with the chrome fields.

### 4.2 Transcript

- Use padded message cards.
- Distinguish user, assistant, and tool output with background and label changes, not heavy ornamentation.
- Render tool calls as tidy blocks with less visual noise.

### 4.3 Composer

- Make the input feel focused and intentional.
- Increase contrast around the caret and active line.
- Keep shortcuts visible but secondary.

### 4.4 Sidebar and pickers

- Treat side panels like command surfaces.
- Selection should feel deliberate and bright.
- Keep empty states and metadata compact.

### 4.5 Diagnostics

- Render `/status` and related reports like a compact control panel.
- Use badges for cache, context, tools, and connection state.

## 5. Theme tokens

Prefer a small semantic layer over ad hoc colors:

- `surface`
- `surface_alt`
- `surface_active`
- `accent_soft`
- `accent_strong`
- `text_muted`
- `text_dim`

## 6. Implementation targets

- `crates/forge-tui/src/theme.rs`
- `crates/forge-tui/src/widgets/status.rs`
- `crates/forge-tui/src/widgets/footer.rs`
- `crates/forge-tui/src/widgets/input.rs`
- transcript rendering widgets
- sidebar and picker widgets

## 7. Deliverables

- Updated TUI styling.
- Updated `/status` presentation.
- Design screenshots in `docs/ui/screens/`.

## 8. Review checklist

- Still readable at 80 columns.
- Still usable at 60 columns.
- No loss of core status information.
- No extra visual noise in tool-heavy turns.
