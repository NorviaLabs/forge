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
  order: [TaskStrip, Search, Files, Workspace, BottomPanel, Sidebar, Approval, Composer, Footer]
  labels:
    TaskStrip: SESSIONS
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
  sessions-tab: Ctrl+1
  files-tab: Ctrl+2
  toggle-navigator-tab: Ctrl+E
  navigator-tab-row: Up (at the first row of either tab's list)
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
11. **Focus has to survive losing colour:** focus markers use shape as well as hue — the `>` title marker, the block caret, the accent scrollbar thumb, the composer's attention-thickened top edge (see the bottom panel's plain/thick rule swap).

## 5. Colour System

Colours are semantic tokens defined per theme (`forge-config::ThemePalette`), not fixed hex values. Every theme supplies the full token set:

| Token | Role |
|---|---|
| `background` | Main canvas |
| `background_deep` | Terminal surround, deepest separators |
| `surface` | Panels, composer, secondary areas |
| `surface_raised` | Elevated content above the canvas |
| `surface_hover` | Hover / pointer affordance ground (weaker than `selection`) |
| `border` | Pane frames, dividers, neutral chrome |
| `border_muted` | Low-priority internal separators |
| `text_primary` | Main readable content |
| `text_secondary` | Supporting copy, metadata |
| `text_muted` | Timestamps, inactive hints, empty-state explanation |
| `accent` | Focus, navigation, caret, active structure |
| `accent_soft` | Low-emphasis accent fills |
| `agent` | Agent narration voice |
| `success` / `warning` / `error` / `info` | Outcome and state semantics |
| `diff_add` / `diff_remove` | Diff line treatments |
| `selection` | Selected text / rows — the strongest neutral ground in the theme |
| `cursor` | Caret and cursor accents |
| `tag` | Dedicated low-emphasis label step (neutral, never saturated) |
| `search_match` | Search match highlights |
| `waiting_border` | Composer border while an approval is pending |
| `structure` | Structural landmarks inside a model response — section labels, list markers |
| `scan_band` | Ground behind a whole list block in a model response |
| `zebra_row` | Even-row tint zebra-striping a rendered table |
| `md_strong` | Editorial emphasis: `**strong**` prose hue (orange in the built-ins) |
| `md_emph` | Editorial emphasis: `*emphasis*` prose hue (greenish yellow in the built-ins) |
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
- Editorial emphasis (`md_strong` / `md_emph`) is the one sanctioned exception to
  hue reservation: it tints prose emphasis so a long answer can be skimmed. It
  never appears in chrome, status glyphs, diffs, code, or the composer, and the
  bold/italic modifier still carries the emphasis without colour.

### 5.3 Status indicators (colour never travels alone)

`crates/forge-tui/src/status_glyph.rs` defines one compact vocabulary used everywhere. Each marker is exactly three cells, with no emoji or Nerd Font dependency, so every state stays legible in monochrome via glyph shape plus an adjacent text label at call sites:

| Indicator | Meaning |
|---|---|
| `[ ]` | Pending / queued |
| `[>]` | Active work (the only orange **state marker**; never focus, selection, or completed success. `md_strong` shares the orange family but is prose emphasis, not state.) |
| `[✓]` | Complete (neutral in history; green only for a confirmed successful result glyph) |
| `[!]` | Failed |
| `[-]` | Cancelled |
| `[?]` | Warning / needs attention |
| `[|]` | Blocked |

Git status is single letters from the same module: `M` `A` `D` `?` `!` `U` (modified / added / deleted / untracked / ignored / conflicted), bold and semantically coloured. The `✓` tick lives inside the `[✓]` completion marker as well as reviewed files and status-bar outcomes; `✗` only for a failed status outcome. Animation is restrained and never changes layout width.

### 5.4 Limited-colour fallback

Every semantic state must include a textual or symbolic cue:

- Lifecycle: the §5.3 bracket markers (`[ ]` `[>]` `[✓]` `[!]` `[-]` `[?]` `[|]`)
- Git: single letters (`M` `A` `D` `?` `!` `U`)
- Success: `[✓]` (green only for a confirmed result) or `✓` for reviewed/status outcomes
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
- Use italics for model-prose emphasis only, and never as the sole signal.
  Terminal italic support varies, so `md_emph` and the wording carry meaning
  when the modifier is dropped.
- Model prose emphasis takes colour: `**strong**` is `md_strong` (orange) at
  bold weight and `*emphasis*` is `md_emph` (greenish yellow) at italic weight,
  so key claims and qualifications pop out when skimming. These hues never
  appear in chrome, status glyphs, diffs, code, or the composer.
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
| Prose strong | `md_strong` + bold — key claims inside an answer |
| Prose emphasis | `md_emph` + italic — qualifications inside an answer |
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
│ Approve-all warning (one row, full width, when on)        │
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

1. **Navigator** — the left column. Two tabs, `Sessions` and `Files`, sharing
   one column (`§7.7`). `Files` is the repository explorer with Git status
   markers and its own search row (`Search` is a separate Tab stop nested in the
   same bordered box). `Sessions` is the multi-session list.
