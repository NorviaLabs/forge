---
version: 2.1
status: reconciled-with-code
name: Forge TUI Design System
product: Forge
platform: terminal-ui
framework: Ratatui
summary: >-
  A terminal-native design system for Forge, an open human-agent development
  workspace. It combines calm workspace hierarchy, developer-first monospace
  clarity, restrained semantic status language, and per-theme accent identity
  governed by one hard invariant: focus colour and outcome colour never share
  a hue.
inspiration:
  warp:
    role: workspace hierarchy, warm dark surfaces, hairline depth, restraint
    weight: 40
  opencode:
    role: terminal-native typography, semantic states, compact technical UI
    weight: 30
  ollama:
    role: simplicity, whitespace discipline, code-first presentation
    weight: 15
  forge:
    role: focus clarity, human control, intervention, accent/status separation
    weight: 15
principles:
  - operational clarity over decoration
  - exactly one visible keyboard owner
  - human judgement remains prominent
  - dense but calm
  - safe and read-only by default
  - colour reinforces meaning but never carries it alone
  - progressive disclosure instead of permanent noise
  - the terminal font belongs to the user, not Forge
layout-blocks:
  - Files (explorer, hides when narrow)
  - Sidebar (persistent conversation column with the composer)
  - Workspace (center pane — File or Diff)
  - BottomPanel (interactive terminal)
  - StatusBar / Footer (chrome rows)
focus-blocks:
  order: [TaskStrip, Search, Files, Workspace, Sidebar, Approval, Composer, Footer, BottomPanel]
  labels:
    TaskStrip: TASKS
    Search: SEARCH
    Files: FILES
    Workspace: CHAT
    Sidebar: SIDEBAR
    Approval: APPROVAL
    Composer: COMPOSER
    Footer: FOOTER
    BottomPanel: PANEL
navigation:
  next-block: Tab
  previous-block: Shift+Tab
  previous-tab: Left
  next-tab: Right
  go-back: Alt+Left
  enter-interaction:
    - Enter
    - i
  leave-interaction: Esc
themes:
  builtin: [forge-dark, forge-light]
  special: [system]
  user-drop-in-dirs: ["~/.config/forge/themes", ".forge/themes"]
minimum-terminal: 80x18
---

# Forge TUI Design System

## 1. Purpose

Forge is an open, terminal-native workspace for delegating development work to agents while keeping the developer in control. The interface must support three activities without making any one of them feel secondary:

1. **Delegate** work to an agent.
2. **Inspect** files, source, diffs, tests and activity.
3. **Intervene** manually when judgement or correction is required.

The design should feel like a serious development instrument, not a chatbot placed inside a terminal and not a dashboard squeezed into character cells.

This document describes Forge's TUI presentation language as implemented in `crates/forge-tui`. Where this document and the code disagree, the code wins — file an issue or fix this doc in the same PR.

## 1.1 Contract Authority

The implemented interaction contract is authoritative for runtime behavior and screen structure. This document defines presentation rules only.

Superseded structural rules that must NOT be reintroduced:

- A permanent Chat/Editor/Diff tab bar as the primary workspace model. Conversation is *not* a center-pane view; it lives permanently in the sidebar (`crates/forge-tui/src/app/types.rs`, `WorkspaceView`).
- A permanent right-hand Inspector with Task/Context/Runtime tabs. No such block exists.
- Bottom-panel tabs (Run / Diagnostics / Terminal / Activity). The bottom panel is the interactive terminal; busy-phase and activity lines render inside it.
- A permanent shortcut footer or manual on every screen. The footer shows contextual hints plus two configuration chips.
- Duplicate status ownership across header, footer, sidebar and workspace.
- Any rule implying Forge controls the terminal font family, size, line height, ligatures, or other font-rendering properties.

## 2. Design Character

Forge should feel:

- **Terminal-native:** every surface respects character-cell constraints.
- **Operational:** status, focus and consequences are immediately legible.
- **Calm:** dark surfaces, restrained colour and minimal ornament.
- **Dense:** useful information is visible without excessive blank space.
- **Human-controlled:** approvals, intervention and review are visually stronger than background automation.
- **Open-source:** straightforward, inspectable and free of glossy enterprise theatre.

Forge should not feel:

