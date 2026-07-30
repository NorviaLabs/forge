---
version: 1.0
status: draft
name: Forge TUI Design System
product: Forge
platform: terminal-ui
framework: Ratatui
summary: >-
  A terminal-native design system for Forge, an open human-agent development
  workspace. It combines Warp's calm dark workspace hierarchy, OpenCode's
  developer-first monospace clarity and semantic status language, Ollama's
  restraint, and Forge's own cyan-and-yellow operational identity.
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
    role: focus clarity, human control, intervention, cyan/yellow identity
    weight: 15
principles:
  - operational clarity over decoration
  - exactly one visible keyboard owner
  - human judgement remains prominent
  - dense but calm
  - safe and read-only by default
  - colour reinforces meaning but never carries it alone
  - progressive disclosure instead of permanent noise
colors:
  canvas: "#181716"
  canvas-deep: "#111110"
  surface: "#23211f"
  surface-elevated: "#2c2926"
  surface-selected: "#2a3a3c"
  hairline: "#45413c"
  hairline-muted: "#35322f"
  text: "#f2efe8"
  text-strong: "#ffffff"
  text-body: "#c9c3b9"
  text-muted: "#918b82"
  accent: "#53d6e3"
  accent-strong: "#84e7ef"
  accent-muted: "#2d5b60"
  warning: "#e5b94f"
  warning-muted: "#594a27"
  success: "#61cf8b"
  success-muted: "#294b37"
  danger: "#ef7078"
  danger-muted: "#5b2c31"
  info: "#7da8f5"
  info-muted: "#2d3f61"
  diff-added: "#61cf8b"
  diff-removed: "#ef7078"
  diff-context: "#a7a198"
  diff-hunk: "#7da8f5"
ansi-fallbacks:
  canvas: default-background
  surface: bright-black
  text: white
  text-muted: bright-black
  accent: cyan
  warning: yellow
  success: green
  danger: red
  info: blue
spacing:
  none: 0
  xs: 1
  sm: 2
  md: 3
  lg: 4
layout:
  blocks:
    - Files
    - Workspace
    - Inspector
    - BottomPanel
  workspace-views:
    - Conversation
    - File
    - Diff
    - Run
  inspector-tabs:
    - Task
    - Context
    - Runtime
  bottom-tabs:
    - Run
    - Diagnostics
    - Terminal
    - Activity
navigation:
  next-block: Tab
  previous-block: Shift+Tab
  next-tab: Shift+Right
  previous-tab: Shift+Left
  enter-interaction:
    - Enter
    - i
  leave-interaction: Esc
---

# Forge TUI Design System

## 1. Purpose

Forge is an open, terminal-native workspace for delegating development work to agents while keeping the developer in control. The interface must support three activities without making any one of them feel secondary:

1. **Delegate** work to an agent.
2. **Inspect** files, source, diffs, tests and activity.
3. **Intervene** manually when judgement or correction is required.

The design should feel like a serious development instrument, not a chatbot placed inside a terminal and not a dashboard squeezed into character cells.

This document is authoritative for Forge's TUI visual language. The reference DESIGN.md files are inspiration, not specifications. Do not copy their product identity, logos, wording, layouts or exact colour systems.

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

- Cyan identifies focus, interaction and navigable structure.
- Yellow identifies waiting, caution and human attention.
- The developer's judgement is visually prioritised over agent narration.
- Active block, selected tab and input mode are separate concepts.
- The interface centres the loop: delegate, inspect, intervene, validate.

## 4. Core UX Invariants

These are not optional styling preferences. They are correctness requirements.

1. **Exactly one effective keyboard owner exists at a time.**
2. **The visually active block matches the actual event owner.**
3. **Selected content and focused content are visually distinct.**
4. **Input, navigation, transient and modal modes are distinguishable.**
5. **A displayed shortcut always invokes a reachable command in the current context.**
6. **Hidden or unavailable blocks cannot retain focus.**
7. **Colour never provides the only indication of state.**
8. **Approvals and failures outrank routine activity.**
9. **Raw model reasoning is not ordinary chat content.**
10. **The primary workflow remains usable at 80 × 24.**