2. **Sidebar** — the persistent conversation column: transcript, outbound-message queue strip, background-task strip, feedback strip, and the composer. It never hides; the composer lives inside it. One rounded frame contains the transcript; a thin scrollbar sits inside its right padding when the transcript overflows. Messages do not get individual frames.
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

Content width is the frame width minus one outer gutter column on each side (`FRAME_INSET_X`).

| Frame width | Behaviour |
|---|---|
| ≥ 116 | Files visible alongside sidebar and workspace (`files_fit()`) |
| < 116 | Files hide entirely; `Ctrl+E` explains instead of toggling |
| any | Sidebar never hides — worst-case floor keeps it at 40 columns of content |
| any | Conversation never falls below 44 columns while the workspace is visible |

Sidebar width: half the content width clamped to 64–88 columns at ≥160; otherwise a quarter clamped to 32–44.

Explorer-first collapse is deliberate: the composer (in the sidebar) outranks the tree.

### 7.4 Height behaviour

- StatusBar consumes three rows for a rounded frame at heights ≥24, otherwise one compact identity row. Footer uses up to two rows (`FOOTER_H`); its background-activity row stays blank when idle.
- Composer input band is capped at 10 visual lines (`MAX_COMPOSER_INPUT_H`), plus top and bottom border rows — it grows within bounds and never crowds out the transcript.
- Theme picker dock is 12 rows (`THEME_DOCK_H`), sized to show built-ins without scrolling.
- A modal leaves surrounding context visible so it reads as overlaying Forge, with the background clearly secondary.
- Every modal title uses the shared `> Title` grammar (`theme::modal_title`) — including the workspace unsaved-changes and file-changed-on-disk conflicts. Borders keep severity colour; the marker says who owns the keyboard.

### 7.5 Cell spacing

Use a compact cell-based scale, biased airy so panes never touch and text
never sits flush against a border:

- `0`: no gap; tightly related glyphs.
- `1`: standard inline gap, outer frame gutter (`FRAME_INSET_X`), interior
  padding (`PANE_PAD_X`), vertical pane gap (`PANE_GAP_Y`), chrome gap
  (`CHROME_GAP_Y`), and transcript ↔ composer gap (`COMPOSER_GAP_Y`).
- `2`: gutter between adjacent columns (`PANE_GAP_X`).

Concretely (`design.rs`): two blank columns separate Files, Workspace and the
Sidebar; one blank row separates chrome from content and transcript from
composer. Border plus `PANE_PAD_X` puts text two cells from the pane edge.
Composer, feedback, queue, and bottom-panel text share this origin
(`TEXT_INSET`). Rounded frames use Ratatui border glyphs and semantic theme
tokens; they do not emulate pixel shadows or change terminal typography.

Avoid double-padding a bordered block and its inner component.

### 7.5.1 Transcript density

The conversation has two densities (`markdown.rs::Density`):

- **Airy** is the default at comfortable pane heights. It adds one blank row
  before a section heading, one after the heading rule, one on each side of a
  fenced code block, one between distinct tool/activity groups (rows inside a
  group stay tight), and one after each `You` / `Answer` speaker label so the
  label reads as a heading rather than a prefix of its text.
- **Compact** is the historical spacing and the fallback for short terminals.
  The app switches to it when the conversation pane is shorter than
  `design::AIRY_MIN_ROWS` (24 rows), so the enforced 80×18 minimum keeps its
  content budget instead of spending rows on padding.

Tight list items remain consecutive in both densities; explicit paragraph
breaks within loose lists are preserved.

Density is part of the streaming cache key: switching densities re-renders the
settled prefix, exactly as a width change does. Both densities keep the "one
blank line between distinct block types" rule from §9.4; airy only widens the
structural rests, it never stacks separators.

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

### 7.7 Navigator — Sessions and Files

The left column is a two-tab **navigator**; it is the single multi-session
surface. The old top task strip is superseded (`§11`).

- Tabs: `Sessions` and `Files`, switched in repository mode with `Ctrl+1` /
  `Ctrl+2`, or flipped with `Ctrl+E` (`input.rs`, `workspace.rs`). Neither
  `Tab` (unconditional block cycling) nor `Enter` (which attaches the selected
  session) switches tabs. They never show side by side: the layout already
  carries three content columns (navigator | Workspace | conversation) and
  cannot afford a fourth, and `Files` is the first thing to collapse (`§7.3`).
- The tab row is reachable from the keyboard: `↑` at the first row of either
  tab's list moves onto the row, where `←` / `→` walk its stops left to right —
  `Sessions`, `Files`, `+` — `Enter` activates the stop the cursor rests on,
  `↓` / `Esc` step back into the pane the active tab shows, and every other key
  is inert on the row. The row is a sub-focus of the column, never a `Tab` stop
  (`§8.3`).
- The row's `+` cell creates a session. It is **not** a third tab: it never
  takes the active tab's ground, and the row's cursor is the only thing that
  marks it, so the tab bar keeps showing which tab is active (`§9.6`). It runs
  the same prompt-less create as `n` in the list and `/new` in the composer, so
  all three stay one verb; the session is named from the first prompt submitted
  in its composer. It is the row's rightmost stop, reachable from either tab
  without switching tabs, and clickable. Its three columns come from the
  `Files` tab — the only elastic box on the row — so on navigators narrow
  enough for the need badge to fit, the badge needs three more columns before
  it reappears beside the cell.
