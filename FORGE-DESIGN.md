---
version: 2.8
status: behavioral-contract-with-changeable-defaults
name: Forge TUI Design System
product: Forge
platform: terminal-ui
framework: Ratatui
summary: >-
  A terminal-native design system for Forge, an open human-agent development
  workspace. It combines calm workspace hierarchy, developer-first monospace
  clarity, restrained semantic status language, and per-theme accent identity
  governed by behavioral invariants: keyboard ownership is clear, state is
  truthful, work is protected, and focus remains distinguishable from outcomes.
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
  - Navigator (Sessions, Files and Git; temporary full-width view when narrow)
  - Sidebar (primary conversation, left of inspection)
  - Workspace (right inspector — File, Diff or GitHub issues)
  - BottomPanel / Composer (span the work surface)
  - StatusBar / Footer (chrome rows)
focus-blocks:
  order: [TaskStrip, Search, Files, Sidebar, Approval, Workspace, BottomPanel, Composer, Footer]
  labels:
    TaskStrip: SESSIONS
    Search: SEARCH
    Files: FILES
    Workspace: INSPECT
    Sidebar: CHAT
    Approval: APPROVAL
    Composer: COMPOSER
    Footer: FOOTER
    BottomPanel: PANEL
navigation:
  next-block: Tab
  previous-block: Shift+Tab
  sessions-tab: Ctrl+1
  files-tab: Ctrl+2
  git-tab: Ctrl+3 (repositories only)
  cycle-navigator-tabs: Ctrl+E
  navigator-tab-row: Up (first row of any navigator list, when the tab row is visible)
  go-back: Alt+Left
  switch-workspace-pane: F6
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

This document defines the behavior Forge must protect and records the current
TUI defaults in `crates/forge-tui`. Code establishes what a build does today;
it does not establish which interface best serves the user. When code and this
document disagree, identify whether the implementation, the reference, or the
design decision needs to change, and resolve that discrepancy in the same PR.

## 1.1 Behavioral invariants, presentation defaults, and history

Every design statement belongs to one of three categories:

| Category | Authority | How it changes |
|---|---|---|
| **Mandatory behavioral invariant** | §4 and the safety guarantees it identifies in component and session behavior | Preserve it while changing the interface. A presentation experiment cannot waive input ownership, truthful state, or work protection. |
| **Changeable presentation default** | The frontmatter mappings and the current treatments in §§2–10: layout, dimensions, focus signals, glyphs, colours, labels, bindings, and timing | Revise it through §1.2 when evidence supports a better experience. Update the implementation and its reference together. |
| **Historical decision or implementation limitation** | §11 and explicitly identified constraints in component descriptions | Record the rationale and a reason to revisit it. An earlier rejection or missing renderer feature does not prohibit a new solution. |

Prescriptive wording in a component reference describes the current default
unless it protects a §4 invariant. Exact cell counts, palette values, focus
cycles, and pane arrangements do not become correctness requirements merely
because they are implemented. Requirements about command authorization,
sanitized terminal output, unsaved work, session identity, and safe cleanup
remain mandatory wherever they appear.

The component references describe shipped behavior. A proposed design must be
identified as proposed until it is implemented and verified. Reading this
document must never lead a contributor to advertise a feature or binding that
the current build does not provide.

## 1.2 Evaluating and changing a default

Use the smallest comparison that can answer the design question:

1. **Name the user task and observed problem.** Record the terminal size,
   workflow, and relevant state. Distinguish an observed difficulty from a
   hypothesis or a personal preference.
2. **Describe the alternative and its tradeoff.** Identify the defaults it
   changes and the §4 invariants it must preserve. Structural alternatives,
   including central conversation views, inspectors, tabs, and task-specific
   layouts, are eligible for evaluation.
3. **Choose success criteria before comparing.** Use relevant measures from
   §1.3 and compare the current and proposed interface on the same tasks.
   Record regressions as well as improvements.
4. **Validate at the scope of the change.** A local style change needs a focused
   visual and interaction check. A new layout or navigation model needs the
   affected workflows, responsive states, themes, and input paths checked.
   Preserve safe cancellation, return paths, and existing drafts and buffers.
5. **Record the decision with the change.** Put the problem, evidence, tradeoff,
   and validation in the PR. Update the affected references; record structural
   changes or revised rationale in §11. If the evidence is inconclusive, keep
   the alternative identified as experimental rather than declaring it an
   improvement.

Changing a default is ordinary design work. It does not require a separate
approval ceremony solely because it departs from the current layout. Runtime
approvals for destructive or external actions remain governed by their own
authorization boundaries.

## 1.3 Evidence of a better experience

Choose measures that reflect the problem being solved; do not require every
measure for every change or invent user-study results from a developer smoke
check.

| Aspect | Evidence to compare |
|---|---|
| Task completion | Ability and time to delegate, inspect a file or patch, switch sessions, and resolve an approval without losing work or context |
| Navigation effort | Keystrokes, focus transitions, backtracking, and whether the next action is discoverable without memorized shortcuts |
| Error prevention and recovery | Accidental drafts or actions, wrong-session actions, refusal clarity, cancellation, and restoration of selection, scroll, and unsaved buffers |
| Reading and information access | Visible useful content, clipped or hidden critical state, command/error inspection, and readability of prose, code, and diffs |
| Accessibility | Legibility in light, dark, reduced-colour, and monochrome presentation; visible keyboard ownership; a usable fallback when terminal capabilities are absent |
| Responsiveness | Input latency, streaming readability, resize behavior, and whether background work interrupts navigation or moves the reader |

For structural changes, exercise representative delegation, inspection, and
intervention tasks at `80×18`, `120×40`, and `160×50`, plus the sizes around any
changed breakpoint. Include long paths and model names, overflowing content,
pending approvals, running work, and unsaved state where relevant. Compare
keyboard and mouse paths when both are affected. Use before/after terminal
captures or rendered artifacts with reproducible steps; a green build alone
does not prove a better experience.

Keep targeted tests for behavioral guarantees and performance. Validate
appearance through terminal inspection and reviewable captures; do not restore
UI, styling, or layout unit tests to freeze a particular palette, spacing, or
screen arrangement. Cite current verification rather than claiming a visual
rule is enforced by a test that no longer exists.

## 2. Design Character

These qualities guide judgment. They do not prescribe a particular number of
panes, a density setting, or a resemblance to another product. Prefer the
treatment that makes the user's current task clearer, and assess it using §1.3.

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

These references explain the current visual direction. The inspiration weights
in the frontmatter are descriptive, not acceptance targets for future designs.

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

- The accent identifies focus, interaction and navigable structure; focus must remain distinguishable from outcome state (see §5.1).
- Yellow/amber identifies waiting, caution and human attention (`waiting_border` pauses the composer while an approval is pending).
- Neutral `agent` text keeps routine narration below the answer; speaker labels
  distinguish authorship without borrowing focus or outcome colours.
- The developer's judgement is visually prioritised over agent narration.
- Active block, selected row and input ownership are separate concepts.
- The interface centres the loop: delegate, inspect, intervene, validate.

## 4. Core UX Invariants

These are not optional styling preferences. They are correctness requirements.

1. **Exactly one effective keyboard owner exists at a time.**
2. **The visually active block matches the actual event owner** (`focus.rs::normalize_focus`).
3. **Selected content and focused content are visually distinct.**
4. **Input, transient, running, and blocked states are distinguishable** without colour alone. A state has a consistent meaning across surfaces; its exact glyph and animation are presentation defaults (§5.3).
5. **An advertised action is reachable in the current context.** Its label and consequence match what it does; an unavailable action is identified as such. Hint compression must preserve a discoverable way to learn the action.
6. **Hidden or unavailable blocks cannot retain focus.** Navigation reaches the available controls and provides a clear way to leave captured input.
7. **Colour never provides the only indication of state.**
8. **Approvals and failures outrank routine activity.**
9. **Raw model reasoning is not ordinary chat content.**
10. **Supported terminal sizes preserve access to the primary task, critical state, and necessary controls.** The current supported minimum is `80×18` (`layout.rs::MIN_WIDTH` / `MIN_HEIGHT`); smaller terminals receive an actionable size message. A revised minimum or collapse strategy needs explicit validation under §1.3.
11. **Keyboard ownership remains visible without colour.** Shape, weight, wording, or a caret must identify the active control. The current title markers and local accents are examples, not mandatory implementations.
12. **Work and reading position survive ordinary navigation.** Preserve drafts, unsaved buffers, selections, and scroll where applicable. Destructive actions require an explicit decision with visible consequences; resizing or switching a view cannot silently discard work.
13. **The user can identify the session and resource an action affects.** Local results, remote checks, stale data, and unknown state remain distinct; changing a view cannot silently redirect an action to another session.
14. **Authorization and output safety survive presentation changes.** A read or preview does not authorize a write, push, merge, or unconfined retry. Untrusted text and link destinations must be sanitized, secrets redacted, and session/worktree cleanup must protect uncommitted work (§12).
15. **Terminal capability limits have usable fallbacks.** No critical action or state requires a particular font, mouse reporting, hyperlink support, animation, or true colour. Forge does not claim to control the user's terminal font settings.

## 5. Colour System

The following tokens and treatments describe the current defaults. Themes
use semantic tokens (`forge-config::ThemePalette`) so colours can change
without changing the meaning of state or keyboard ownership. Token mappings,
hues, and decoration may evolve under §1.2; the readability and non-colour
signals required by §4 remain mandatory.

Every theme supplies the full token set:

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
| `activity` | Work in progress and active-work emphasis |
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
| `link` | Actionable prose link hue (the `info` family in the built-ins) |
| `syntax.*` | Code highlighting palette |

The current design expresses depth through border weight, contrast, and
placement rather than shadows. An alternative treatment needs to justify its
content cost and preserve legibility.

Do not render large bodies of important text using dim styling; terminal dim support varies and may harm readability.

### 5.1 Distinguishable focus and outcome state

The behavioral requirement is that focus and outcome state remain distinguishable,
including without colour. The current palette diagnostic uses
`ACCENT_STATUS_MIN_HUE_DISTANCE`:

> Default palette guideline: keep the accent at least 60° of hue away from
> `success`, `warning`, and `error`.

The accent answers *"where am I and what will my next keystroke touch"*; outcome
colours answer *"what happened"*. The diagnostic warns about potentially
confusing mappings; it does not prove legibility or perceptual separation.
Compare text/background contrast and the actual focus, selection, and outcome
signals in light, dark, reduced-colour, and monochrome presentations. A palette
that meets the numeric threshold can still fail those checks.