- futuristic for its own sake
- like a web dashboard recreated in Ratatui
- like a direct clone of Warp, OpenCode, Ollama or another coding agent
- permanently busy
- dependent on colour alone
- modal without making the current mode visible

## 3. Source Synthesis

### Borrow from Warp

- Warm near-charcoal surfaces instead of pure black.
- Hairline borders and surface contrast instead of shadows.
- Clear block-based workspace hierarchy.
- Quiet confidence: restrained emphasis rather than constant visual shouting.
- Technical content as the main visual material.

### Borrow from OpenCode

- Monospace-first presentation.
- Compact, developer-oriented information density.
- Explicit semantic colours for success, warning, failure and information.
- Textual and ASCII-friendly indicators rather than decorative iconography.
- Keybinding hints as a first-class part of the interface.

### Borrow from Ollama

- Minimal visual vocabulary.
- Code and command output treated as primary content.
- Limited use of highlighted surfaces.
- Simple, truthful empty states.
- Restraint: do not invent a new visual treatment when an existing one works.

### Keep distinctly Forge

- The accent identifies focus, interaction and navigable structure — and it must stay hue-separated from every outcome colour (see §5.3).
- Yellow/amber identifies waiting, caution and human attention (`waiting_border` pauses the composer while an approval is pending).
- Violet (`agent`) marks agent narration as a distinct voice from the user's.
- The developer's judgement is visually prioritised over agent narration.
- Active block, selected row and input ownership are separate concepts.
- The interface centres the loop: delegate, inspect, intervene, validate.

## 4. Core UX Invariants

These are not optional styling preferences. They are correctness requirements.