- Default tab: `Sessions` when more than one session exists, `Files` otherwise.
  The choice is remembered for the session.
- **Sessions tab** is a vertical, attention-ordered list. Only three states are
  user-facing: `● needs you`, `◐ working`, `○ idle`. Rows carry the label and a
  short qualifier; branch, worktree and ownership are never shown here.
  Selection (`›`) is the only cursor; `Enter` attaches, `Space` peeks and
  replies inline, `n` creates a session and opens its composer (the session is
  named from the first prompt submitted in that composer), `s` stops, `d` marks
  done, `r` renames. The same create verb is the row's `+` cell and the
  composer's `/new`, so starting a session never requires visiting the list.
- **Files tab** is today's explorer, unchanged.
- Ownership (primary/managed/attached), slots/pinning, and the
  archive/cleanup/remove split are internal — not navigator affordances.
- Below `files_fit()` the whole navigator collapses exactly as `Files` does
  today: the `Sessions` list falls back to a one-line status-row chip
  (`⌄ 2 need · 1 working`) so session attention stays visible, and the session
  switcher (`F3`, `/sessions`) keeps every session reachable.
- The conversation sidebar stays permanent; the Workspace stays
  `File`/`Diff`. The navigator introduces no new column.

## 8. Focus, Modes and Navigation

### 8.1 Blocks and cycle

Nine spatially stable focus blocks (`types.rs::FocusBlock`), cycled by `Tab` / `Shift+Tab` through a fixed order that skips unavailable blocks:

```
TaskStrip → Search → Files → Workspace(CHAT) → BottomPanel(PANEL) → Sidebar → Approval → Composer → Footer
```

- `Approval` enters the cycle only while a HITL request or agent question is pending.
- `Search` is a real Tab stop of its own so Tab has one consistent meaning everywhere instead of toggling a sub-mode inside Files.
- Opening an interactive block focuses it; closing a block restores the previous valid owner, falling back to the Composer (never the Workspace, which is a modal editor).
- A handled event never falls through to another block.
- Model activity does not capture pane navigation: `Tab` / `Shift+Tab` still
  leave the Composer while thinking, answering, or running tools. Plain `Tab`
  completes an active slash suggestion; `Enter` submits or queues a draft.
- `Esc` pops exactly one interaction level.

The canonical label vocabulary is `SESSIONS SEARCH FILES CHAT SIDEBAR COMPOSER FOOTER PANEL APPROVAL` (`types.rs::FocusBlock::label`). Labels identify the active block in help and status contexts; panes themselves carry `>` title rows, not label tags.

### 8.2 Modes

`FocusMode` has exactly two values (`types.rs`):

- **Navigation** — block-level keys apply.
- **Transient(owner)** — a captured input field owns keys: `SourceSearch` or `JumpToLine`.

Text entry in the Composer or editor is expressed by which block is focused, not by a mode overlay. There is no persistent mode chip; where ambiguity could exist, the block title carries a marker (e.g. `> Terminal` with a thick rule).

### 8.3 Navigation grammar

| Action | Binding |
|---|---|
| Next visible block | `Tab` (while the `Panel` block holds the keyboard, plain `Tab` goes to its shell) |
| Previous visible block | `Shift+Tab` |
| Navigator tabs `Sessions` / `Files` (repository mode) | `Ctrl+1` / `Ctrl+2`; `Ctrl+E` flips the two (`Ctrl+1` also focuses the list) |
| Navigator tab row (repository mode) | `↑` at the first row of either tab's list; `←` / `→` walk `Sessions` · `Files` · `+`, `Enter` activates the stop, `↓` / `Esc` step back into the pane |
| Enter interaction | `Enter` or `i` where appropriate |
| Leave one interaction level | `Esc` |
| Go back through workspace history | `Alt+←` |
| Start a draft | any printable key the active block has no binding for (`input.rs::type_to_compose`) |
| Contextual help | `/help` |

Type-to-chat is the rule for every block except `Panel`: an unconsumed
printable key starts a message, because with no local meaning that is the only
reading it has. The block's own bindings are matched first, so a displayed
shortcut never loses to a draft, and focus moves with the character — the block
that renders the keyboard is always the block that owns it. `Panel` is the one
exception: its PTY takes every byte the panel does not claim, so a shell command
typed there can never become a draft the next `Enter` submits to the model.

No block switches tabs on `←` / `→` while its pane holds the keyboard. Plain
arrows keep their in-block meaning: the session cursor in `Sessions` (`←`/`→`
as well as `↑`/`↓`), tree collapse/expand in `Files` and its `Search` row, chip
selection in `Footer`, the composer caret (its text input keeps normal arrow
behaviour), and pass-through to its shell in `Panel`. In the file view a plain
`←` is the same history-back as `Alt+←`. Modified arrows never switch tabs.

The one place `←` / `→` switch tabs is the tab row itself, reached with plain
`↑` at the first row. The row is a sub-focus of the navigator column rather
than a block: `Tab` / `Shift+Tab` still cycle blocks from it (it is never a Tab
stop), only `←` / `→` / `Enter` / `↓` / `Esc` are bound on it, and the pane
under it paints as unfocused while it is up. `←` / `→` move between the row's
three stops and halt at each end rather than wrapping, so the movement always
matches the row as drawn; `Enter` activates the stop under the cursor — a tab
steps back into its pane, the `+` cell creates a session. Resting on `+` moves
nothing else: the active tab keeps its ground and `focus.block()` keeps the
pane on screen, so the row never covers a key owner that is not drawn.