## 5. Colour System

### 5.1 Surface ladder

| Role | Token | Use |
|---|---|---|
| Deep background | `canvas-deep` | Terminal surround, strong modal dimming, deepest separators |
| Main canvas | `canvas` | Default application background |
| Block surface | `surface` | Panels, composer, secondary areas |
| Elevated surface | `surface-elevated` | Modal, focused input, selected activity details |
| Selected surface | `surface-selected` | Current selectable row or subtle active-line treatment |
| Border | `hairline` | Standard dividers and inactive block borders |
| Muted border | `hairline-muted` | Low-priority internal separators |

Do not use shadows. Ratatui depth comes from border weight, contrast and placement.

### 5.2 Text ladder

| Role | Token | Use |
|---|---|---|
| Highest emphasis | `text-strong` | Critical action, active title, approval consequence |
| Primary | `text` | Main readable content |
| Secondary | `text-body` | Supporting copy, metadata, routine details |
| Muted | `text-muted` | Timestamps, inactive hints, empty-state explanation |

Do not render large bodies of important text using dim styling; terminal dim support varies and may harm readability.

### 5.3 Accent and semantic roles

| Role | Colour | Meaning |
|---|---|---|
| Cyan | `accent` | Focus, navigation, active structure, links |
| Yellow | `warning` | Waiting for user, caution, approval needed |
| Green | `success` | Verified success, passing validation, clean completion |
| Red | `danger` | Failure, destructive consequence, blocked state |
| Blue | `info` | Neutral information, diff hunks, background progress |

Rules:

- Cyan is the Forge interaction colour, not a decorative fill.
- Yellow is reserved for states that need human attention.
- Green appears only for evidence-backed success.
- Red is reserved for credible error or destructive consequence.
- Blue is lower priority than cyan and should not compete with focus.
- Never use semantic colour on every row in a busy transcript.

### 5.4 Limited-colour fallback

Every semantic state must include a textual or symbolic cue:

- Success: `✓ Passed`
- Warning: `! Waiting for you`
- Failure: `× Failed`
- Information: `i Running`
- Focus: stronger border plus title/mode label
- Selection: pointer or inverse text, not colour alone

Support terminals without true colour by mapping to ANSI cyan, yellow, green, red, blue, white and bright black.

## 6. Typography and Text Treatment

Forge inherits the user's terminal font. Never bundle or require a font.

### Rules

- Use monospace throughout.
- Use bold sparingly for active labels, headings and consequences.
- Use underline for links or explicit selected actions only.
- Use dim only for genuinely secondary metadata and always test legibility.
- Avoid italics; support is inconsistent across terminals.
- Use uppercase for compact structural labels only, such as `FILES`, `EDITOR`, `NAV` and `INPUT`.
- Use sentence case for messages, explanations and actions.
- Avoid decorative ASCII art inside the product chrome.

### Hierarchy in character cells

| Level | Treatment | Examples |
|---|---|---|
| Application title | Bold + primary text | `FORGE` |
| Active block title | Bold + accent | `EDITOR · NAV` |
| Inactive block title | Normal + muted | `FILES` |
| Active supporting tab | Bold + primary or accent underline | `Activity` |
| Inactive supporting tab | Muted | `Diagnostics` |
| Primary content | Primary text | Assistant response, source code |
| Supporting content | Body text | Metadata, descriptions |
| Utility content | Muted | Keys, timestamps, token counts |

Do not use size as a hierarchy mechanism; terminal font size is controlled by the user.

## 7. Layout Model

### 7.1 Four top-level blocks

1. **Files** — repository navigation and Git markers.
2. **Workspace** — the active contextual view: Conversation, File, Diff or Run.
3. **Inspector** — Task, Context and Runtime.
4. **Bottom panel** — Run, Diagnostics, Terminal and Activity.

The centre Workspace is always primary. Auxiliary blocks support it and should collapse before the Workspace becomes unusably narrow.