1. **Exactly one effective keyboard owner exists at a time.**
2. **The visually active block matches the actual event owner** (`focus.rs::normalize_focus`).
3. **Selected content and focused content are visually distinct.**
4. **Input, transient and blocked states are distinguishable** without colour alone.
5. **A displayed shortcut always invokes a reachable command in the current context** (hints degrade by dropping verbs, then pairs — never by advertising dead keys).
6. **Hidden or unavailable blocks cannot retain focus**; Tab cycles only available blocks.
7. **Colour never provides the only indication of state.**
8. **Approvals and failures outrank routine activity.**
9. **Raw model reasoning is not ordinary chat content.**
10. **The primary workflow remains usable at the enforced minimum of 80 × 18** (`layout.rs::MIN_WIDTH` / `MIN_HEIGHT`). Below that Forge refuses to render rather than drawing a broken screen.
11. **Focus has to survive losing colour:** active panels use thick borders or title markers in addition to accent styling (see the bottom panel's plain/thick rule swap).

## 5. Colour System

Colours are semantic tokens defined per theme (`forge-config::ThemePalette`), not fixed hex values. Every theme supplies the full token set:

| Token | Role |
|---|---|
| `background` | Main canvas |
| `background_deep` | Terminal surround, deepest separators |
| `surface` | Panels, composer, secondary areas |
| `surface_raised` | Elevated content above the canvas |
| `surface_hover` | Hover / subtle highlight treatment |
| `border` | Standard dividers and inactive block borders |
| `border_muted` | Low-priority internal separators |
| `text_primary` | Main readable content |
| `text_secondary` | Supporting copy, metadata |
| `text_muted` | Timestamps, inactive hints, empty-state explanation |
| `accent` | Focus, navigation, caret, active structure |
| `accent_soft` | Low-emphasis accent fills |
| `agent` | Agent narration voice |
| `success` / `warning` / `error` / `info` | Outcome and state semantics |
| `diff_add` / `diff_remove` | Diff line treatments |
| `selection` | Selected text / rows |
| `cursor` | Caret and cursor accents |
| `tag` | Dedicated low-emphasis label step (neutral, never saturated) |
| `search_match` | Search match highlights |
| `waiting_border` | Composer border while an approval is pending |
| `structure` | Structural landmarks inside a model response — section labels, list markers |
| `scan_band` | Ground behind a whole list block in a model response |
| `zebra_row` | Even-row tint zebra-striping a rendered table |
| `syntax.*` | Code highlighting palette |

Do not use shadows. Ratatui depth comes from border weight, contrast and placement.

Do not render large bodies of important text using dim styling; terminal dim support varies and may harm readability.

### 5.1 The accent/status invariant

The single hardest rule in this system (`ACCENT_STATUS_MIN_HUE_DISTANCE`, asserted over built-ins in tests):

> **The accent must sit at least 60° of hue away from `success`, `warning` and `error`.**

The accent answers *"where am I and what will my next keystroke touch"*; the outcome colours answer *"what happened"*. A theme that renders both in the same hue cannot say both at once — the focused border starts reading as status. `info` and `agent` are deliberately excluded from the check: neither reports an outcome, so both belong near the accent's own arc.

Forge Dark's comment on its own palette is the model for how to reason about new themes: *"the brand green lives in the ground, not in the signal"* — every neutral is green-tinted while the accent is deliberately not green.

### 5.2 Semantic roles

| Role | Token | Meaning |
|---|---|---|
| Accent | `accent` | Focus, navigation, active structure, links |
| Warning | `warning` | Waiting for user, caution, approval needed |
| Success | `success` | Verified success, passing validation, clean completion |
| Error | `error` | Failure, destructive consequence, blocked state |
| Info | `info` | Neutral information, diff hunks, background progress |
| Agent | `agent` | Agent-attributed narration and tool activity |

Rules:

- The accent is the interaction colour, not a decorative fill.
- Yellow/amber is reserved for states that need human attention.
- Green appears only for evidence-backed success.
- Red is reserved for credible error or destructive consequence.
- Blue/info is lower priority than the accent and should not compete with focus.
- Never use semantic colour on every row in a busy transcript.

### 5.3 Status indicators (colour never travels alone)

`crates/forge-tui/src/status_glyph.rs` defines one compact ASCII vocabulary used everywhere. Each marker is exactly three cells, with no emoji or Nerd Font dependency, so every state stays legible in monochrome via glyph shape plus an adjacent text label at call sites:

| Indicator | Meaning |
|---|---|
| `[ ]` | Pending / queued |
| `[>]` | Active work (the only orange element; never focus, selection, or completed success) |
| `[x]` | Complete (neutral in history; green only for a confirmed successful result glyph) |
| `[!]` | Failed |
| `[-]` | Cancelled |
| `[?]` | Warning / needs attention |
| `[|]` | Blocked |

Git status is single letters from the same module: `M` `A` `D` `?` `!` `U` (modified / added / deleted / untracked / ignored / conflicted), bold and semantically coloured. The `✓` tick survives only where it marks a reviewed file and a status-bar outcome; `✗` only for a failed status outcome. Animation is restrained and never changes layout width.

### 5.4 Limited-colour fallback

Every semantic state must include a textual or symbolic cue:

- Lifecycle: the §5.3 bracket markers (`[ ]` `[>]` `[x]` `[!]` `[-]` `[?]` `[|]`)
- Git: single letters (`M` `A` `D` `?` `!` `U`)
- Success: `[x]` (green only for a confirmed result) or `✓` for reviewed/status outcomes
- Failure: `[!]` or `✗` for a failed status outcome
- Focus: stronger/thicker border plus the `>` title marker
- Selection: neutral background plus the `>` pointer, never tint alone

Themes map onto ANSI fallbacks for terminals without true colour.

## 6. Typography and Text Treatment

Forge inherits the user's terminal font. Never bundle or require a font.

### Rules

- Use monospace throughout.
- Forge may only use terminal attributes: bold, dim, underline, foreground and background.
- Use bold sparingly: active labels, headings, consequences, status glyphs.
- Use underline for links or explicit selected actions only.
- Use dim only for genuinely secondary metadata and always test legibility.
- Avoid italics; support is inconsistent across terminals.
- Use uppercase for compact structural labels only — the focus-block titles are exactly `SEARCH`, `FILES`, `CHAT`, `SIDEBAR`, `COMPOSER`, `FOOTER`, `PANEL`, `APPROVAL` (`types.rs::FocusBlock::label`).
- Use sentence case for messages, explanations and actions.
- Avoid decorative ASCII art inside the product chrome.
- Chrome glyphs are ASCII first (`>`, `v`, `[ ]`, `*`, `+`/`-`): tree markers, state markers and counts never depend on Unicode coverage. Arrows survive only inside key hints (`↑↓←→`, `⇧`, `⏎`), `·` joins hint pairs, and the block caret keeps its cell — all carrying meaning that is also spelled out in adjacent words.

Hierarchy comes from weight, token step and placement — never from size, since terminal font size belongs to the user.

| Level | Treatment |
|---|---|
| Brand / application title | `theme::brand()` — bold primary |
| Active block title | Bold + accent with the `>` marker (`> Terminal`) |
| Inactive block title | Normal + muted, two-space indented to hold alignment |
| Primary content | `text_primary` — assistant response, source code |
| Supporting content | `text_secondary` — metadata, descriptions |
| Utility content | `text_muted` — keys, timestamps, counts |

### Hint grammar

All key hints use one grammar (`hints.rs`): `key verb` pairs joined by ` · `, keys bold, verbs sentence case. Under width pressure hints degrade by first dropping verbs (bare keys), then dropping trailing pairs. Hints never wrap — a hint that reflows breaks its container's height budget.

```text
Enter confirm · Esc cancel
↑↓ select · Enter confirm · Esc skip
```

## 7. Layout Model

Implemented in `crates/forge-tui/src/layout.rs`. Regions (`LayoutRegions`):

```
┌──────────────────────────────────────────────────────────┐
│ StatusBar (one row)                                       │
├────────┬───────────────────────┬─────────────────────────┤
│        │                       │ feedback strip (0–1)    │
│        │                       │ conversation            │
│ Files  │     Workspace         │ queue strip             │
│ (opt.) │  (File / Diff /       │ background strip        │
│        │   empty placeholder)  │ composer                │
├────────┴───────────────────────┴─────────────────────────┤
│ BottomPanel (interactive terminal, 0-height when closed)  │
├──────────────────────────────────────────────────────────┤
│ Footer (chips + contextual hints)                         │
└──────────────────────────────────────────────────────────┘
```

### 7.1 Blocks

1. **Files** — repository explorer with Git status markers and its own search row (`Search` is a separate Tab stop nested in the same bordered box).
2. **Sidebar** — the persistent conversation column: transcript, outbound-message queue strip, background-task strip, feedback strip, and the composer. It never hides; the composer lives inside it.
3. **Workspace** — the center pane. Its only views are `File` and `Diff` (`types.rs::WorkspaceView`); with nothing open it renders an empty-state placeholder. Conversation is deliberately *not* a workspace view.
4. **BottomPanel** — the interactive terminal. One top-rule border, thick + `> Terminal` title when focused. Closing it does not kill the shell; reopening resumes the same session. Busy phase and activity feed lines render inside the panel.
5. **StatusBar / Footer** — chrome rows described in §9.

### 7.2 Spatial priority

1. Modal or approval overlay (HITL card in the transcript is itself a Tab stop).
2. Transient input such as source search or jump-to-line.
3. Sidebar conversation and composer.
4. Workspace content.
5. Files.
6. BottomPanel.
7. Decorative or redundant metadata.

### 7.3 Width behaviour in terminal columns

Content width is 95% of frame width (`CONTENT_WIDTH_PERCENT`).

| Frame width | Behaviour |
|---|---|
| ≥ 116 | Files visible alongside sidebar and workspace (`files_fit()`) |
| < 116 | Files hide entirely; `Ctrl+E` explains instead of toggling |
| any | Sidebar never hides — worst-case floor keeps it at 40 columns of content |
| any | Conversation never falls below 44 columns while the workspace is visible |

Sidebar width: half the content width clamped to 64–88 columns at ≥160; otherwise a quarter clamped to 32–44.

Explorer-first collapse is deliberate: the composer (in the sidebar) outranks the tree.

### 7.4 Height behaviour

- StatusBar and Footer consume one row each.
- Composer input band is capped at 10 visual lines (`MAX_COMPOSER_INPUT_H`) — it grows within bounds and never crowds out the transcript.
- Theme picker dock is 12 rows (`THEME_DOCK_H`), sized to show built-ins without scrolling.
- A modal leaves surrounding context visible so it reads as overlaying Forge, with the background clearly secondary.
- Every modal title uses the shared `> Title` grammar (`theme::modal_title`) — including the workspace unsaved-changes and file-changed-on-disk conflicts. Borders keep severity colour; the marker says who owns the keyboard.

### 7.5 Cell spacing

Use a compact cell-based scale:

- `0`: no gap; tightly related glyphs.
- `1`: standard inline gap or one-cell padding.
- `2`: block interior padding where width allows.

Avoid double-padding a bordered block and its inner component.

### 7.6 Responsive Presentation

- Preserve the sidebar (conversation + composer) first.
- Preserve critical status or current action.
- Collapse Files before anything else.
- Remove secondary metadata before removing primary content.
- Truncate paths visually without mutating stored values.

Verify layouts at least at these sizes (tests pin `80×18`):

- `80×18` (enforced minimum)
- `120×40`
- `160×50`

## 8. Focus, Modes and Navigation

### 8.1 Blocks and cycle

Nine spatially stable focus blocks (`types.rs::FocusBlock`), cycled by `Tab` / `Shift+Tab` through a fixed order that skips unavailable blocks:

```
TaskStrip → Search → Files → Workspace(CHAT) → Sidebar → Approval → Composer → Footer → BottomPanel
```

- `Approval` enters the cycle only while a HITL request or agent question is pending.
- `Search` is a real Tab stop of its own so Tab has one consistent meaning everywhere instead of toggling a sub-mode inside Files.
- Opening an interactive block focuses it; closing a block restores the previous valid owner, falling back to the Composer (never the Workspace, which is a modal editor).
- A handled event never falls through to another block.
- `Esc` pops exactly one interaction level.

The canonical label vocabulary is `TASKS SEARCH FILES CHAT SIDEBAR COMPOSER FOOTER PANEL APPROVAL` (`types.rs::FocusBlock::label`). Labels identify the active block in help and status contexts; panes themselves carry `>` title rows, not label tags.

### 8.2 Modes

`FocusMode` has exactly two values (`types.rs`):

- **Navigation** — block-level keys apply.
- **Transient(owner)** — a captured input field owns keys: `SourceSearch` or `JumpToLine`.

Text entry in the Composer or editor is expressed by which block is focused, not by a mode overlay. There is no persistent mode chip; where ambiguity could exist, the block title carries a marker (e.g. `> Terminal` with a thick rule).

### 8.3 Navigation grammar

| Action | Binding |
|---|---|
| Next visible block | `Tab` |
| Previous visible block | `Shift+Tab` |
| Previous/next tab within a block | `←` / `→` (plain, no modifiers — chords are explicitly rejected) |
| Enter interaction | `Enter` or `i` where appropriate |
| Leave one interaction level | `Esc` |
| Go back through workspace history | `Alt+←` |
| Contextual help | `/help` |

Modified arrows do not switch tabs; text inputs retain normal arrow behaviour.

### 8.4 Active block treatment

The active block must use at least two signals:

- accent-coloured or stronger/thicker border
- accent or bold block title
- explicit state marker where relevant (`> Terminal`)

Inactive blocks use a muted hairline border and normal title weight.

Do not fill the entire active block with accent colour. Focus is structural, not a selection rectangle.

### 8.5 Selected tab versus focused block

- **Block focus** is shown by the block border and title.
- **Selection** (a row, a list item, a diff entry) is shown inside the block.
- A selection inside an inactive block stays visible but muted, and never implies keyboard ownership.

## 9. Component Specifications

### 9.1 StatusBar

Purpose: identity and global session state in one row (`widgets/status.rs`).

Includes: brand, repository/branch (polled, TTL-cached), turn lifecycle with glyph, busy phase, model/provider/effort, context pressure.

Avoid duplicating file counts, task details or provider telemetry already shown elsewhere.

### 9.2 Block frame

- Inactive border: `border_muted` / `border`.
- Active border: `accent` (or thick border type where the region is a single rule).
- Active title: bold accent with the `>` marker, e.g. `> Terminal`; modals use `theme::modal_title`, panes `theme::pane_title`.
- No double borders except to express a modal or focused panel.

### 9.3 Footer

One row (`widgets/footer.rs`): configuration chips on the left, live activity on the right.

- **Chips:** model (`provider/model`, prefix-stripped for display) and reasoning effort. They are an ordinary Tab stop (`Footer` block): `←`/`→` picks a chip, `Enter` opens the picker. `Enter` still sends from the composer.
- **Lifecycle:** turn state glyph plus short detail qualifier, styled secondary — severity lives in the glyph, never duplicated in colour.
- **Context pressure:** a word, not a meter — `context` / `context high` / `context full`, coloured ok/warn/error at the 70% and 90% thresholds. (The old nine-cell shade-bar was removed: at typical single-digit percentages it read as stipple texture.)
- **Hints:** the §6 hint grammar. Blocking dialogs take over the whole row; footer-focus hints share the row with the chips.
- **Working meter:** one quarter-circle glyph from the same ◐◓◑◒ family the turn line speaks, stepped once per event-loop tick while a turn runs (`throbber-widgets-tui` state, forge styling). Motion pauses with work instead of free-running on the wall clock.
- When an approval pends, the row dims — it must not look interactive.

### 9.4 Chat transcript (sidebar)

Hierarchy:

1. User request — left-aligned gutter treatment, distinct background.
2. Final assistant-facing response — tinted background, visually dominant.
3. Approval or failure.
4. Grouped tool activity.
5. Routine progress and metadata.

Rules:

- Raw provider reasoning is hidden by default.
- Assistant answers should visually dominate routine activity.
- Group repetitive tools under a collapsible activity row.
- Tool calls use concise verbs: `Read 4 files`, `Ran cargo test`.
- Use colour only for result state, not every tool type.
- Preserve exact commands and errors in details.
- While a turn runs, the live turn line (`widgets/turn_line.rs`) names the phase and counts up. No placeholder shimmer rows in the transcript — the pane stays empty until content arrives. Gated behind the busy debounce so instant turns never flash it.
- Keep zero-result searches neutral unless they block progress.
- Keep genuine failures visible.
- Do not render a permanent progress narration stream.
- Do not surround every message with a full-width box.

Response-structure treatment (editorial): inside an answer, the *skeleton*
is tinted so a long reply can be skimmed by shape before it is read — H1/H2
section labels render uppercased in `structure` over a hairline
`border_muted` rule, list markers take `structure`, and whole list blocks sit
on the `scan_band` ground while rendered tables zebra-stripe body rows with
`zebra_row`. Prose itself stays `text_primary`; `accent` never appears in an
answer, and outcome colours stay reserved for result state.

Implementation: `crates/forge-tui/src/conversation.rs`.

Planning checklists use the lifecycle grammar: `[ ]` pending, `[>]` active
(orange, bold — the only orange element), `[x]` completed in neutral muted.
The active task has bold text; other tasks are muted. Wrapped text aligns
after the checkbox. The heading reports completed tasks, and the pinned
summary retains the count and current task when the checklist scrolls away.
Completion reflects the agent's reported plan status; tool evidence remains
below each step. Nesting stays flat: deeper levels need a transcript schema
change, so the renderer locks one level rather than inventing hierarchy.

### 9.5 Composer

- `surface` background; hairline border normally; `accent` border when focused; `waiting_border` while an approval pends ("paused" look — the composer visibly cannot accept a send).
- Multi-line growth bounded by `MAX_COMPOSER_INPUT_H`.
- Outbound messages queue below the input as a strip; `Ctrl+↑`/`Ctrl+↓` move the selection, `Ctrl+Backspace` cancels one.

### 9.6 File tree

- Search is one surface row (`/ ` prefix plus query); the tree begins immediately below. No box, no separator.
- Selected row uses the neutral `selection` token plus a `>` pointer in a dedicated gutter column; the inactive selection loses the background entirely but keeps bold text and the pointer.
- Active file and selected row may differ; distinguish them.
- Git markers come from the shared glyph set (§5.3): `M` `A` `D` `?` `!` `U`, bold and semantically coloured.
- Directory expansion uses ASCII `>` / `v` with 2-cell indentation; the query match inside a name takes the shared `search_match` highlight (contiguous runs only — fuzzy-only matches stay plain).
- A filtered-to-nothing query reports `No matches for "<query>"`; an empty repository reports `This directory is empty`. The two states are never the same line.
- Do not clear the visible tree during a Git-only refresh (pinned by test — FORGE-DESIGN invariant).
- Empty, loading, unavailable and failed states must be distinct.

### 9.7 Source viewer

- Code remains the visual focus; syntax highlighting is restrained (`syntax.*` palette).
- The title shows the exact file with an ASCII `*` unsaved marker that is never elided; there is no trailing "modified" word.
- The NORMAL / INSERT mode row shares the composer's text inset so both baselines align.
- Search matches rank: active match, other matches, current line.
- Line numbers muted; active line number accented.
- Horizontal scrolling must not detach markers from content.
- Binary and invalid-UTF-8 files are explicitly read-only.

### 9.8 Diff viewer

Conventional semantics with textual fallbacks:

- addition: `diff_add` + `+`
- removal: `diff_remove` + `-`
- context: body/muted
- hunk header: info + `@@`
- file header: primary text

Rules:

- Preserve old and new line numbers.
- Prefer foreground/gutter markers over large background fills per changed line.
- The header names the selected file as the pane title (`> …`), with ASCII `+N -M` counts and the `N of M` position; the marker column comes off the elision budget so counts never clip.
- Reviewed files carry the `✓` tick; counts stay ASCII even in narrow panes.
- Stale diff state must be explicit; binary/untracked/conflicted states must be truthful.
- `/diff` holds no content state itself — the pane reads live diff state so refreshes update in place.

### 9.9 Terminal (BottomPanel)

- One interactive login shell per session; closing the panel never kills it.
- Focused presentation: thick top rule + `> Terminal` + accent title — legible without colour (shape carries it too).
- Busy phase, activity feed lines, shell label and a painted caret render inside the panel.
- Standard control keys, arrows, Tab, paste and resize are forwarded to the shell.

### 9.10 Transient toast overlay

- Success and error notices additionally surface as a positioned toast (`ratatui-toaster`, `widgets/toasts.rs`), bottom-right, auto-expiring after 2s. Notification only: never focusable, never blocking.
- The feedback strip keeps its persistent latest-status role; the toast is the interruption, the strip is the record.

## 10. Theme Policy

Built-in themes ship as TOML in `crates/forge-tui/themes/` and compile into the binary:

| id | Name |
|---|---|
| `forge-dark` | Forge Dark (default) |
| `forge-light` | Forge Light |

Plus the pseudo-theme `system`, which follows the terminal's light/dark preference and re-resolves on OS appearance changes.

Users drop custom `.toml` themes into `~/.config/forge/themes/` or `.forge/themes/`; unparseable drop-ins are skipped (with diagnostics where the caller can show them) rather than breaking startup.

Rules:

- Themes are semantic token mappings against the full `ThemePalette`, not arbitrary plugin formats.
- Palette invariants (including the §5.1 accent/status hue distance) are asserted over the built-in set in tests.
- Bare `/theme` opens a bottom dock: `↑↓` live-previews against the real UI, `Enter` confirms, `Esc` restores the previous theme. `/theme <id>` applies immediately.
- Theme choice must not change runtime semantics, navigation, persistence or command availability.
- Theme policy applies to conversation presentation, chrome, activity and code rendering — never to terminal font selection.

### Forge Dark identity notes

Forge Dark is the reference implementation of the system's philosophy:

- All neutrals are green-tinted — the brand lives in the ground, not in signals.
- Accent is periwinkle blue (`#8FA4D6`), placed at 222° precisely because it is the widest arc clear of success (119°), warning (39°), error (6°) and agent violet (274°).
- Agent narration gets its own violet voice (`agent`), distinct from both the user's text and every outcome colour.
- `tag` is deliberately unsaturated: low-emphasis labels must not read as a hue with meaning.

New themes should document their hue arithmetic the same way in their TOML comments.

## 11. Explicitly Superseded Structural Rules

These older assumptions are wrong for the shipped architecture and must not be reintroduced:

- Chat is not a center-pane view or a mode tab — it is the permanent sidebar.
- There is no Inspector block and no Task/Context/Runtime tabs.
- The bottom panel has no Run/Diagnostics/Activity tabs — it is the terminal.
- The workspace does not offer a Run view; runs happen in the terminal panel.
- Files visibility is not owned by a workspace tab; it collapses on width alone.
- The shell is not organized around a permanent shortcut manual.
- The transcript does not need a box for every message.
- The application must not imply that terminal typography can be configured from inside Forge.