### 8.4 Active block treatment

Three border levels (`design.rs`, `theme::panel_border`):

- **L1 — pane frame.** Every pane, focused or not, takes the same neutral
  `border`. A box that changes hue when it takes the keyboard turns the whole
  layout into a status display, so panes never take accent borders.
- **L2 — inset field.** The composer outline and the explorer's search field
  sit at the same neutral step as L1; a nested field never reads as a second,
  louder box.
- **L3 — local accent.** Only the element that owns the keyboard or the
  selection: the active tab's ground, the navigator tab row's outlines while
  the row itself holds the keyboard, a focused search field's border, the
  composer's top edge, a pane's `>` title marker, the scrollbar thumb. Thick
  rules survive only where the region is a single rule (the bottom panel).

The active block must use at least two signals from the L3 set:

- accent or bold block title
- explicit state marker where relevant (`> Terminal`, `> Chat`)
- caret, scrollbar thumb, or the active tab's ground at the point of interaction

The transcript has one rounded L1 frame that always carries its block title
in the shared pane grammar (`theme::pane_title`, same as the panel and diff
header): `> Chat` in bold accent while the Sidebar block owns the keyboard,
neutral two-space `  Chat` otherwise (never an accent border), including
when the transcript has no overflow. Its
scrollbar also takes a solid accent thumb while the Sidebar block owns the
keyboard, a muted half-block otherwise. Modals suppress background focus.

Do not fill the entire active block with accent colour. Focus is structural, not a selection rectangle.

### 8.5 Selected tab versus focused block

- **Block focus** is shown by local L3 markers and the block title, never by
  the pane outline.
- **Selection** (a row, a list item, a diff entry) is shown inside the block.
- A selection inside an inactive block stays visible but muted, and never implies keyboard ownership.

### 8.6 Mouse

Mouse is a second input for the same grammar, never a separate mode. Clicking moves block focus and acts on the hit target; hover previews without moving focus.