With `NO_COLOR`, remove colours from the completed frame before backend output
so bold, italic, and inverse attributes survive colour suppression. Keep input
carets visible with inverse video; focus and status markers remain legible by
shape and text.

`info` and `agent` are excluded from the current diagnostic because neither
reports an outcome. A different palette may use other hues or separation rules
when evidence supports them; update the diagnostic and its guidance together
rather than leaving a warning that contradicts the accepted design.

The current built-ins use neutral grey grounds and blue focus accents. Describe
new palettes from their token values and rendered behavior; an older theme's
identity notes are historical context, not a requirement to preserve its hues.

### 5.2 Semantic roles

| Role | Token | Meaning |
|---|---|---|
| Accent | `accent` | Focus, navigation, active structure |
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
| `[>]` | Active work (orange in the current themes; prose emphasis may share the family, while its text and weight carry a different meaning) |
| `[✓]` | Complete (neutral in history; green only for a confirmed successful result glyph) |
| `[!]` | Failed |
| `[-]` | Cancelled |
| `[?]` | Warning / needs attention |
| `[\|]` | Blocked |

Git status is single letters from the same module: `M` `A` `D` `?` `!` `U` (modified / added / deleted / untracked / ignored / conflicted), bold and semantically coloured. The `✓` tick lives inside the `[✓]` completion marker as well as reviewed files and status-bar outcomes; `✗` only for a failed status outcome. Animation is restrained and never changes layout width.

**Running has a consistent meaning across surfaces.** The current live turn
line, navigator session rows, and collapsed navigator chip share the braille
spinner `⣾⣽⣻⢿⡿⣟⣯⣷` (`widgets/turn_line.rs::SPINNER_FRAMES`). Each frame
occupies one cell and advances every 125 ms on a monotonic clock, so animation
never shifts a label or speeds up when keys arrive. `NO_COLOR` uses one-cell
ASCII frames. `[tui] reduced_motion = true` uses a static `*` for working and
suppresses brightness motion while preserving polling and input delivery.
The missing preference defaults to false for existing configurations.
The footer instead names
the lifecycle beside a fixed-width `●` that pulses in brightness, and its
background chips use static category/state glyphs (§9.3).

Those treatments are defaults. A replacement must preserve recognizable state,
stable geometry, and a static or textual fallback when animation or glyph
coverage is unavailable. Introducing a different symbol must not make running
look like waiting, completion, or keyboard focus.

Available provider chunks paint promptly; there is no typewriter reveal timer.
The existing 150 ms entrance debounce prevents a pinned busy line from flashing
for very short work. Waiting, queued and terminal outcomes stay static; blocked
background rows say waiting instead of repainting an elapsed counter. Idle
polling remains independent of decoration so external state can still arrive.
Human decisions use immediate focus, explicit text and a steady warning. The
prototype's brief arrival flash is omitted to keep the inspected request steady.

### 5.4 Limited-colour fallback

Every semantic state must include a textual or symbolic cue:

- Lifecycle: the §5.3 bracket markers (`[ ]` `[>]` `[✓]` `[!]` `[-]` `[?]` `[|]`)
- Git: single letters (`M` `A` `D` `?` `!` `U`)
- Success: `[✓]` (green only for a confirmed result) or `✓` for reviewed/status outcomes
- Failure: `[!]` or `✗` for a failed status outcome
- Focus: a visible title marker, caret, or other non-colour ownership signal; border weight may reinforce it
- Selection: neutral background plus the `>` pointer, never tint alone

Themes map onto ANSI fallbacks for terminals without true colour.

## 6. Typography and Text Treatment

Forge inherits the user's terminal font. Never bundle or require a font.
The hierarchy, casing, emphasis colours, and hint treatment below are current
defaults. Changes may improve readability or discoverability under §1.2 while
preserving legibility and the terminal-capability fallbacks in §4.

### Rules

- Use monospace throughout.
- Forge uses terminal attributes: bold, dim, underline, italic, foreground and background.
- Use bold sparingly: active labels, headings, consequences, status glyphs.
- Use underline for links or explicit selected actions only. An underline on
  its own never says "link": a link carries the `link` hue, which is what
  separates it from a focused footer chip underlined in `accent`.
- Use dim only for genuinely secondary metadata and always test legibility.
- Use italics for model-prose emphasis only, and never as the sole signal.
  Terminal italic support varies, so `md_emph` and the wording carry meaning
  when the modifier is dropped.
- Model prose emphasis takes colour: `**strong**` is `md_strong` (orange) at
  bold weight and `*emphasis*` is `md_emph` (greenish yellow) at italic weight,
  so key claims and qualifications pop out when skimming. These tokens apply
  to prose; chrome, status, diffs, code, and the composer use their own semantic
  tokens, which may share a colour family.
- Use uppercase for compact structural labels only — the current focus-block labels are `SESSIONS`, `SEARCH`, `FILES`, `CHAT`, `INSPECT`, `COMPOSER`, `FOOTER`, `PANEL`, `APPROVAL` (`types.rs::FocusBlock::label`).
- Use sentence case for messages, explanations and actions.
- Avoid decorative ASCII art inside the product chrome.
- Current chrome combines ASCII markers (`>`, `v`, `[ ]`, `*`, `+`/`-`) with
  Unicode disclosure, lifecycle, and hint glyphs (`›`, `⌄`, `↑↓←→`, `⇧`, `⏎`).
  Labels and alternate cues must keep critical state and actions understandable
  when glyph coverage is incomplete (§4.15). Glyph choice is a default, not a
  requirement to reproduce a particular symbol family.

Hierarchy comes from weight, token step and placement — never from size, since terminal font size belongs to the user.

| Level | Treatment |
|---|---|
| Brand / application title | `theme::brand()` — bold primary |
| Active block title | Bold primary label with an accent `>` marker (`> Terminal`) |
| Inactive block title | Secondary text, two-space indented to hold alignment |
| Primary content | `text_primary` — assistant response, source code |
| Prose strong | `md_strong` + bold — key claims inside an answer |
| Prose emphasis | `md_emph` + italic — qualifications inside an answer |
| Prose link | `link` + underline — an actionable destination |
| Supporting content | `text_secondary` — metadata, descriptions |
| Utility content | `text_muted` — keys, timestamps, counts |

### Hint grammar

Current key hints use one grammar (`hints.rs`): `key verb` pairs joined by ` · `,
keys bold, verbs sentence case. The shared renderer drops verbs before trailing
pairs under width pressure and keeps the row unwrapped. The Git patch footer
instead drops secondary pairs first, keeping `? keys` and `Esc close` labeled
and reserving a gap before branch/status metadata (§9.8). Bare keys save space
but lose meaning for a user who has not learned them. Evaluate fewer labeled
actions, expanded help, or a different hint budget when comprehension suffers.
Preserve reachable actions and truthful bindings; exact compression order and
row height are defaults.

```text
Enter confirm · Esc cancel
↑↓ select · Enter confirm · Esc skip
```

## 7. Current Layout Defaults

The current layout is implemented in `crates/forge-tui/src/layout.rs`. Its
regions (`LayoutRegions`) are an implementation reference, not a restriction
on future screen structure. Alternative layouts must satisfy §4 and demonstrate
their tradeoffs through §1.3.

```
┌──────────────────────────────────────────────────────────┐
│ Repository / branch · session attention                   │
│ Approve-all warning (when on) · selected session strip    │
├──────────┬───────────────────────────────────────────────┤
│ Navigator│ Conversation · current resource · F6 switch    │
│ (opt.)   ├─────────────────────────┬─────────────────────┤
│ Sessions │ Conversation            │ Resource inspector  │
│ Files    │ (primary, borderless)   │ File / Diff / Issues│
│ Git      ├─────────────────────────┴─────────────────────┤
│          │ Terminal (when open)                          │
│          │ Queue / background tasks (when present)       │
│          │ Composer                                      │
├──────────┴───────────────────────────────────────────────┤
│ Footer: model / effort · turn state / context             │
└──────────────────────────────────────────────────────────┘
```

### 7.1 Blocks

1. **Navigator** — the left column. `Sessions` and `Files`, plus `Git` in
   repositories, share one column (`§7.7`). `Files` is the repository explorer
   with Git status markers and its own search row (`Search` is a separate Tab
   stop nested in the same bordered box). `Sessions` is the multi-session list.
2. **Conversation** — the primary work surface, left of the resource inspector.
   It uses the canvas without an enclosing outline; a thin scrollbar occupies
   its right padding. Its internal focus block remains `Sidebar`, labeled `CHAT`.
3. **Resource inspector** — the right pane for `File`, `Diff`, or GitHub issue
   details. Its internal focus block remains `Workspace`, labeled `INSPECT`.
   With no resource open, conversation uses the whole work surface. On narrow
   terminals, `F6` or the workspace tabs switch between retained views.
4. **BottomPanel** — the interactive terminal, spanning the work surface under
   conversation and inspection. One top-rule border, thick + `> Terminal` title
   when focused. Closing it retains the shell for reopening. The queue,
   background tasks, and composer also span the work surface beside the
   persistent navigator; composing does not depend on the inspector's width.
5. **StatusBar / Footer** — chrome rows described in §9. Feedback is recorded
   in the app model and surfaced through notices and toasts; the current layout
   reserves no separate feedback/status-line row (§9.10).

### 7.2 Current spatial priority and alternatives

The current split gives conversation and composer priority:

1. Modal or approval overlay (HITL card in the transcript is itself a Tab stop).
2. Transient input such as source search or jump-to-line.
3. Conversation and composer.
4. Resource inspection.
5. Files.
6. BottomPanel.
7. Decorative or redundant metadata.

The invariant is access to the active task, necessary controls, and critical
state. Conversation-first is a default, not a permanent ranking of delegation
above inspection. Evaluate file- or review-focused layouts, user-controlled
pane sizing, temporary single-pane views, and alternate collapse strategies
when they make the current task easier. A collapsed pane needs a discoverable
return path; hidden content must not strand an approval, draft, or unsaved file.

### 7.3 Width behaviour in terminal columns

Content width is the frame width minus one outer gutter column on each side (`FRAME_INSET_X`).

| Frame width | Behaviour |
|---|---|
| ≥ 116 | Conversation and inspector appear together when at least 60 + 1 + 44 work columns fit |
| < 116, resource open | `F6` or a workspace tab shows conversation or inspection at full work width; both retain their state |
| Resource open, ≥ 132 | A persistent navigator can also fit: at least 24 + 1 navigator columns beside the two work panes |
| No resource open | Conversation expands; a persistent navigator is eligible from 116 columns |
| Navigator requested without room for a persistent column | It temporarily fills the body, leaving the composer accessible; leaving navigator focus returns to the retained work view |