### 7.2 Spatial priority

1. Modal or approval overlay.
2. Transient input such as search or jump-to-line.
3. Active Workspace content.
4. Files or Inspector.
5. Bottom panel.
6. Decorative or redundant metadata.

### 7.3 Width behaviour in terminal columns

| Width | Behaviour |
|---|---|
| `>= 120` | Files and Inspector may both be visible; full contextual footer |
| `100–119` | Keep one side panel narrower; collapse verbose Inspector fields |
| `80–99` | Show at most one auxiliary side panel at a time; compact footer |
| `< 80` | Workspace-first emergency layout; auxiliary panels become toggled views |

These are design targets, not hard-coded requirements if the existing layout uses nearby breakpoints.

### 7.4 Height behaviour

- The header and footer should consume no more than one row each in normal operation.
- Bottom panel default height should be approximately 25–35% of available rows.
- A modal must leave enough surrounding context to show that it overlays Forge, but the background should be clearly secondary.
- Chat composer should grow only within a bounded range.

### 7.5 Cell spacing

Use a compact cell-based scale:

- `0`: no gap; tightly related glyphs.
- `1`: standard inline gap or one-cell padding.
- `2`: block interior padding where width allows.
- `3`: strong separation, rarely used.
- `4`: exceptional wide-layout breathing room.

Avoid double-padding a bordered block and its inner component.

## 8. Focus, Modes and Navigation

### 8.1 Active block treatment

The active block must use at least two signals:

- accent-coloured or stronger border
- accent or bold block title
- explicit mode label where relevant

Example:

```text
┌─ EDITOR · NAV ─────────────────────────────────┐
```

Inactive blocks use a muted hairline border and normal title weight.

Do not fill the entire active block with cyan. Focus is structural, not a selection rectangle.

### 8.2 Selected tab versus focused block

- **Block focus** is shown by the block border and title.
- **Selected tab** is shown inside the tab strip.
- A selected tab inside an inactive block remains visible but muted.
- The selected tab must not imply keyboard ownership when its block is inactive.

### 8.3 Modes

Use concise labels only when ambiguity exists:

- `NAV` — block-level navigation owns keys.
- `INPUT` — text input owns keys.
- `SEARCH` — search field owns keys.
- `JUMP` — jump-to-line owns keys.
- `WAITING` — blocked on human action.

Modal titles carry their own mode and do not need `MODAL` appended.

### 8.4 Common navigation grammar

| Action | Binding |
|---|---|
| Next visible block | `Tab` |
| Previous visible block | `Shift+Tab` |
| Previous tab in active block | `Shift+Left` |
| Next tab in active block | `Shift+Right` |
| Enter interaction | `Enter` or `i` where appropriate |
| Leave one interaction level | `Esc` |
| Contextual help | `?` |

Rules:

- Modified arrows switch tabs only in navigation mode.
- Text inputs retain normal arrow behaviour.
- `Esc` pops exactly one interaction level.
- A handled event never falls through to another block.
- Opening an interactive block focuses it.
- Closing a block restores the previous valid focus owner.

## 9. Component Specifications

### 9.1 Application header

Purpose: identity and global state, not a second Inspector.

Include only high-value global information:

- Forge name/logo
- repository and branch when reliable
- current execution state
- model only when space permits

Avoid duplicating file counts, task details, validation and provider telemetry already shown elsewhere.

Wide example:

```text
 FORGE   forge/main   ● Running                           GPT-5.x
```

Narrow example:

```text
 FORGE   forge/main   ● Running
```

### 9.2 Block frame

- Inactive border: `hairline` or `hairline-muted`.
- Active border: `accent`.
- Active title: bold `accent` or bold `text-strong` with accent delimiter.
- No rounded-corner illusion beyond standard box-drawing glyphs.
- Avoid double borders unless expressing a modal.

### 9.3 Tab strip