- **Click** focuses the block under the pointer (navigator, task strip, composer, footer, conversation, workspace, panel). A second click at the same cell within the double-click window acts: a navigator session row attaches; a file-tree row opens on the first click. Overlay list rows (model picker providers/models/effort, resume picker, session switcher, theme dock) are pointer-actionable too: a single click moves the highlight (and the picker's focused column), a double-click confirms through the same `Enter` path the keyboard uses.
- **Wheel** scrolls the focused pane's content (conversation, file tree, source viewer), matching the keyboard page/step size. `Shift` pages.
- **Right-click** opens the copy/clear context menu over a text selection.
- **Hover** (when the terminal reports motion) is the pointer's focus ring, and only actionable surfaces take it: session rows, file-tree rows, footer chips, approval options, navigator tabs, queued-message rows, and overlay list rows. It combines a raised `surface_hover` ground with one non-colour signal — a leading `›` marker in the reserved gutter and/or a weight step — so clickability is never colour-only; the marker column is pre-reserved, so hover never shifts text. It never moves keyboard focus and never changes layout. Terminals that do not report motion simply show no hover. Precedence stays focused block > selected row > hover: `selection` is the strongest neutral ground in both built-in themes (`selection` outranks `surface_hover`), so hover never impersonates keyboard ownership or a selection; rows that cannot be acted on never take hover.

## 9. Component Specifications

### 9.1 StatusBar

Purpose: centered repository/branch identity (`widgets/status.rs`). At comfortable
heights a rounded, neutral frame surrounds the row; short terminals use one row.

Includes repository/branch (polled, TTL-cached) and the collapsed navigator's
session-attention chip. Model, effort, lifecycle and context pressure live in
the footer rather than being duplicated here.

Avoid duplicating file counts, task details or provider telemetry already shown elsewhere.

### 9.2 Block frame

- L1 frame: neutral `border` at every focus state (`theme::panel_border()`).
- L3 accents (single-rule regions only): the bottom panel's thick top rule,
  the composer's top edge, a focused search field's border.
- Active title: bold accent with the `>` marker, e.g. `> Terminal`; modals use `theme::modal_title`, panes `theme::pane_title`.
- Modal bodies inset `MODAL_PAD_X` (2) horizontally; a titled modal adds one top
  row. A title never touches the rule it sits on — one space separates the
  label from the fill.
- No double borders except to express a modal or focused panel.

### 9.3 Footer

Two rows (`widgets/footer.rs`); the second row is the background activity line.

- **Row 0 — configuration and turn state.** Configuration chips on the left, live activity on the right.
  - **Chips:** model (`provider/model`, prefix-stripped for display) and reasoning effort. They are an ordinary Tab stop (`Footer` block): `←`/`→` picks a chip, `Enter` opens the picker. `Enter` still sends from the composer.
  - **Lifecycle:** turn state glyph plus short detail qualifier, styled secondary — severity lives in the glyph, never duplicated in colour.
  - **Context pressure:** a word, not a meter — `context` / `context high` / `context full`, coloured ok/warn/error at the 70% and 90% thresholds. (The old nine-cell shade-bar was removed: at typical single-digit percentages it read as stipple texture.)
  - **Hints:** the §6 hint grammar. Blocking dialogs take over the whole row; the footer's own per-chip hint and the task strip's session hint share the row with the chips. Focusing any other block — files, search, the panes — leaves the activity line alone.
  - **Working meter:** one quarter-circle glyph from the same ◐◓◑◒ family the turn line speaks, stepped once per event-loop tick while a turn runs (`throbber-widgets-tui` state, forge styling). Motion pauses with work instead of free-running on the wall clock.
  - When an approval pends, the row dims — it must not look interactive.
- **Row 1 — background activity (design A3, segmented count chips).** One `[glyph label]` chip per group — terminal/background jobs, agents/subagents, queued prompts — each counts-only (`[⟳ jobs 2 · 1 need]`). Glyph and colour carry state (`⟳` running, `●` needs you, `✕` failed, `✓` done, `◆` agent, `⇥` queued); the bracket is shared chrome so the chips read as a segmented strip. The row is blank when nothing is in flight, so an idle footer is unchanged. Per-item detail (command, elapsed, live subagent activity) lives in the background strip (§9.12), not the footer.
  - **A completion is an observation, not a queued prompt.** Finishing a background task does not inject a user-role prompt. The result stays in the background strip and the operator attaches it to the composer explicitly (`i` on the selected task). Only approve-all — no human in the loop — auto-continues by enqueuing the result at the next turn boundary.

### 9.4 Chat transcript (sidebar)

Hierarchy:

1. User request — left-aligned gutter treatment on the neutral `selection` ground (never an accent tint: the answer below must dominate).
2. Final assistant-facing response — tinted background, visually dominant.
3. Approval or failure.
4. Grouped tool activity.
5. Routine progress and metadata.

Rules:

- Raw provider reasoning is hidden by default.
- When reasoning is displayed during streaming, reasoning and answer text use
  independent incremental markdown caches and viewport-tail rendering. Supervisor
  events are processed in bounded batches so input runs between batches.
- Assistant answers should visually dominate routine activity.
- Group repetitive tools under a collapsible activity row.
- Tool calls use concise verbs: `Read 4 files`, `Ran cargo test`.
- Use colour only for result state, not every tool type.
- Preserve exact commands and errors in details.
- The home card is the first screen only: once the operator has sent a turn it retires, never pinned above the conversation for the rest of the session.
- The transcript has one rounded container frame; when its content overflows the pane, a thin
  track (`│`) with a solid thumb (`▐`) marks position in the column's right
  padding, and the thumb turns into the accent `█` while the Sidebar block owns
   the keyboard. No overflow, no track; focus remains visible on the frame.
- While a turn runs, the live turn line (`widgets/turn_line.rs`) names the phase and counts up from the current turn's start — including supervised sessions, where the clock is anchored on the actor's `Running` state, never on process uptime. No placeholder shimmer rows in the transcript — the pane stays empty until content arrives. Gated behind the busy debounce so instant turns never flash it.
- Keep zero-result searches neutral unless they block progress.
- Keep genuine failures visible: a terminal failure renders one error-styled row in the transcript (the durable `[forge.turn_failed]` marker stays hidden — it is model-facing state), so a failed turn never reads as an empty gap.
- Do not render a permanent progress narration stream.
- Distinct top-level block types (paragraph, list, quote, code, table) are separated by exactly one blank line — never zero, never a stack. Each block carries its own trailing blank so the streaming split renderer sees the same separator in a settled prefix as a one-shot render. Under airy density (§7.5.1) the structural rests around headings, fenced code and the `You` / `Answer` speaker labels widen by one blank row; distinct tool/activity groups are separated by one
blank row while rows inside one group stay tight; the rule itself never stacks
separators.
- Lists, quotes, tables and fenced code share the prose left edge; only the code rail sits inside the block, never the whole block inset past its neighbours. A plan's explanation is separated from its `Plan · N of M done` header by one blank.
- Do not surround every message with a full-width box.

Response-structure treatment (editorial): inside an answer, the *skeleton*
is tinted so a long reply can be skimmed by shape before it is read — H1/H2
section labels render uppercased in `structure` over a hairline
`border_muted` rule, list markers take `structure`, and whole list blocks sit
on the `scan_band` ground while rendered tables zebra-stripe body rows with
`zebra_row`. Prose itself stays `text_primary` except for emphasis:
`**strong**` takes `md_strong` (orange) bold and `*emphasis*` takes
`md_emph` (greenish yellow) italic, so the load-bearing words pop out while
the rest of the paragraph stays calm. `accent` never appears in an answer,
and outcome colours stay reserved for result state.

Implementation: `crates/forge-tui/src/conversation.rs`.

Planning checklists use the lifecycle grammar: `[ ]` pending, `[>]` active
(orange, bold — the only orange element), `[✓]` completed in neutral muted.
The active task has bold text; other tasks are muted. Wrapped text aligns
after the checkbox. The heading reports completed tasks, and the pinned
summary retains the count and current task when the checklist scrolls away.
Completion reflects the agent's reported plan status; tool evidence remains
below each step. Nesting stays flat: deeper levels need a transcript schema
change, so the renderer locks one level rather than inventing hierarchy. Only
the newest checklist renders: a superseded revision is removed, never recorded
as a second `Plan updated · N of M done` line beside it, so the checklist is
the single plan surface.

### 9.5 Composer

- `surface` background and a full rounded outline. Side and bottom borders stay
  neutral; the top edge takes `accent` when focused and `waiting_border` while
  an approval pends ("paused" look). Only attention states thicken the top
  rule — focus alone is a hue change, with the block caret as the monochrome
  signal. Waiting outranks focus colour.
- Multi-line growth bounded by `MAX_COMPOSER_INPUT_H`.
- Outbound messages queue below the input as a strip; `Ctrl+↑`/`Ctrl+↓` move the selection, `Ctrl+Backspace` cancels one.

### 9.6 File tree

- Search is an inset, three-row rounded field (`/ ` prefix plus query) at the
  neutral L2 step. One blank resting row separates it from the first tree row,
  and the whole tree sits one indent step (`LIST_INSET_X`) inside the field
  above it. Search focus colours the border and prefix and shows the caret;
  clicking the field focuses Search.
- Navigator tabs have rounded outlines sharing the list's top edge, neutral
  whether selected or not. The selected tab fills its inner row edge to edge
  with `accent_soft` ground — a full-width signal that stays inside the frame,
  never flowing over or under the text — and carries its label at accent hue
  and bold weight; no tab is underlined and no tab carries a marker glyph, so the
  label stays centred in its tab in every state. The tab bar shows which tab
  is active, not which block owns the keyboard — with one exception: while the
  row itself holds the keyboard (`↑` at the first row of either tab's list,
  `§8.3`), both outlines step to the L3 accent so the row reads as the thing
  being driven. The active tab keeps its ground and hue in that state, so the
  focus signal never stands in for the active-tab signal.
- The row's `+` cell is the row's third segment, sharing its frame edges with
  the tab boxes and taking the same focus step on its outline while the row
  holds the keyboard. It is not a tab and never carries a tab's treatment: no
  `accent_soft` ground in any state, so the active-tab signal stays the tab
  bar's alone. Its glyph is muted by default, takes the hover ground plus a
  weight step under the pointer, and steps to accent and bold only while the
  row's cursor rests on it. Below the width the navigator column is laid out at
  the cell is dropped rather than drawn cramped, and it is then skipped as a
  cursor stop, so the frame and the keyboard can never disagree.
- Selected row uses the neutral `selection` token plus a `>` pointer in a dedicated gutter column; the inactive selection loses the background entirely but keeps bold text and the pointer.
- Active file and selected row may differ; distinguish them.
- Git markers come from the shared glyph set (§5.3): `M` `A` `D` `?` `!` `U`, bold and semantically coloured.
- Directory expansion uses ASCII `>` / `v` with 2-cell indentation; the query match inside a name takes the shared `search_match` highlight (contiguous runs only — fuzzy-only matches stay plain).
- A filtered-to-nothing query reports `No matches for "<query>"`; an empty repository reports `This directory is empty`. The two states are never the same line.
- Do not clear the visible tree during a Git-only refresh (pinned by test — FORGE-DESIGN invariant).
- Empty, loading, unavailable and failed states must be distinct.

### 9.7 Source viewer

- Code remains the visual focus; syntax highlighting is restrained (`syntax.*` palette).
- One blank row separates the pane title from the content, so the header reads as chrome rather than the first source line.
- The title shows the exact file with an ASCII `*` unsaved marker that is never elided; there is no trailing "modified" word.
- The header row follows the shared pane-title grammar (`theme::pane_title`): `>` marker in brand bold when the Workspace block owns the keyboard, neutral otherwise — the same treatment as the diff header and the transcript frame.
- The NORMAL / INSERT mode row shares the composer's text inset so both baselines align.
- Search matches rank: active match, other matches, current line.
- Markdown (`.md` / `.markdown`) opens in a read-only rendered `PREVIEW` mode; `:preview` toggles between the rendered document and the editable source, which keeps any unsaved buffer. Other files are unaffected.
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
- One blank row separates the pane title from the patch (same rule as the source viewer).
- Prefer foreground/gutter markers over large background fills per changed line.
- The header names the selected file as the pane title (`> …`), with ASCII `+N -M` counts and the `N of M` position; the marker column comes off the elision budget so counts never clip.
- Reviewed files carry the `✓` tick; counts stay ASCII even in narrow panes.
- Stale diff state must be explicit; binary/untracked/conflicted states must be truthful.
- `/diff` holds no content state itself — the pane reads live diff state so refreshes update in place.

### 9.9 Terminal (BottomPanel)

- One interactive login shell per session; closing the panel never kills it.
- Focused presentation: thick top rule + `> Terminal` + accent title — legible without colour (shape carries it too). The title keeps one cell before the rule.
- The body shares the shared text origin (`TEXT_INSET`), like the composer and the feedback strip.
- Busy phase, activity feed lines, shell label and a painted caret render inside the panel.
- Standard control keys, arrows, Tab, paste and resize are forwarded to the shell.

### 9.10 Transient toast overlay

- Success and error notices additionally surface as a positioned toast (`ratatui-toaster`, `widgets/toasts.rs`), bottom-right, auto-expiring after 2s. Notification only: never focusable, never blocking.
- The feedback strip keeps its persistent latest-status role; the toast is the interruption, the strip is the record.

### 9.11 Approve-all warning strip

- While a session's approve-all mode is on, one full-width row renders directly under the StatusBar: `⚠ SANDBOX OFF · approvals, filesystem and network unconfined · this session only · /approve-all to re-enable`.
- Error-coloured (`theme::danger()`), one row, full frame width, and part of the fixed chrome — the conversation scroll cannot move it off screen.
- It is the persistent record that the sandbox is off; it disappears the moment approve-all is disabled.

### 9.12 Background activity strip

What the footer's counts-only chip deliberately omits: one row per background
task, docked between the outbound-message queue and the composer and scoped to
the sidebar's width (`layout.rs::regions.background`, built by
`tasks_strip.rs`, drawn by `widgets/background_strip.rs`).