The navigator defaults to 24 columns, growing with an explicit saved width
preference when the remaining panes still meet their floors.
Conversation defaults to three fifths of the remaining body width, reserving
at least 44 region columns for inspection and 60 for conversation. Saved width
preferences remain effective within these floors. Region widths include any
border and padding; conversation only has one padding column per side.

This gives a file review more conversation and composer width at 120 columns.
The tradeoff below 116 columns is an explicit view switch rather than two
constrained text panes. `Ctrl+P`, `Ctrl+1`/`2`/`3`, and the focus cycle reveal
temporary navigation; width alone never makes those actions unreachable.

### 7.4 Height behaviour

- StatusBar consumes one identity row at every height. Footer uses one row
  when no queue or retained background work exists, and adds a count-chip row
  when either collection is nonempty.
- Composer input is capped at four visual rows, or three below 28 frame rows,
  plus one top rule. Empty input uses one row at compact heights and two at
  comfortable heights. Longer drafts scroll without discarding stored text.
- Theme picker dock is 12 rows (`THEME_DOCK_H`), sized to show built-ins without scrolling.
- Below 24 frame rows, an open terminal appears while `Panel` owns the keyboard
  and yields its rows to other focused surfaces. `Tab`, `Ctrl+Backtick` or
  `/terminal` reveal the retained shell; navigation, editing and decisions
  remain usable at 80×18.
- A modal leaves surrounding context visible so it reads as overlaying Forge, with the background clearly secondary.
- Every modal title uses the shared `> Title` grammar (`theme::modal_title`) — including the workspace unsaved-changes and file-changed-on-disk conflicts. Borders keep severity colour; the marker says who owns the keyboard.

These row budgets are current defaults. Evaluate them against visible useful
content, editable input, and discoverable controls at the supported sizes;
decorative chrome and picker previews should yield before the active task.

### 7.5 Cell spacing

Use a compact cell-based scale so the shell reads as one application while
text keeps breathing room inside borders:

- `0`: chrome gap (`CHROME_GAP_Y`) and transcript ↔ composer gap
  (`COMPOSER_GAP_Y`); pane borders separate these regions.
- `1`: standard inline gap, outer frame gutter (`FRAME_INSET_X`), interior
  padding (`PANE_PAD_X`), column gutter (`PANE_GAP_X`), vertical pane gap
  (`PANE_GAP_Y`).

Concretely (`design.rs`): one blank column separates navigator, conversation,
and inspector; no blank row separates chrome from content or transcript from
composer. Bordered panes put text two cells from their edge. The borderless
transcript's own two-column inset is not wrapped in another layer of pane
padding; its scrollbar occupies the right edge. Composer, queue, and
bottom-panel text use `TEXT_INSET`.
The Footer uses the same two-column origin. Feedback appears through notices rather
than a separate status line (§9.10). Rounded frames use Ratatui border glyphs
and semantic theme tokens; they do not change terminal typography.

Avoid double-padding a bordered block and its inner component.

### 7.5.1 Transcript density

The conversation has two densities (`markdown.rs::Density`):

- **Airy** is the default at comfortable pane heights. It ensures one blank row
  before a section heading, one after the heading rule, and one on each side of a
  fenced code block, sharing adjacent blocks' separators rather than adding
  another row. It adds one between distinct tool/activity groups (rows inside a
  group stay tight). `You` / `Answer` labels sit directly above their content;
  the separator belongs between messages rather than between label and body.
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

### 7.6 Responsive validation

- Preserve access to the user's current task and its next action.
- Keep approvals, failures, unsaved state, and session identity accessible.
- Give collapsed panes a discoverable way to reopen or reach their content.
- Remove secondary metadata before removing primary content.
- Truncate paths visually without mutating stored values.

Inspect layouts at least at these sizes, following §1.3:

- `80×18` (enforced minimum)
- `120×40`
- `160×50`

Also exercise sizes immediately around changed breakpoints and resize while
input, selection, scroll, and unsaved state are active. Record before/after
captures and interaction results. The dimensions are a baseline for review;
they do not establish that the active task remains usable on their own.

### 7.7 Navigator — Sessions, Files, and Git

The current left column is a **navigator** with `Sessions` and `Files`, plus
`Git` in repositories. It is the main multi-session surface. The earlier top
task strip is recorded as a historical decision in §11.

- Tabs: `Ctrl+1` / `Ctrl+2` / `Ctrl+3` select `Sessions` / `Files` / `Git`
  in repository mode; `Ctrl+E` cycles available tabs (`input.rs`, `workspace.rs`).
  `Tab` cycles focus blocks, with the input exceptions in §8.1. The current
  navigator shows one tab's content at a time to avoid adding a fourth content
  column. A different arrangement can be evaluated under §1.2.
- **The named tabs identify the active pane.** Do not repeat `SESSIONS` /
  `FILES` / `GIT` or a title focus marker above the tab row. The persistent
  session strip renders session names only, including below `files_fit()`
  where the tab row is hidden; its selected and focused session styling stays
  distinct.
- The tab row is reachable from the keyboard: `↑` at the first row of the
  active navigator list moves onto the visible row, where `←` / `→` walk its stops left to right —
  `Sessions`, `+`, `Files`, and `Git` when available — `Enter` activates the stop the cursor rests on,
  `↑` / `↓` / `Esc` step back into the pane the active tab shows, and every other key
  is inert on the row. The row is a sub-focus of the column, never a `Tab` stop
  (`§8.3`).
- The row's `+` cell creates a session. It is **not** a tab: it never
  takes the active tab's ground, and the row's cursor is the only thing that
  marks it, so the tab bar keeps showing which tab is active (`§9.6`). It runs
  the same prompt-less create as `n` in the list and `/new` in the composer, so
  all three stay one verb; the session is named from the first prompt submitted
  in its composer. It is docked to the `Sessions` tab — sharing that tab's
  right edge, immediately left of `Files` — so the create verb reads as acting
  on sessions rather than on the row at large or on `Files`, which is the one
  thing that was ambiguous when it sat at the row's far right. It stays
  keyboard-reachable from any navigator tab without switching tabs and is
  clickable whenever visible. Its three
  columns come from the `Files` tab — the only elastic box on the row — so on
  navigators narrow enough for the need badge to fit, the badge needs three
  more columns before it reappears beside the cell.
- Default tab: `Sessions` when more than one session exists, `Files` otherwise.
  The choice is remembered for the session.
- **Sessions tab** is a vertical, attention-ordered list. A row is five
  reserved cells and an elastic label — `bar marker space glyph space label` —
  so no state (selection, hover, expansion) can move another row's label. The
  glyph is the row's state, one shape per state, never colour alone:

  | Glyph | State | Source |
  |---|---|---|
  | `○` | idle | list-local |
  | `⣾⣽⣻⢿⡿⣟⣯⣷` | working | the turn line's frames (`§5.3`) |
  | `●` | needs you | list-local |
  | `⇥` | queued | list-local |
  | `✓` | completed | `TurnLifecycle` |
  | `✗` | failed | `TurnLifecycle` |
  | `■` | cancelled | `TurnLifecycle` |
  | `∅` | interrupted | `TurnLifecycle` |

  Only the first four are the list's own; the four outcomes borrow the turn's
  vocabulary so the list and the transcript cannot disagree about how a turn
  ended. Precedence is attention → queued → working → lifecycle. Rows carry the
  primary-text label and a quiet qualifier naming the same state as the glyph,
  followed by age. A finished turn says `completed`, `failed`, `cancelled`, or
  `interrupted` even after its runtime becomes idle. Attention adds a weight
  step to the label. Branch and worktree context belong only in the expanded
  peek, not ordinary rows. Ownership remains internal.
- **Selection is the bar plus the ground; the cursor cell is disclosure.**
  Selection is the accent bar in the reserved gutter plus a ground across both
  of the row's lines, and the cursor cell is `›` collapsed, `⌄` expanded — the
  same two glyphs the `+` cell's row uses for `§9.6`. While the navigator owns
  the keyboard the ground is the neutral `selection` step; when the block loses
  the keyboard the row keeps the bar and a weight step and gives up the ground
  (`§8.5`). In the current treatment a selected row has no accent wash
  (`§8.4`); hover uses its raised ground and `›`, without the selection bar
  (`§8.6`).
  `Enter` attaches, `Space` peeks and replies inline, `n` creates a session and
  opens its composer (the session is named from the first prompt submitted in
  that composer), `s` stops, `c` continues, `x` archives and cleans
  (confirmed), `r` renames. The same create verb is the row's `+` cell and the
  composer's `/new`, so starting a session never requires visiting the list.
- **The expanded row is framed.** `Space` frames the peeking row in a rounded
  accent box whose left edge takes the reserved gutter and whose right edge the
  list's last column, so expanding a row moves nothing; the peek hangs under it
  behind one guide rule — the last answer, then the reply box, a hairline, and
  the `§6` hint grammar. A frame costs a line above and below the row, so it is
  drawn only when the whole block fits: a short pane falls back to the unframed
  layout rather than painting half a box.
- **Files tab** uses the explorer. Filename navigation (`Ctrl+P`) and Find in
  Files (`Ctrl+Shift+F`) are separate modes of the search field. Content results
  use collapsible file groups with line-numbered, highlighted snippets below
  each path; selecting a snippet opens that source location, not a rendered
  preview. Match origin is conveyed by structure, never a badge or colour alone.
- **Git tab** appears only where the workspace is a repository (`.git` at the
  session's root — a directory in a plain checkout, a file in a linked
  worktree, so both qualify). It is not a new column and adds no permanent
  chrome: it splits the space `Files` already had. Its column is the explorer
  filtered to the changed files and the patch renders in the Workspace pane, so
  entering the tab opens the working-tree review and leaving it puts the pane
  back exactly as `Esc` does. `Ctrl+3` reaches the same view on narrow terminals
  through the temporary navigator. The column
  and the patch are one surface with one keymap (§8.3): the list owns
  `↑`/`↓`/`s`/`u`/`i`/`Enter` and the patch owns the diff keymap, so the hint
  row the patch draws is the truth about every key in the tab.
- Ownership (primary/managed/attached), slots/pinning, and the
  archive/cleanup/remove split are internal — not navigator affordances.