- Tabs sit on one row where possible.
- Active tab uses bold text plus underline, reverse, or small accent marker.
- Inactive tabs use `text-muted`.
- A tab may include a concise count, such as `Diagnostics 3`.
- Avoid brackets around every tab unless they improve clarity in limited-colour mode.
- Switching tabs must not cause layout jitter.

### 9.4 Contextual footer

Show four to six actions relevant to the current owner. Generate hints from authoritative binding metadata where possible.

Preferred grammar:

```text
Tab block   ⇧←/⇧→ tab   Enter interact   Esc back   ? help
```

Rules:

- Do not advertise unreachable commands.
- Hide disabled actions or label them as disabled.
- Use compact notation consistently.
- On narrow screens, prioritise `Esc`, navigation and the primary local action.

### 9.5 Chat transcript

Hierarchy:

1. User request.
2. Final assistant-facing response.
3. Approval or failure.
4. Grouped tool activity.
5. Routine progress and metadata.

Rules:

- Raw provider reasoning is hidden by default.
- Assistant answers should visually dominate routine activity.
- Group repetitive tools under a collapsible activity row.
- Tool calls use concise verbs: `Read 4 files`, `Ran cargo test`, `Updated source_viewer.rs`.
- Use colour only for result state, not every tool type.
- Preserve exact commands and errors in details.

### 9.6 Chat composer

- `surface` background, `hairline` border when inactive.
- Stronger `accent` border when it owns input.
- `CHAT · INPUT` must be visible through block title or composer label.
- Placeholder copy should be useful and short.
- Multi-line input may grow, but must not crowd out transcript context.
- Sending, cancelling and leaving input must be discoverable.

### 9.7 File tree

- Selected row uses `surface-selected` plus a pointer or strong text.
- Active file and selected tree row may differ; distinguish them.
- Git markers are compact and semantic:
  - `M` modified
  - `A` added
  - `D` deleted
  - `?` untracked
  - `!` conflict or error
- Directory expansion uses simple Unicode or ASCII markers with fallback.
- Do not clear the visible tree during a Git-only refresh.
- Empty, loading, unavailable and failed states must be distinct.

### 9.8 Source viewer

- Code remains the visual focus.
- Syntax highlighting is restrained; avoid rainbow density.
- Current line uses a subtle gutter marker or low-contrast surface, never a bright full-width cyan band.
- Search match hierarchy:
  1. active match
  2. other matches
  3. current line
- Line numbers use muted text; active line number may use accent.
- Horizontal scrolling must not detach markers from content.

### 9.9 Diff viewer

Use conventional semantics with textual fallbacks:

- addition: green + `+`
- removal: red + `-`
- context: body/muted
- hunk header: blue + `@@`
- file header: primary text

Rules:

- Preserve old and new line numbers.
- Do not use large background fills for every changed line if foreground and gutter markers are sufficient.
- Stale diff state must be explicit.
- Unsupported staged, binary, untracked or conflicted states must be truthful.
- Diff actions remain read-only unless explicitly expanded by a later product decision.

### 9.10 Inspector

The Inspector should answer one focused question per tab:

- **Task:** What is being attempted and what state is it in?
- **Context:** What information is attached or available?
- **Runtime:** Which model/provider/session is executing?

Rules:

- Hide unknown fields instead of filling the panel with `Not available`.
- Reuse authoritative state from the header, Git layer and task execution.
- Keep labels muted and values primary.
- Truncate long objectives with a visible ellipsis.
- Avoid creating display-only state that can contradict the application.

### 9.11 Bottom panel

The bottom panel is an interactive block, not a passive status bar.

- Opening it gives it focus.
- Tab strip is visible and uses the common Shift+Arrow grammar.
- Terminal means captured command output unless an interactive shell is actually implemented.
- Run and Diagnostics have truthful empty states.
- Activity should summarise events rather than repeat the Chat transcript.
- Closing restores previous focus.

### 9.12 Activity rows

Use a compact structure:

```text
✓ Read 4 files                         1.2s
● Running cargo test                  8.4s
! Approval required: remove directory
× cargo test failed                   exit 101
```