Every rule here exists to protect something the operator is relying on.

- **Height is derived, never requested.** The renderer asks for the lines the
  strip actually needs — header, one per row, plus one more for each row with a
  second line — and `layout.rs` clamps that against the transcript's `Min`
  floor. A job that is merely running can never take a row from the
  conversation, which is why this is the only fixed-height strip the caller
  specifies rather than the layout deriving. Rows are one *or* two lines, so a
  count of rows is not a height: a budget that assumed one line each drew the
  next row straight over the line underneath.
- **The header tells the truth about truncation:** ` Background · 8 ` with
  `+N more` right-aligned when the row cap (8) or the available space hides
  some. The count is what survived expiry, not what was spawned.
- **Ordering is blocked → failed → active → queued → done**, stable by task id
  inside a band so a running row never jumps when a sibling finishes. `failed`
  outranks `active` because it is the only other state that can need an
  operator, and truncation takes from the tail — naive ordering hides the
  failure first. `cancelled` is its own state rather than folded into `done`,
  so a cancellation the operator did not perform (a stopped session, an
  interrupted turn) stays visible instead of ageing out.
- **Row anatomy:** `> [|] ◆ explore · audit auth deps  needs you` — the
  navigator's selection grammar, the §5.3 marker, the kind glyph the footer
  chip also uses, then the label and a right-aligned elapsed. A row grows a
  **second line** in exactly two cases, and the styling tells them apart:
  - **A blocked row** shows the pending request (tool and its already-redacted
    arguments) in the warning hue. That is the one case where a label is not
    enough to act on, because the operator has to answer it.
  - **An active subagent** shows what it is doing, in secondary text: the
    assistant text as it streams, or `running bash…` while a tool is out. The
    request outranks the activity if both exist, which they cannot.
  - Reported activity is not a problem, so it is never warning-coloured; only
    the blocked line is.