- When there is insufficient room for a persistent navigator, its inactive
  column collapses. The `Sessions` list falls back to a one-line status-row chip
  (`⌄ 2 need · ⣾ 1 working`) so session attention stays visible, and the session
  switcher (`F3`, `/sessions`) keeps every session reachable. The chip carries
  the same live spinner frame as the rows it replaces, on the same tick: on a
  collapsed column it still separates work in flight from work waiting on you.
  Direct navigator shortcuts or cycling focus reveal a temporary full-width
  navigator. Returning to conversation or inspection restores the retained view.
- Conversation and inspection share the work surface. A pending approval or
  question reveals conversation and prevents temporary navigation from covering
  its decision card. Drafts and dirty editor buffers survive view switching.

#### GitHub workflow safety

Repository issues are a nested Git view, not a modal. The list occupies the
existing navigator and the selected issue, linked PR, checks and handoff preview
occupy the workspace. Closing issues restores the previous review state.
Network reads run off the UI thread; unknown, loading, stale and failed states
must never look like an empty successful response. Remote text is untrusted.

Starting an issue session and applying feedback require explicit submission.
Starting an issue does not authorize pushing or creating a PR. Refreshing PR
status is strictly read-only and never authorizes a merge. Mutating actions must
verify the linked session, issue, repository and PR before executing. Local test
results and remote checks remain separate evidence; check status is associated
with the inspected commit, not a permanent session-level success badge.

## 8. Current Focus and Navigation Defaults

The current event model below is a reference for compatibility and discoverability.
The required behavior is clear ownership, reachable actions, safe routing, and
recoverable navigation (§4). The number and order of focus blocks, labels,
shortcuts, and subfocus states may change through §1.2.

### 8.1 Blocks and cycle

Nine spatially stable focus blocks (`types.rs::FocusBlock`), cycled by `Tab` / `Shift+Tab` through a fixed order that skips unavailable blocks:

```
TaskStrip → Search → Files → Sidebar(CHAT) → Approval → Workspace(INSPECT) → BottomPanel(PANEL) → Composer → Footer
```

- `Approval` enters the cycle only while a HITL request or agent question is pending.
- `Workspace` enters the cycle only with an open resource. In Git review,
  `Tab` from the changed-file list goes directly to the patch; `Shift+Tab`
  returns to the list. On narrow terminals focus reveals the target view.
- `Search` is a separate Tab stop rather than a sub-mode of Files. `Tab`
  normally cycles blocks; the terminal receives plain `Tab` for completion,
  and active composer slash suggestions use it for completion. `Shift+Tab`
  leaves the terminal block (§8.3).
- Opening an interactive block focuses it; closing a block restores the previous valid owner, falling back to the Composer (never the Workspace, which is a modal editor).
- A handled event never falls through to another block.
- Model activity does not capture pane navigation: `Tab` / `Shift+Tab` still
  leave the Composer while thinking, answering, or running tools. Plain `Tab`
  completes an active slash suggestion; `Enter` submits or queues a draft.
- `Esc` pops exactly one interaction level.

The label vocabulary is `SESSIONS SEARCH FILES CHAT INSPECT COMPOSER FOOTER
PANEL APPROVAL` (`types.rs::FocusBlock::label`). Internal `Sidebar` means
conversation; internal `Workspace` means resource inspection.

### 8.2 Modes

`FocusMode` has exactly two values (`types.rs`):

- **Navigation** — block-level keys apply.
- **Transient(owner)** — a captured input field owns keys: `SourceSearch` or `JumpToLine`.

The current design expresses text entry through the focused block and local
editor state, without a persistent global mode chip. A title marker or caret
communicates ownership. If that is insufficient for a task, a clearer mode
indicator or a simpler interaction model may replace it; ownership must stay
visible and accurately describe the next keystroke.

### 8.3 Navigation grammar

| Action | Binding |
|---|---|
| Next visible block | `Tab` (while the `Panel` block holds the keyboard, plain `Tab` goes to its shell) |
| Previous visible block | `Shift+Tab` |
| Navigator tabs `Sessions` / `Files` / `Git` (repository mode) | `Ctrl+1` / `Ctrl+2` / `Ctrl+3`; `Ctrl+E` cycles all available tabs (`Sessions` → `Files` → `Git` → `Sessions`). The Git tab separates staged and unstaged changes; paths changed on both sides appear in both groups, and untracked files are unstaged. `Ctrl+3` is inert outside a repository |
| Navigator tab row | `↑` at the first row of the active navigator list when the row is visible; `←` / `→` walk the available `Sessions` · `+` · `Files` · `Git` stops, halting at each end, `Enter` activates the stop, `↑` / `↓` / `Esc` step back into the pane |
| Enter interaction | `Enter` or `i` where appropriate |
| Leave one interaction level | `Esc` |
| Go back through workspace history | `Alt+←` |
| Switch conversation / current resource | `F6` or click its workspace tab; retains history, draft, dirty buffer and reading position. Modals, captured inputs and the terminal retain their keys |
| Start a draft | any printable key the active block has no binding for (`input.rs::type_to_compose`) |
| Contextual help | `/help` |
| Toggle the session scratchpad (running notes) | `Ctrl+N` |

Type-to-chat is the current fallback outside `Panel`: an unconsumed printable
key starts a draft and moves focus to the composer. Local bindings are matched
first. This saves a focus transition but can surprise a user who expected to
search or navigate. Evaluate accidental drafts and discoverability before
extending it; an explicit compose action or another fallback is eligible under
§1.2. Preserve the existing draft when changing the behavior.

The mandatory routing boundary is that shell input must not become a model
prompt, editor input must not escape its owner, and a handled shortcut must not
also enter text or act on a second control. The current Panel routes unclaimed
bytes to its PTY.

No block switches tabs on `←` / `→` while its pane holds the keyboard. Plain
arrows keep their in-block meaning: the session cursor in `Sessions` (`←`/`→`
as well as `↑`/`↓`), tree collapse/expand in `Files` and its `Search` row, chip
selection in `Footer`, the composer caret (its text input keeps normal arrow
behaviour), and pass-through to its shell in `Panel`. In the file view a plain
`←` is the same history-back as `Alt+←`. Modified arrows never switch tabs.

The one place `←` / `→` switch tabs is the tab row itself, reached with plain
`↑` at the first row of the active navigator list. The row is a sub-focus of the
navigator column rather than a block: `Tab` / `Shift+Tab` still cycle blocks
from it (it is never a Tab stop), only `←` / `→` / `Enter` / `↑` / `↓` / `Esc`
are bound on it, and the pane
under it paints as unfocused while it is up. `←` / `→` move between the row's
available stops (`Sessions`, `+`, `Files`, and repository-only `Git`) and halt
at each end rather than wrapping, so the movement always
matches the row as drawn; `Enter` activates the stop under the cursor — a tab
steps back into its pane, the `+` cell creates a session. Resting on `+` moves
nothing else: the active tab keeps its ground and `focus.block()` keeps the
pane on screen, so the row never covers a key owner that is not drawn.

In Git review, `↑` reaches the tab row only while the changed-file list owns
the keyboard. Patch focus keeps `↑` / `↓` as scrolling keys, and an open diff
search retains input ownership. Entering and leaving the tab row preserves the
draft, selected path/side, and patch reading position.

The **Git tab** is the one tab whose surface spans two blocks — the
changed-file list in the navigator column (`Files`) and the patch in the
Workspace pane — so both route shared review actions through one keymap. The
focused list keeps the keys it is built around, because it is the file picker: `↑`/`↓`
move its own cursor (independently of the tree cursor, so a path changed on both
sides stays addressable twice), `s`/`u` stage the selected side, `i` opens the
inline issues view, and `Enter` hands the keyboard to the patch. The focused
patch scrolls with `↑`/`↓` without changing the selected file. In issues,
`Esc` restores the preceding Git review without recreating its selection or
scroll; issue actions never fall through to staging or commit bindings. `Esc`
leaves the ordinary Git review from either pane. Every other key belongs to the patch, whose hint row
advertises the diff keymap — routing those from the list is what keeps that row
honest, and it is why a printable key in this tab commits or switches source
instead of quietly typing into the chat draft. Leaving the tab (`Ctrl+1`,
`Ctrl+E`, the row, or `Esc`) puts the workspace pane back exactly as `Esc` does
outside repository mode.

Git rows are grouped staged-first, then unstaged, with paths sorted within
each group and at most one heading per nonempty group. A successful stage or
unstage advances to the next file in the same group when one exists. Status
refreshes preserve the selected path and side. Working-tree changes and Git
index replacements trigger debounced status refreshes, including linked
worktrees. `o` opens the selected working-tree file; preview-capable files
keep their preview-first behavior (`i` edits), and editor focus owns its keys.

### 8.4 Active block treatment

Three border levels (`design.rs`, `theme::panel_border`):

- **L1 — pane frame.** Bordered panes, focused or not, take the same neutral
  `border` in the current design. This keeps large outlines quieter than
  content and distinguishes pane structure from the local focus accent.
- **L2 — inset field.** The composer outline and the explorer search separator
  sit at the same neutral step as L1; a field never reads as a second,
  louder box. Explorer search has a bottom rule, leaving the query open above it.
- **L3 — local accent.** Only the element that owns the keyboard or the
  selection: the active tab's ground, the navigator tab row's outlines while
  the row itself holds the keyboard, a focused search field's border, the
  composer's top edge, a pane's `>` title marker, the scrollbar thumb. Thick
  rules survive only where the region is a single rule (the bottom panel).

The current active-block treatment combines at least two signals from the L3 set:

- accent or bold block title
- explicit state marker where relevant (`> Terminal`, `> Conversation`)
- caret, scrollbar thumb, or the active tab's ground at the point of interaction

The transcript has no enclosing frame. The workspace tab row identifies
conversation and the resource: the selected view is bold and underlined, while
an accent `>` identifies the pane that owns the keyboard. These are separate
signals: a selected view does not claim focus while the composer is active.
The scrollbar takes a solid accent thumb while conversation owns the keyboard,
a muted half-block otherwise. Modals suppress background focus.

The current treatment avoids a full accent fill, which can overwhelm content
or resemble selection. A different border, marker, or fill treatment is eligible
when it improves ownership recognition without confusing focus with selection
or outcome state. At least one effective ownership signal must survive loss
of colour.

### 8.5 Selected tab versus focused block

- **Block focus** currently uses local L3 markers and the block title while
  the pane outline stays neutral. Alternative treatments follow §8.4.
- **Selection** (a row, a list item, a diff entry) is shown inside the block.
- A selection inside an inactive block stays visible but muted, and never implies keyboard ownership.

### 8.6 Mouse

Mouse is a second input for the same grammar, never a separate mode. Clicking moves block focus and acts on the hit target; hover previews without moving focus.