- The leading symbol and text both carry state.
- Duration and timestamps are muted.
- Expand only when details add value.
- Avoid a new row for every streaming delta.

### 9.13 Approval modal

Approval is consequence-first.

Information order:

1. Action-oriented question.
2. Exact command or operation.
3. Concise consequence.
4. Available decisions.
5. Optional technical details.

Example:

```text
┌─ Delete this temporary directory? ─────────────────────┐
│                                                       │
│  rm -rf /tmp/forge-test                               │
│                                                       │
│  This permanently removes the directory and contents.│
│                                                       │
│  [a] Approve once   [s] Allow session   [d] Deny      │
│  [Esc] Keep waiting                                    │
└───────────────────────────────────────────────────────┘
```

Rules:

- Use yellow for caution and red only for clearly destructive consequence.
- Background is strongly dimmed but still recognisable.
- Technical policy metadata is secondary.
- Do not generate security-critical consequence text with an LLM; use deterministic mappings.
- Exact arguments remain inspectable.

### 9.14 Search and jump bars

- Render as a compact transient row inside the parent block.
- Show mode in the block title or bar label.
- Query, match count and navigation hints remain visible.
- Zero results use `No matches`, not an error colour.
- `Esc` returns to parent navigation without leaving stale offsets.

### 9.15 Empty, loading and error states

Use truthful, compact language.

| State | Preferred copy |
|---|---|
| Loading | `Loading files…` |
| Genuine empty directory | `This directory is empty` |
| Repository missing | `No repository detected` |
| Load failure | `Unable to load files` |
| No validation | `No validation run yet` |
| No diagnostics | `No diagnostics` |
| No captured output | `No command output yet` |

An empty vector is not a universal UI state. Components must distinguish not-loaded, loading, empty, unavailable and failed.

### 9.16 Notifications

Use transient notices sparingly.

- Success: low-priority, brief, often no toast needed.
- Warning: visible until user can understand the consequence.
- Failure: persistent enough to inspect details.
- Avoid stacking multiple toasts over active work.
- Never use a success notice to compensate for unclear state in the underlying panel.

## 10. Syntax and Symbol Language

Prefer familiar terminal symbols with ASCII fallbacks.

| Meaning | Preferred | ASCII fallback |
|---|---|---|
| Running | `●` | `*` |
| Success | `✓` | `OK` |
| Failure | `×` | `X` |
| Warning | `!` | `!` |
| Collapsed | `▸` | `>` |
| Expanded | `▾` | `v` |
| Selected | `›` | `>` |
| Modified | `M` | `M` |
| Added | `A` | `A` |
| Deleted | `D` | `D` |
| Untracked | `?` | `?` |

Do not use obscure symbols merely because Unicode permits them. Alignment and recognisability matter more than novelty.

## 11. Responsive Degradation

Forge should remove secondary information before reducing legibility.

### Wide

- Files + Workspace + Inspector visible.
- Bottom panel may coexist.
- Full header metadata and contextual footer.

### Medium

- Narrow Files and Inspector.
- Hide low-value labels before values.
- Compact footer notation.

### Narrow

- Only one side block visible at a time.
- Workspace keeps the majority of width.
- Inspector rows collapse or become a dedicated view.
- Header omits model and duplicate status.
- Footer shows only essential local and escape/navigation actions.

### Short height

- Bottom panel shrinks first.
- Chat composer growth is capped.
- Tool details remain collapsed.
- Modal content scrolls rather than extending beyond the viewport.

### Minimum target

At `80 × 24`, users must still be able to:

- identify active block and mode
- move between visible blocks and supporting tabs
- type and send Composer input
- inspect a file
- inspect a diff
- read and answer an approval
- exit without terminal corruption

## 12. Accessibility and Reliability