- **Expiry:** a `[✓]` row retires one minute after `finished_at`; `[!]`, `[-]`
  and `[|]` wait for `x`. The strip is a status, not a log — but a completion
  has to still be there when the operator looks back, which is what the timer
  is measured against.
- **Elapsed freezes at `finished_at`**, so a finished row shows how long it took
  rather than creeping upward while it is read.
- **Selection** is the `>` pointer plus the neutral `selection` ground, muted to
  secondary text when the Sidebar block does not own the keyboard (§8.5). The
  keys (`↑↓ x a d i`) predate this surface; before it the operator selected and
  acted on rows that nothing drew.
- **An empty registry draws nothing at all** — no header, no reserved gap — so
  an idle sidebar is unchanged, the same contract Row 1 of the footer keeps.

### 9.13 Child session view

What the strip's two-line activity summary deliberately omits: the subagent's
full transcript. `→` on a background task row replaces the transcript pane with
that child's own session, read-only; `←`/`Esc` returns. Read-only by
construction — the view is a journal replay (`forge_core::session_messages` →
`forge_session::replayed_transcript`), never a second runtime, so there is no
second writer against a session the child still owns.

Every rule here exists to keep the operator oriented about whose session is on
screen.

- **The frame title says whose.** `Chat ‹ explore` names the child; the
  parent's own `Chat` comes back with it on `←`. `‹` reads as "drilled into",
  not a path — there is no parent-task lineage to draw, and inventing one
  would be a lie. The composer hint carries the same notice
  (`read-only · viewing explore · ← to return`), and the composer refuses input
  while the view is open.
- **The view refreshes while the child is live.** The poll tick re-reads the
  child's journal while its task is non-terminal; a finished child keeps its
  final snapshot. The refresh is silent, swaps only when the messages actually
  changed (else every tick would invalidate the render cache for nothing), and
  never touches scroll or follow — the operator may be reading an older page.
- **The header still describes the parent session** (workspace, branch): the
  child runs in its own worktree, so those do not match while the view is
  open. The frame title names the child that *is* on screen.
- **A replayed child carries no live activity rows.** `TurnEvent`s are not
  replayed, so the view shows the conversation without the streaming
  second-lines the owning session carries — the right trade for a view that
  cannot act. Shell tasks have no session of their own and are refused with a
  reason rather than opening an empty view.

### 9.14 Out-of-band notification
Reaching an operator who is looking at another window. `notify.rs`; configured
by `[tui] notify = auto | bell | osc9 | both | off`.