- **Click** focuses the block under the pointer (navigator, task strip, composer, footer, conversation, workspace, panel), handing its bindings to the keyboard. Clicking a navigator list row clears tab-row keyboard subfocus. A second click at the same cell within 400 ms acts: a navigator session row attaches, a Git-change row opens its patch, and a background-task row opens its live Jobs or Agents view; a file-tree row opens on the first click. Queued-message rows only select: double-click never cancels a message or edits an unrelated last message. Dock clicks use the identities from the painted frame, so reordering cannot redirect a click to a different item.
- **Overlay lists** (model picker providers/models/effort, resume picker, session switcher, theme dock, GitHub issues and issue actions, commit-suggest, branch picker, read-only file explorer) record painted row geometry. A single click moves the highlight (and the picker's focused column); a double-click confirms through the same `Enter` path the keyboard uses. Clipped paragraph rows are not pointer targets. Clicking an issue preserves the action menu's selection.
- **Composer popups** (slash suggestions and `Ctrl+r` search) use painted row geometry: click selects while the composer keeps the keyboard; double-click accepts. These popups currently do not show hover.
- **Editor** clicks place the caret using the text body's gutter and scroll geometry; read-only source clicks move the current line. Source-search and jump-to-line prompts retain keyboard ownership and block click, hover and drag behind them, as they already block the wheel.
- **Wheel** scrolls the focused pane's content (conversation, file tree, source viewer), matching the keyboard page/step size. `Shift` pages.
- **Right-click** opens the copy/clear context menu over a text selection.
  Its painted and clickable rectangle stays inside the frame, including after
  resize. Selection endpoints are inclusive display cells: wide characters and
  combining marks copy as whole glyphs. Source, preview and terminal copy use
  the rendered viewport; scrolling, editing or resizing invalidates a finished
  selection instead of leaving its highlight over unrelated text.
- **Paste** follows keyboard ownership: dialogs, context menus and overlays
  precede the underlying terminal or editor. File editors and the scratchpad
  accept multiline paste in Insert mode and query text in Search mode; Normal
  and Visual mode reject it visibly. Command-line paste inserts text without
  executing it. Preview wheel input scrolls the visible preview, not its
  backing editor; Shift+wheel invokes one page movement.
- **Hover** (when the terminal reports motion) is the pointer's focus ring, and only actionable surfaces take it: session rows, file-tree rows, footer chips, approval options, navigator tabs, queued-message rows, background-task rows, and overlay list rows. It combines a raised `surface_hover` ground with one non-colour signal — a leading `›` marker in the row's reserved marker cell and/or a weight step — so clickability is never colour-only; the marker cell is pre-reserved, so hover never shifts text (`§7.7`). It never moves keyboard focus and never changes layout. Terminals that do not report motion simply show no hover. Precedence stays focused block > selected row > hover: `selection` is the strongest neutral ground in both built-in themes (`selection` outranks `surface_hover`), so hover never impersonates keyboard ownership or a selection; rows that cannot be acted on never take hover.

### 8.7 Discoverability and recovery

Evaluate whether a user can identify the active session/resource, find the next
action, understand its consequence, and return to their work without knowing
the keymap in advance. Keyboard efficiency and learnability are separate
criteria; fewer keystrokes do not compensate for an action that cannot be found.

Under width pressure, preserve a meaningful next action or an obvious route to
its explanation. Avoid compressing every action into unexplained keys. Contextual
help, direct navigation, persistent hints, and alternate focus cycles are eligible
when a task comparison justifies their space and interaction cost. New controls
must work through the same authorization and state paths as existing ones.

Closing a modal, changing tabs, or leaving a temporary view should return the
user to a valid owner and preserve the draft, selection, and reading position.
Validate those return paths along with the forward action.

## 9. Current Component Defaults and Safeguards

Dimensions, placements, colours, exact glyphs, and component anatomy in this
section describe the current implementation. They can evolve under §1.2.
Requirements that protect truthful state, input ownership, authorization,
output safety, and existing work remain mandatory under §4.

### 9.1 StatusBar

Purpose: left-aligned brand and repository/branch identity (`widgets/status.rs`) in one quiet
row. The workspace panes receive the rows previously spent framing this identity.

Includes repository/branch (polled, TTL-cached) and the collapsed navigator's
session-attention chip. Model, effort, lifecycle and context pressure live in
the footer rather than being duplicated here.

Avoid duplicating file counts, task details or provider telemetry already shown elsewhere.

### 9.2 Block frame

- L1 frame: neutral `border` at every focus state (`theme::panel_border()`).
- L3 accents (single-rule regions only): the bottom panel's thick top rule,
  the composer's top edge, a focused search field's border.
- Active pane title: accent `>` marker and bold primary-text label, e.g. `> Terminal`
  (`theme::pane_title`). Inactive titles reserve the marker column and use
  secondary text. Modal titles remain bold accent (`theme::modal_title`).
- Modal bodies inset `MODAL_PAD_X` (2) horizontally; a titled modal adds one top
  row. A title never touches the rule it sits on — one space separates the
  label from the fill.
- No double borders except to express a modal or focused panel.

### 9.3 Footer

One or two rows (`widgets/footer.rs`); the second row appears for retained
background work or queued prompts.

- **Row 0 — configuration and turn state.** Configuration chips on the left, live activity on the right.
  - **Chips:** model (`provider/model`, prefix-stripped for display) and reasoning effort, with an optional notes chip for a nonempty or unsaved scratchpad. Reserve its width before fitting the model so the chip and lifecycle do not collide. They share the `Footer` focus block: `←`/`→` selects a configuration chip, `Enter` opens its picker. `Enter` still sends from the composer.
  - **Connection:** a disconnected provider has a textual qualifier beside its model so the state survives monochrome. Empty token totals stay hidden until usage is reported.
  - **Lifecycle:** turn state glyph plus short detail qualifier, styled secondary — severity lives in the glyph, never duplicated in colour.
  - **Context pressure:** a word, not a meter — `context` / `context high` / `context full`, coloured ok/warn/error at the 70% and 90% thresholds. (The old nine-cell shade-bar was removed: at typical single-digit percentages it read as stipple texture.)
  - **Hints:** the §6 hint grammar. Blocking dialogs take over the whole row; the footer's own per-chip hint and the task strip's session hint share the row with the chips. Focusing any other block — files, search, the panes — leaves the activity line alone.
  - **Working meter:** the lifecycle is a state *word* (`running`, `waiting`, `failed`) with one fixed-width `●` beside it, whose brightness pulses bright/dim while a turn runs (`throbber-widgets-tui` state, forge styling). The pulse changes brightness without moving a column; the state word remains meaningful without animation (§5.3).
  - When an approval pends, the row dims — it must not look interactive.
- **Row 1 — background activity (design A3, segmented count chips).** One `[glyph label]` chip per group — jobs, agents, queued prompts — each counts-only (`[⟳ jobs 2 · 1 need]`). Glyph and colour carry state (`⟳` running, `●` needs you, `✕` failed, `✓` done, `■` cancelled, `◆` agent, `⇥` queued); brackets are shared chrome. Counts include retained terminal tasks, matching the live filter rather than only the dock's expiring rows. Cancelled work is never counted as success. Clicking a category opens that parent's Jobs, Agents or Queue filter, including while a parent approval waits; dismissing returns to that decision. Per-item detail lives in the background strip and task view (§9.12).
  - **A completion is an observation, not a queued prompt.** Finishing a background task does not inject a user-role prompt. The result stays in the background strip and the operator attaches it to the composer explicitly (`i` on the selected task). Only approve-all — no human in the loop — auto-continues by enqueuing the result at the next turn boundary.

### 9.4 Chat transcript (primary work surface)

Before the first turn, a centered task group joins the heading, editable
prompt, complete starter prompts, and local hints. The group is capped at 84
columns; terminals below 24 rows show one starter, larger terminals show three.
Model/connection and lifecycle information remains in the footer and workspace
identity in the header. The start hides empty navigation until requested.
Typing retains the task composition until submission. Sidebar owns starter
selection (`↑↓`, `Enter`); selecting or clicking a starter appends to the draft
and returns focus to Composer without submitting it.

Hierarchy:

1. User request — left-aligned gutter treatment on the neutral `panel` ground;
   the `You` label carries authorship without a selection-strength band.
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
- The centered start is the first screen only; submitting a turn reveals the
  conversation. Navigation and inspection keep the retained draft.
- Height-only resizes invalidate cached transcript spacing when density changes.
- The transcript is borderless; when its content overflows the pane, a thin
  track (`│`) with a solid thumb (`▐`) marks position in the column's right
  padding, and the thumb turns into the accent `█` while the Sidebar block owns
  the keyboard. No overflow, no track; focus remains visible on the workspace tab.
- While a turn runs, the live turn line (`widgets/turn_line.rs`) names the phase and counts up from the current turn's start — including supervised sessions, where the clock is anchored on the actor's `Running` state, never on process uptime. No placeholder shimmer rows in the transcript — the pane stays empty until content arrives. Gated behind the busy debounce so instant turns never flash it.
- Keep zero-result searches neutral unless they block progress.
- Keep genuine failures visible: a terminal failure renders one error-styled row in the transcript (the durable `[forge.turn_failed]` marker stays hidden — it is model-facing state), so a failed turn never reads as an empty gap.
- Do not render a permanent progress narration stream.
- Distinct top-level block types (paragraph, list, quote, code, table) share one
  blank separator. Each block carries its trailing separator so the streaming
  split renderer agrees with a one-shot render. Airy density (§7.5.1) adds a
  rest after `You` / `Answer` labels and between distinct tool/activity groups;
  heading and code rests reuse the existing separator. Intentional blank lines
  inside code and explicit paragraph breaks remain intact.
- **Links carry destinations out of band, never inside the text.** An `OSC 8`
  sequence is zero columns wide on screen but its bytes are characters to every
  width measurement, so a destination written into a `Span` would be measured as
  columns and shift every wrapped row after it. Destinations therefore travel
  beside the rendered rows as column ranges and are attached to the buffer's
  cells after layout (`crates/forge-tui/src/links.rs`), with each cell's original
  display width preserved for Ratatui's buffer diff. Escape bytes must not make
  the diff skip adjacent text. Nothing in the visible text, and nothing in the
  copy path, ever contains an escape byte.
- **The underline is a promise.** A link renders as `link` plus underline only
  when its destination may actually be emitted — `http`/`https`, no control
  bytes, a real host. Every other scheme (`file:`, `mailto:`, `javascript:`, an
  injected `BEL`) renders as plain text with no underline, because an
  affordance that cannot be acted on is worse than none. The hue is what tells
  a link from every other underlined run — §6 keeps underline for links and
  explicit selected actions, and the focused footer chips already underline in
  `accent` — while the underline is what keeps the affordance when colour
  cannot (§5.4). `accent` stays out of the label: it means focus ("where am
  I"), not "this is clickable".
- **A URL is a link whether or not it was written as markdown.** A bare
  `http(s)://` URL in an answer, in tool output, or pasted into a prompt carries
  a destination, because the reader shown a URL is the reader who may want to
  open it, and asking them to retype it as `[label](url)` is not an affordance.
  Detection only proposes: `links::autolink_matches` scans, and
  `links::destination_for` still decides, so this widens what is recognized
  without widening what may be emitted. The workspace Editor tab's Markdown
  preview is the same prose under the same rule: it renders through the
  links-preserving renderer and marks its rows, so a link in a previewed file
  is the link it would be in an answer. **Current implementation limitations:**
  bare URLs in headings retain the plain-label treatment, and table rows discard
  cell destination metadata. These paths do not auto-link URLs. These are
  changeable defaults and renderer limitations: preserving destination metadata
  can make links eligible without relaxing destination validation.
- **The click belongs to the terminal.** The current implementation emits
  the sequence only for terminals known to render it, resolved once per process
  from `TERM_PROGRAM` with any multiplexer disqualifying (`tmux` before 3.4
  cannot forward hyperlinks, and `TERM_PROGRAM` names the outer terminal inside
  one either way). On anything else the underline stays and nothing else
  happens, which is why no hint row names a click: §4.5 forbids advertising a
  binding the current context cannot reach. The blanket multiplexer exclusion
  is a current capability-detection limitation. A verified capability check or
  an actionable in-app fallback may improve access without promising unsupported
  terminal behavior or admitting unsafe destinations.
- Lists, quotes, tables and fenced code share the prose left edge; only the code rail sits inside the block, never the whole block inset past its neighbours. A plan's explanation is separated from its `Plan · N of M done` header by one blank.
- Do not surround every message with a full-width box.

Response-structure treatment (editorial): inside an answer, the *skeleton*
is tinted so a long reply can be skimmed by shape before it is read — H1/H2
section labels preserve the author's case in bold `structure` over a hairline
`border_muted` rule, list markers take `structure`, and whole list blocks sit
on the `scan_band` ground while rendered tables zebra-stripe body rows with
`zebra_row`. Prose itself stays `text_primary` except for emphasis:
`**strong**` takes `md_strong` (orange) bold and `*emphasis*` takes
`md_emph` (greenish yellow) italic, so the load-bearing words pop out while
the rest of the paragraph stays calm. `accent` never appears in an answer,
and outcome colours stay reserved for result state.

Implementation: `crates/forge-tui/src/conversation.rs`.

Planning checklists use the lifecycle grammar: `[ ]` pending, `[>]` active
(orange and bold in the current themes), `[✓]` completed in neutral muted.
The active task has bold text; other tasks are muted. Wrapped text aligns
after the checkbox. The heading reports completed tasks, and the pinned
summary retains the count and current task when the checklist scrolls away.
Completion reflects the agent's reported plan status; tool evidence remains
below each step. **Current implementation limitation:** nesting stays flat
because the transcript schema supplies one level. A schema and renderer change
may support hierarchy when the task warrants it; the UI must not invent parent
relationships that the data does not contain. Only
the newest checklist renders: a superseded revision is removed, never recorded
as a second `Plan updated · N of M done` line beside it, so the checklist is
the single plan surface.

Focused tool approvals use the conversation area as an independent decision
surface. Its two-column origin aligns owner, invocation, working directory and
consequence. Literal wrapping preserves command whitespace and long tokens;
control and direction-changing characters are visible escapes. `PgUp` /
`PgDn` scroll details above pinned choices and hints. Compact surfaces show a
window of choices with its actual range; `↑` / `↓` reaches every grant scope.
The initial choice is **Don't run**. `Enter` chooses the highlighted action;
`Esc` declines; `Tab` leaves the decision and preserves the parent's reading
position and draft. The underlying inline request remains available in context.

A decision captures the presented request and selected session. An unpainted or
replaced request cannot consume a confirmation, and the session actor checks
the expected call ID before applying it. Explicit session-wide approve-all
retains its existing policy. Opening help or a picker gives that surface sole
keyboard and pointer ownership above any pending decision.

File, model and session pickers measure their results within capped terminal
budgets. Help has an independent viewport and a pinned close hint; its scroll
does not move the transcript, selection or draft beneath it.

### 9.5 Composer

- Spans the work surface under conversation and inspection, including temporary
  navigator views. It remains visible while an editor occupies the narrow body.
- `surface` background and one top rule, without an enclosing outline. The
  top edge takes `accent` when focused and `waiting_border` while
  an approval pends ("paused" look). Only attention states thicken the top
  rule — focus alone is a hue change, with the block caret as the monochrome
  signal. Waiting outranks focus colour.
- Multi-line growth uses three input rows at compact heights and four at
  comfortable heights. Complete drafts and pending paste payloads survive
  visual scrolling and resizing.
- Outbound messages queue above the composer as a strip; `Ctrl+↑`/`Ctrl+↓`
  select a prompt, and `Ctrl+Backspace` cancels its exact identity. If it was
  promoted or removed, cancellation refuses rather than affecting its
  successor. The Queue filter shows full text, owner and FIFO position. Plain
  `↑` returns the last queued message to an empty draft only. New typing during
  an asynchronous edit survives, and returned text is appended to its owning
  parent's draft without submitting or taking focus from a later inspection.

### 9.6 File tree

- Search is a two-row field (`/ ` prefix plus query, then a bottom rule carrying
  the mode-switch hint) at the neutral L2 step. One blank resting row separates
  it from the first tree row,
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
  row itself holds the keyboard (`↑` at the first row of a navigator list,
  `§8.3`), the tab outlines step to the L3 accent so the row reads as the thing
  being driven. The active tab keeps its ground and hue in that state, so the
  focus signal never stands in for the active-tab signal.
- The row's `+` cell shares the `Sessions` tab's right edge and the `Files`
  tab's left edge, so it reads as the `Sessions` tab's own create affordance.
  It is not a tab and never carries a tab's treatment: no `accent_soft` ground
  in any state, so the active-tab signal stays the tab bar's alone. Its glyph
  is muted by default, takes the hover ground plus a weight step under the
  pointer, and steps to accent and bold only while the row's cursor rests on
  it. Below the width the navigator column is laid out at the cell is dropped
  rather than drawn cramped, and it is then skipped as a cursor stop, so the
  frame and the keyboard can never disagree.
- Selected tree row uses the neutral `selection` token plus a `▌` bar in a
  dedicated gutter column; the inactive selection loses the background but
  keeps bold text and the bar. Content snippets retain their `>` pointer.
- The open file has a `•` in the ordinary tree's disclosure column and a bold
  name, independent of the selected row. Content-search file groups keep their
  expand/collapse marker and use the bold name. Diff and other workspace views
  carry no open-file dot.
  Names and the selected-path footer elide in the middle, preserving the tail
  and reserving room for Git status.
- Git markers come from the shared glyph set (§5.3): `M` `A` `D` `?` `!` `U`, bold and semantically coloured.
- Directory expansion uses `›` / `⌄` with 2-cell indentation; loading uses a
  one-cell `…` so names do not shift. The query match inside a name takes the
  shared `search_match` highlight (contiguous runs only — fuzzy-only matches stay plain).
- A filtered-to-nothing query reports `No matches for "<query>"`; an empty repository reports `This directory is empty`. The two states are never the same line.
- **Filename navigation and content search are separate.** `Ctrl+P` focuses
  `Search files...` with fuzzy workspace-path matching in Quick Open order.
  Name results synthesize their ancestors, not the workspace root. The field's
  bottom border advertises `Ctrl+Shift+F content`.
- `Ctrl+Shift+F` focuses `Find in files...`. Literal content matches use smart
  case (lowercase queries ignore case; an uppercase query is case-sensitive).
  Results group by workspace-relative file path, with `›` / `⌄` disclosure
  and a per-file line count; indented children show muted line numbers and
  snippets using `search_match`. Selected snippets retain the neutral selection
  style with underlined matches. `←` / `→` collapse / expand a file group, Enter
  on a header toggles it, and Enter or a click on a match opens its exact source
  line and column. A source result leaves rendered preview mode; unsaved buffers
  still require the existing save/discard confirmation when switching files.
  `Ctrl+P files` on the field's bottom border returns to filename navigation.
- Results are capped at 200 files or matching lines. The right side of the
  field shows `N files` or `N matches`, with `+` when more results were omitted;
  query text truncates around the count. Empty content queries show
  `Type to find in files`, never the ordinary explorer tree. A pending scan with
  no previous results says `Searching...`; stale results keep their own query's
  highlighting until the replacement arrives. Paste edits the focused query;
  Ctrl+U clears it, and Esc clears a non-empty query before leaving Search.
- A scan that could not run reports `Search unavailable`, never the
  no-matches line: a failed index says nothing about the query. The next
  keystroke retries the open.
- Gitignored paths are out of scope for Files search, matching what the `grep`
  tool sees. `.git` and `target` were the only exclusions before this change.
- Preserve the visible tree and its navigation state during a Git-only refresh (§4.12).
- Empty, loading, unavailable and failed states must be distinct.

### 9.7 Source viewer

- Code remains the visual focus; syntax highlighting is restrained (`syntax.*` palette).
- A rounded neutral frame carries the file path, middle-elided to fit, with an
  ASCII `*` unsaved marker outside the elision budget. There is no trailing
  "modified" word. Its title follows `theme::pane_title`, so keyboard ownership
  uses the same marker and label hierarchy as the conversation.
- The inner header is quiet language, line-count and preview/read-only metadata;
  one blank row separates it from source content when the body has room.
- Editing mode appears once in a neutral badge on the bottom row, followed by
  `Ln` / `Col` when they fit. Commands, search and editor messages take that row
  directly. Its text inset matches the composer.
- Search matches rank: active match, other matches, current line.
- Markdown, structured text, HTML, and log files open in a read-only rendered `PREVIEW` mode; `i` enters the editable source while `:preview` re-enters preview. `:edit` remains available for leaving preview or reloading the current file, and unsaved buffers stay intact.
- Line numbers stay muted; the caret identifies the exact editing position.
- The caret is painted only while the editor owns input; leaving the pane
  preserves its position and selection without showing a second input cursor.
  In monochrome, inverse video keeps the caret visible without a coloured ground.
- Horizontal scrolling must not detach markers from content.
- Binary and invalid-UTF-8 files are explicitly read-only.

### 9.8 Diff viewer

The patch pane's keys are the review's keys: they act whether the keyboard is
on the patch or on the Git tab's changed-file list, whose hint row this pane
draws (`FORGE-DESIGN §8.3`).

Conventional semantics with textual fallbacks:

- addition: `diff_add` + `+`
- removal: `diff_remove` + `-`
- context: body/muted
- hunk header: info + `@@`
- file header: primary text

Rules:

- Preserve old and new line numbers.
- The neutral frame carries the pane title at the shared two-column origin.
  Comfortable bodies retain one separator before the patch; compact bodies
  use it for evidence. Metadata, old/new line numbers and diff signs share
  one gutter without an extra indent.
- Prefer foreground/gutter markers over large background fills per changed line.
- The header names the selected file as the pane title (`> …`), with ASCII `+N -M` counts and the `N of M` position; the marker column comes off the elision budget so counts never clip.
- Reviewed files carry the `✓` tick; counts stay ASCII even in narrow panes.
- The hint row keeps labeled `? keys` and `Esc close` available. Under width
  pressure it drops secondary pairs before their verbs and elides a long
  branch/status tag to preserve those two actions. Hunk and file navigation
  remain visible when they fit; the full keymap stays reachable through `?`.
  A two-column gap separates hints from metadata. Routine workflow narration
  does not consume this action budget.
- Stale diff state must be explicit; binary/untracked/conflicted states must be truthful.
- `/diff` holds no content state itself — the pane reads live diff state so refreshes update in place.
- Three sources, cycled by `d` in the order the questions are asked: the
  **working tree** (everything against `HEAD`, the default and the only one the
  header does not name), the **index** (`git diff --cached` — exactly what a
  commit would take, which is the only way to review a partial stage), and the
  **last turn** (the transcript's own cards, not `git`, so it stays honest when
  the tree has moved on). The staged list is the same status snapshot filtered
  to paths with a staged side, and each entry's unstaged half is cleared before
  its marker is read: the marker comes from the more severe of the two sides, so
  leaving the working-tree side in place would label a file in the index list
  with a change that is not in it.
- `c` commits the index; it does not stage first. When nothing is staged it
  follows VS Code's smart commit instead of refusing: a prompt offers **Yes**
  (stage every change and commit), **Always** (same, and `[tui] smart_commit`
  is written so later commits skip the prompt), or **Cancel**. With
  `[tui] smart_commit` already on, `c` skips the prompt. `C` is the explicit
  commit-all — stage every change (`git add -A`, untracked included) and commit,
  never prompting — matching VS Code's separate "Commit All" action. A tree
  with no changes at all is the only refusal.
- The hint row's right-aligned tag carries the branch before the layout: the
  branch name with only the non-zero `↓behind ↑ahead`, or the `pull`/`push` in
  flight (`Pushing origin/main…`). A running operation outranks the branch, and
  the branch outranks the `split` layout note, which it then follows after a
  `·`. A branch the pane cannot read renders `branch ?` — an empty tag would
  read as "nothing to report", which is the one answer that is certainly wrong
  when the state is merely unknown. An in-progress merge outranks the plain
  branch name (`merging · 2 unresolved`): it is the state that decides the next
  key.
- Branch deletion and branch rename live in the picker, not on the view's key
  row: both act on *a* branch, and the picker is where a branch is chosen. `x`
  deletes behind a confirmation and `r` renames in one prefilled field, and both
  are inert in merge mode, where the list is answering "merge what?" rather than
  "which branch?". Deletion has **no force path in the UI**: `-d` refuses a
  branch whose commits are not merged anywhere, and that refusal is the feature —
  escalating to `-D` from a TUI is how commits get lost by accident. Rename is
  allowed on the branch `HEAD` is on; deletion is not. The asymmetry is
  deliberate: rename destroys nothing.
- A merge in progress also earns a full-width banner above the file list, in the
  warning severity colour rather than the focus accent — it is a state to act
  on, not the pane that owns the keyboard. One row, elided when narrow, and it
  yields to the patch on a pane too short for both, exactly as the hint row does.
  Conflict resolution deliberately reuses what is already bound: `o` opens the
  conflicted path, `s` stages the saved resolution, `c` commits the merge, and
  only the destructive exit is new — `a` aborts, and only behind a confirmation.
- These keys act from the Git tab's changed-file list as well as the patch
  (`FORGE-DESIGN §8.3`). With list focus, `↑`/`↓` move the file cursor and `↑`
  at the first row reaches the navigator tabs. With patch focus, `↑`/`↓` scroll
  the patch. `s`/`u` stage the selected side and `Esc` closes the tab from either
  pane. Shared review actions use the same keymap; an open search keeps its own
  input rather than triggering an action underneath it.

### 9.9 Terminal (BottomPanel)

- One interactive login shell per session; closing the panel never kills it.
- Focused presentation: thick top rule + `> Terminal` + accent title — legible without colour (shape carries it too). The title keeps one cell before the rule.
- The body shares the shared text origin (`TEXT_INSET`), like the composer and the queue strip.
- Once a shell exists, its emulator screen occupies the whole body: no shell
  label or activity rows displace its coordinates. ANSI attributes and Unicode
  cells are preserved; the caret styles its cell without erasing its character.
- Standard control keys (including Ctrl+E and Ctrl+N), modified arrows, Tab,
  bracketed paste and resize are forwarded to the shell. Shift+Tab leaves focus.
  Manual submissions are not rewritten with command-status scripts; explicit
  `!command` execution retains Forge's exit-status reporting.
- Shift+PageUp / Shift+PageDown and the focused panel's wheel browse the existing
  1,024-line scrollback. Typing returns to live output; history shows no caret.
  Alternate-screen programs keep Esc; Ctrl+Backtick still hides their panel.
- A dead shell is named in the title. Hide/reopen restarts it; ordinary hiding
  preserves the live shell and its environment. Panel height yields to the
  transcript and composer minimums on short terminals.

### 9.10 Status line and transient toast overlay

**Feedback state** (`widgets/feedback.rs`) — the app retains the latest message
and severity for compatibility, but the current draw path and layout reserve
zero rows for a separate status line. Setting feedback shows a toast rather
than inserting a row that moves the transcript. A separate status line is an
earlier presentation decision, not a currently displayed surface.

**Transient toast** — notices surface through `ratatui-toaster`
(`widgets/toasts.rs`), currently at the top-right and auto-expiring after 2s.
Toasts do not take focus or block input. Notices from another session use the
overlay without replacing the watched session's feedback state
(`SupervisorEvent::Attention`).

Position, duration, and whether a persistent notice surface is useful are
changeable defaults. The behavioral requirement is that an unresolved failure,
approval, or destructive consequence remains accessible after a transient
notice disappears (§4). Evaluate missed notices and access to their details
when changing notification behavior; a two-second toast is not proof that a
user has seen or understood a result.

### 9.11 Approve-all warning strip

- While a session's approve-all mode is on, one full-width row renders directly under the StatusBar: `⚠ SANDBOX OFF · approvals, filesystem and network unconfined · this session only · /approve-all to re-enable`.
- Error-coloured (`theme::danger()`), one row, full frame width, and part of the fixed chrome — the conversation scroll cannot move it off screen.
- It is the persistent record that the sandbox is off; it disappears the moment approve-all is disabled.

### 9.12 Background activity strip

What the footer's counts-only chip deliberately omits: one row per background
task, docked between the outbound-message queue and the composer across
the work surface (`layout.rs::regions.background`, built by
`tasks_strip.rs`, drawn by `widgets/background_strip.rs`).

Every rule here exists to protect something the operator is relying on.

- **Height comes from the available lines.** Comfortable docks show up to
  three queued prompts and three background tasks; below 28 frame rows each
  shows its header and one selected row. Only the selected task may add a
  detail row when its actual budget permits. Layout retains a conversation
  floor, and widgets clip within their allocated rectangles. Empty collections
  reserve no rows. Full request or prompt text stays available through
  inspection instead of expanding the dock over the composer.
- **The header tells the truth about truncation:** ` Background · 8 ` with
  `+N more` right-aligned when the row cap or available space hides
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
- **Selection** belongs to `(parent session, task ID)`, independent of current
  attention order; its viewport follows that identity when siblings change
  status. It uses the `>` pointer plus neutral `selection` ground, muted to
  secondary text when the Sidebar block does not own the keyboard (§8.5). The
  keys (`↑↓ x a d i`) predate this surface; before it the operator selected and
  acted on rows that nothing drew.
- **An empty registry draws nothing at all** — no header, no reserved gap — so
  an idle sidebar is unchanged, the same contract Row 1 of the footer keeps.

`/tasks` and footer count chips open a live, parent-scoped Jobs / Agents /
Queue view. `Tab` / `Shift+Tab` change the filter, existing selection keys move
within it, and `PgUp` / `PgDn`, `Home` / `End` or the wheel inspect full details above pinned
controls. Empty collections use a short empty-state card. Jobs explain that
they have no child session. Successful tasks expire from the dock after
60 seconds but remain inspectable while retained in the registry. Returning
preserves the parent's draft, caret and reading state. Opening a filter or
clicking a count never inserts a result or resolves a request.

Jobs show the exact command, parent owner, launch cwd, actual process exit
code and output from the executor's own readers. Each stream retains its
first 32 KiB for inspection, with a truncation notice, while the tool's
existing output budget stays unchanged. Controls and bidi formatting are
shown literally. Running output appears only after real bytes arrive;
cancelled jobs retain those bytes as partial output. Missing launch metadata
or an unobserved exit is stated explicitly. Restored tasks may show their
retained summary or failure without inventing execution metadata.

`i` explicitly appends an available result to the owning parent's draft and
places the caret at the end. Inserted evidence stays visible and editable in
the bounded, scrolling composer; the draft, attachments and queue survive.
Normal completion does not insert, submit, enqueue or change focus.

`x` on active work opens a named stop surface. It captures the parent and
task identity, defaults to Keep running (Keep waiting for blocked work), and
keeps choices pinned below independently scrollable details. Opening or
dismissing it never stops work. Confirmation requires the painted target and
rechecks its current lifecycle; a finished or changed target is refused.
Stopping retains available evidence. `x` on a terminal task hides only its
dock row; `/tasks` and retained execution evidence remain available, and no
checkout is removed.

### 9.13 Child session view

What the strip's two-line activity summary deliberately omits: the subagent's
full transcript. `→` on a background task row replaces the transcript pane with
that child's own session, read-only; `←`/`Esc` returns. Read-only by
construction — the view is a journal replay (`forge_core::session_messages` →
`forge_session::replayed_transcript`), never a second runtime, so there is no
second writer against a session the child still owns.

Every rule here exists to keep the operator oriented about whose session is on
screen.

- **The workspace tab says whose.** `Conversation ‹ explore` names the child; the
  parent's own `Conversation` comes back with it on `←`. `‹` reads as "drilled into",
  not a path — there is no parent-task lineage to draw, and inventing one
  would be a lie. The composer hint carries the same notice
  (`read-only · viewing explore · ← to return`), and the composer refuses input
  while the view is open.
- **The view refreshes from actual child evidence.** The poll tick re-reads
  the live child's journal and reads its final journal once after completion,
  including when the parent is supervised. Equal-length changes still count
  as changes. An unchanged replay keeps its revision and render cache. A reader
  who has scrolled back keeps the same logical reading row when evidence arrives.
- **The parent remains identifiable.** The header still describes the parent.
  Child inspection and the Agents filter identify the actual mode, workspace
  and branch. A writer names its separate worktree; a read-only scheduling mode
  uses the shared repository workspace. Mode is launch metadata, not a new tool
  permission policy. Unretained mode or branch information says unavailable.
  The reading pane uses four compact metadata/control rows with visibly elided
  paths and branches. `/tasks` and decisions retain their full literal details.
  Stop and decision cards avoid repeating the same agent/state metadata.
- **Return restores the parent.** `←`/`Esc` restores its reading/follow state,
  focus, text selection and retained draft. Parent progress is held separately
  while the child is shown; parent approvals and stream text cannot appear as
  the child's conversation. Return reports the child's actual current outcome.
  Switching parent sessions closes the child reader before saving view state.
  Ordinary transcript scrolling retains its existing cache buckets. Restoring
  a changed parent can materialize its full settled history once to locate the
  previously visible rows.
- **Child decisions require inspection.** `a` and `d` open the same full named
  request with **Don't run** selected. Literal invocation, owner, child, mode,
  cwd/workspace, branch and consequences scroll above pinned controls. Opening
  or returning makes no decision. Confirmation requires a painted request and
  binds parent, task, child execution and a unique pending-request identity.
  The producer rejects stale or repeated decisions even when a provider reuses
  a call ID. Explicit confirmation returns to that child's read-only view;
  parent drafts, queue and sibling work retain their own ownership.
- **Interruption retains work.** `x` confirms that named execution, defaulting
  to Keep waiting or Keep running; stop does not remove a checkout. `i` explicitly
  appends a terminal result to the owning parent's draft without sending it.
  Failed or cancelled children can hand off retained visible assistant findings,
  labelled partial and unverified. Hidden reasoning and tool messages are
  excluded. Missing findings are reported rather than manufactured. Result
  insertion does not integrate a branch or establish validation.
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
- In-app notices and the background strip remain available while focused.
  Terminal notifications reach the operator outside Forge.

### 9.15 Quit-all confirm
Quitting Forge has always stopped every session — process exit retires every
actor, so the sessions the operator is not looking at die with the one they
are. What was missing was the bill: neither `Ctrl+D` nor `/quit` said how many
turns, queued prompts, pending approvals, or unsaved buffers went with it.

- **Raised by the quit gate**, one path shared by an uncaptured `Ctrl+D`, `/quit` on the
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
- **Progress currently lives in compatibility state.** While the sweep runs,
  `status_state.message` counts down against the roster (`closing 3 sessions…`).
  The current layout does not paint that state as a separate status line
  (§9.10); exposing visible progress is eligible for improvement under §1.2.
  The quitting flag suppresses the per-session "removed with unsaved editor changes"
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

## 10. Current Theme Defaults and Behavioral Guarantees

Built-in themes ship as TOML in `crates/forge-tui/themes/` and compile into the binary:

| id | Name |
|---|---|
| `forge-dark` | Forge Dark (default) |
| `forge-light` | Forge Light |

Plus the pseudo-theme `system`, which follows the terminal's light/dark preference and re-resolves on OS appearance changes.

Users drop custom `.toml` themes into `~/.config/forge/themes/` or `.forge/themes/`; unparseable drop-ins are skipped (with diagnostics where the caller can show them) rather than breaking startup.

Rules:

- Themes are semantic token mappings against the full `ThemePalette`, not arbitrary plugin formats.
- The canvas supplies foreground and background colours together, so raw
  chrome text inherits the active palette rather than the terminal's unpaired
  foreground.
- Palette diagnostics use the current §5.1 hue guideline. Validate built-in and changed themes for readability, distinguishable state, and non-colour signals under §1.3; do not assume removed styling tests still enforce these properties.
- Bare `/theme` opens a bottom dock: `↑↓` live-previews against the real UI, `Enter` confirms, `Esc` restores the previous theme. `/theme <id>` applies immediately.
- Theme choice must not change runtime semantics, navigation, persistence or command availability.
- Theme policy applies to conversation presentation, chrome, activity and code rendering — never to terminal font selection.

### Current built-in palette choices

These values describe the shipped TOML palettes, not constraints on their
future evolution:

| Role | Forge Dark | Forge Light |
|---|---|---|
| Canvas / panel | `#14171B` / `#1D2229` | `#F6F7F9` / `#EBEFF4` |
| Primary / secondary text | `#EDF0F5` / `#B5BECB` | `#202936` / `#47576A` |
| Focus accent | `#86B5FF` | `#255CAA` |
| Success / warning / error | `#9BD1AD` / `#F3C575` / `#EE94A0` | `#246040` / `#745100` / `#A52D43` |
| Activity | `#E5BB80` | `#80511B` |
| Strong prose | `#FFA31D` | `#965300` |
| Emphasized prose / links | `#C7D96B` / `#4FC9DF` | `#5F7300` / `#00606E` |

`agent`, `tag`, and `structure` share the secondary neutral in both built-ins;
agent narration also uses italic styling. Prose emphasis tokens apply inside
model answers, not chrome, status, or code, and fall back to `text_primary` in
themes that omit them.

New themes should document their token choices and relevant legibility evidence.
Hue arithmetic can explain a choice, but does not establish usability by itself.

## 11. Historical Decisions and Reasons to Revisit Them

This record describes the current direction and its documented rationale.
It does not claim that each choice won a usability comparison. Earlier layouts
and controls remain eligible under §1.2 when a concrete task exposes a limitation.

| Current decision | Rationale and tradeoff | Revisit when |
|---|---|---|
| Conversation is primary on the left; inspection is on the right; the composer spans both | At 120 columns the old file-review layout left roughly 44 columns for conversation and composer; the new layout allocates 70 conversation columns and 118 composer columns. Below 116, views switch explicitly | Switching cost outweighs readable full-width content during narrow review |
| Sessions, Files, and repository Git share one navigator column | Avoid adding another content column; users switch to reach different resources | Switching cost or simultaneous session/resource monitoring outweighs the saved width |
| No permanent Inspector with Task/Context/Runtime tabs | Keep routine metadata out of the primary workspace | Repeated context or runtime inspection lacks a discoverable, efficient route |
| The bottom panel is an interactive terminal without Run/Diagnostics/Activity tabs | Preserve one shell surface; other results appear in their owning views | Comparing output, diagnostics, or activity requires unnecessary navigation or obscures the active task |
| Navigator becomes a temporary view when its persistent column cannot fit | Keep file, session and Git navigation reachable without constraining the conversation or hiding the composer | A task requires continuous navigation and content side by side at narrower widths |
| Hints are contextual and compact rather than a permanent shortcut manual | Save content rows; users unfamiliar with bindings can lose the action's meaning | Users cannot discover the next action or learn a compressed hint without memorizing the keymap |
| Sessions use the navigator list rather than a horizontal task strip or permanent switcher | Consolidate session attention and navigation; the list shares space with Files and Git | A session comparison or switching task demonstrates a clearer alternative with accurate ownership and state |
| Session pinning/slots and separate archive/cleanup/remove verbs remain internal | Keep the ordinary lifecycle interaction small; advanced organization is limited | A concrete workflow needs persistent ordering or clearer lifecycle control without risking worktree loss |
| The transcript is borderless; workspace tabs carry view identity and focus | Recover content rows and make the answer the primary surface; the focus marker and scrollbar preserve keyboard ownership | Reading or decision cards need stronger grouping without excessive chrome |

The current session list distinguishes idle, working, needs-you, queued, and
terminal outcomes (§7.7); it is not a fixed three-state model. Alternative
structures must preserve the information needed to act on those states.

Implementation limitations are also eligible for work: heading/table links,
terminal hyperlink capability detection, and nested plans are recorded in §9.4.
Do not convert a missing schema or renderer feature into a permanent UX ban.
Terminal font ownership remains a capability boundary (§4.15), regardless of
the layout chosen.

When a decision changes, replace its current-default description and retain a
short record of the problem, comparison, tradeoff, and reason for the new choice.
Use concrete evidence from the associated change; do not present a new default
as shipped until its implementation is verified.

## 12. Session Worktrees

The lifecycle details below describe the current implementation. Work
protection, correct session identity, explicit destructive decisions, and
cleanup only after writers have stopped are mandatory guarantees (§4).
Branch naming and the presentation of lifecycle actions can evolve without
weakening those guarantees.

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
- **Cleanup runs at the session's retirement boundary**, not when a key is
  pressed: `x` ("archive and clean up") requests retirement, and the removal
  executes where the session's actors have stopped and its workspace is being
  released (`supervisor.rs`). Archive and cleanup are one confirmed verb, not
  two steps.
- **Removal is clean-only.** A managed worktree holding uncommitted changes —
  staged, unstaged or untracked — is never deleted, and Forge never passes
  `--force`. Only the checkout is removed; the branch is always kept.
- **Removal is all-or-nothing per session.** The session's own checkout and
  every finished child agent's worktree are preflighted together. If any one of
  them is dirty, nothing is removed and the session becomes `retained` instead
  of `archived`.
- **`retained` is the fifth lifecycle state** (`control.rs::SessionLifecycle`)
  and a failure surface, not a resting state: the session retired but its
  checkout is still on disk. The navigator renders such a row as `● needs you`
  with the qualifier `cleanup blocked`, `x` on the row retries the removal, and
  because the preflight is all-or-nothing, `retained` always implies the
  session's own worktree is still present.
- Child worktrees are reclaimed at the same boundary, because that is where the
  session-lifetime follow-up capability ends. A child that itself spawned
  children reports only its own worktree; nested descendants are not covered.
- The primary session and attached worktrees are unchanged.