- Do not rely on red/green distinction alone.
- Maintain readable contrast on both true-colour and ANSI terminals.
- Avoid flicker, animation and rapidly changing spinners.
- Keep cursor visibility correct when entering or leaving inputs.
- Terminal cleanup must restore raw mode, alternate screen and cursor after normal or abnormal exit.
- Long content must wrap, scroll or truncate deliberately—never overwrite borders.
- Unicode width must be measured using terminal display width, not byte length.
- Every overlay must own input and prevent event fallthrough.

## 13. Do's and Don'ts

### Do

- Make focus visible through border, title and mode.
- Keep the Workspace visually dominant.
- Use cyan for interaction and yellow for human attention.
- Use hairlines and surface contrast for hierarchy.
- Keep routine activity compact and expandable.
- Prefer deterministic, truthful status copy.
- Reuse a small set of visual primitives.
- Generate footer/help hints from authoritative bindings.
- Test wide, medium, `80 × 24`, limited-colour and Unicode cases.
- Preserve Forge's current black/cyan/yellow identity while warming the neutral surfaces.

### Don't

- Do not clone another product's colour palette or visual signature.
- Do not introduce gradients, shadows, glass effects or web-card styling.
- Do not fill active panels with bright cyan.
- Do not use colour as the only status signal.
- Do not show every internal event at equal visual weight.
- Do not let Composer input act as a fallback key sink.
- Do not advertise a shortcut that is not reachable.
- Do not display repeated `Not available` fields.
- Do not call captured output an interactive terminal.
- Do not redesign working interaction semantics during a visual-only task.

## 14. Ratatui Implementation Guidance

### 14.1 Theme architecture

Create or maintain one semantic theme layer. Components should request roles such as:

- `canvas`
- `surface`
- `border_inactive`
- `border_active`
- `text_primary`
- `text_muted`
- `state_warning`
- `state_success`
- `state_danger`
- `selection`
- `diff_added`
- `diff_removed`

Avoid hard-coded RGB values inside individual renderers.

### 14.2 Focus-driven rendering

The renderer must consume the same authoritative focus state used by event routing. Do not create a rendering-only `is_active` flag that can drift.

### 14.3 Binding metadata

A command declaration should ideally provide:

- command identifier
- default keybinding
- valid focus contexts
- enabled state
- short label
- help description

Footer and help surfaces should derive from this metadata where practical.

### 14.4 Snapshot coverage

Maintain rendering snapshots for:

- each active top-level block
- each Workspace view
- Conversation `NAV` and Composer `INPUT`
- search and jump modes
- approval modal
- selected supporting-surface tab inside inactive block
- wide, medium and `80 × 24`
- limited-colour fallback
- long Unicode paths and labels

Snapshot updates must be reviewed, not accepted blindly.

## 15. Agent Instructions

When an AI coding agent changes Forge UI:

1. Read this file before editing.
2. Identify the affected semantic roles and focus states.
3. Preserve the focus and navigation contract.
4. Reuse existing theme tokens and components.
5. Avoid general UI refactors unless the task explicitly requires one.
6. Add or update targeted rendering and interaction tests.
7. Verify contextual shortcuts against actual handlers.
8. Run the relevant targeted tests during development and the full suite once before completion.
9. Manually verify the affected workflow at wide and `80 × 24` sizes.
10. Report any deliberate deviation from this document.

## 16. Review Checklist

Before merging a Forge UI change, answer:

- Which block owns the next keypress?
- Is that ownership visually obvious?
- Is the selected supporting-surface tab distinct from focused block?
- Are `NAV`, `INPUT` or transient modes truthful?
- Do displayed shortcuts work in this context?
- Is important content readable without colour?
- Are approvals, failures and waiting states prominent enough?
- Is routine activity quieter than the final result?
- Does the interface remain usable at `80 × 24`?
- Did the change preserve terminal restoration and event-consumption invariants?

## 17. Provenance

This design is an original synthesis for Forge. It was informed by independently authored DESIGN.md analyses of Warp, OpenCode and Ollama from the VoltAgent `awesome-design-md` project. Those analyses document publicly observable patterns and are not official design systems from the named companies.

The reference files are retained separately for research. They must not be treated as Forge's implementation specification.