- **Fires on two transitions only** — a task entering `WaitingForApproval`, and
  a task becoming terminal — detected by diffing the task set each tick. Never
  on steady state: a notification per frame is not a notification.
- **Gated on lost focus.** Forge requests focus events (`EnableFocusChange`) for
  this purpose alone. Default focus is *focused*, so a terminal that never
  reports focus changes never notifies.
- **`auto` is silent, not the bell, on an unrecognised terminal**, matching the
  terminal against a known-good allowlist. Terminal support for OSC 9 cannot be
  probed, and an unexpected audible bell is worse than no notification; `bell`
  is the opt-in universal fallback.
- **The body carries the same source label as the strip** —
  `explore needs approval: bash` — so the notification and the strip agree.
- **Text is sanitized, not trusted.** Labels come from the model, and an
  embedded `ESC` or `BEL` would terminate the OSC 9 sequence early and leave
  the remainder to be read as terminal commands.
- The in-app toast and feedback strip still fire while focused. The terminal
  notification and the in-app notice answer different questions.

### 9.15 Quit-all confirm
Quitting Forge has always stopped every session — process exit retires every
actor, so the sessions the operator is not looking at die with the one they
are. What was missing was the bill: neither `Ctrl+D` nor `/quit` said how many
turns, queued prompts, pending approvals, or unsaved buffers went with it.

- **Raised by the quit gate**, one path shared by `Ctrl+D`, `/quit` on the
  primary session, and the exit that follows a resolved unsaved-changes
  dialog. `/quit` in any other session view stays a per-session close, so the
  dialog exists in exactly one place.
- **Only when a second session has work in flight.** The condition is "more
  than one Active session with a `Running` or `Queued` turn". Quitting one
  busy session that the operator is watching is the ordinary case; prompting
  for it would put a dialog in front of every quit and the dialog would stop
  meaning anything.
- **Two rows, `Cancel` selected first.** The dialog exists to protect work, so
  an `Enter` the operator did not mean must not destroy any. `↑↓` moves,
  `Enter` confirms the highlighted row, `Esc` cancels; the binding line comes
  from `hints::QUIT_ALL`, which is the same set the dialog documents.
- **The body names what is lost, and only what is lost.** A headline
  (`Quitting stops every session.`), then one line per non-empty category:
  sessions with a turn running, queued prompts never dispatched, requests
  waiting on the operator, and unsaved changes by session label. Categories at
  zero are absent rather than shown as `0`, so the dialog stays short when
  quitting is cheap.
- **Border severity follows the loss.** `warn` while every buffer is saved,
  `danger` once any session view holds unsaved changes. Counts come from the
  supervisor's roster, never from the focused view, so the numbers describe
  every session rather than the visible one.
- **Progress is the status line, not a second dialog.** While the sweep runs
  the message counts down against the roster (`closing 3 sessions…`). The same
  flag suppresses the per-session "removed with unsaved editor changes"
  warnings, which the confirm has already accounted for and which would
  otherwise fire once per session on the way out.
- **Exit is not negotiable, and failure is not silent.** The process leaves
  whether or not every session retired. Failures travel out in the exit
  summary and print after the terminal is restored
  (`quit all sessions: 2 of 5 sessions did not close`), because a toast on the
  final frame is a report nobody reads. The supervisor's sweep attempts the
  whole roster; one session that will not retire never strands the rest.
- **The double-`Ctrl+C` escalation stays ungated.** The operator has already
  seen `Ctrl+C again to quit` and insisted; interposing a dialog there would
  fight the one binding whose whole purpose is to stop asking.

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
- `md_strong` shares the activity orange family (36°), and `md_emph` is a
  yellow-green at 70° — clear of warning (45°) and success (141°). Both are
  prose-only and fall back to `text_primary` in themes that omit them.

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
- There is no horizontal task strip above the workspace, and no separate modal session switcher with its own keymap and five-group taxonomy. Multi-session lives in the navigator (`§7.7`): one vertical list, three states, one keymap.
- Pinning/slots are not a user-facing affordance, and archive/cleanup/remove are not three separate verbs.

## 12. Session Worktrees

Managed (new) sessions run in their own worktree per session, created from the
initiating worktree's committed `HEAD`.

- A managed session **starts detached**: no branch is created at creation.
- On the **first filesystem change** relative to `HEAD`, Forge creates
  `forge/<label-slug>` in that worktree and switches to it. The trigger is
  Git's view of the working tree (staged, unstaged and untracked, gitignore-
  aware) — not which tool ran — so shell redirects, formatters and MCP writes
  are caught equally.
- A read-only / research session therefore adds **no ref**. The branch is named
  from the session label (disambiguated with the short session id on collision),
  never a random UUID. A session created prompt-less, before its composer has
  been used, is unnamed only until the first prompt is submitted; the branch
  materializes on that first turn's first filesystem change, by which point the
  session has its real name — so the temporary unnamed state never reaches Git.
- While branchless, the session's identity is its worktree path plus the base
  commit; startup reconciliation keeps it active. Cleanup verifies the worktree
  is still the session's — by branch once branched, or still-detached before.
- The primary session and attached worktrees are unchanged.
