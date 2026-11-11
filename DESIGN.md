---
name: Forge
description: A quiet three-column agent workbench where state is legible at a glance.
colors:
  bg: "#0f1115"
  bg-deep: "#0b0d10"
  surface: "#181b21"
  raised: "#1f232b"
  hover: "#272c36"
  border: "#2a2f38"
  border-strong: "#6c7686"
  text: "#e6e8eb"
  secondary: "#b6bcc7"
  muted: "#9aa2af"
  strong: "#f5f6f8"
  accent: "#6e9bff"
  accent-text: "#000000"
  accent-soft: "#1c2740"
  selection: "#27344f"
  link: "#7cc4d6"
  success: "#7fd69a"
  warning: "#e8b04a"
  error: "#f47c76"
  dot-waiting: "#e8b04a"
  add: "#14301f"
  remove: "#3a1a1d"
typography:
  title:
    fontFamily: "Geist Variable, Geist, -apple-system, BlinkMacSystemFont, SF Pro Text, Segoe UI, system-ui, sans-serif"
    fontSize: "16px"
    fontWeight: 600
    lineHeight: "24px"
    letterSpacing: "-0.01em"
  heading:
    fontFamily: "Geist Variable, Geist, -apple-system, BlinkMacSystemFont, SF Pro Text, Segoe UI, system-ui, sans-serif"
    fontSize: "13px"
    fontWeight: 600
    lineHeight: "20px"
  body:
    fontFamily: "Geist Variable, Geist, -apple-system, BlinkMacSystemFont, SF Pro Text, Segoe UI, system-ui, sans-serif"
    fontSize: "13px"
    fontWeight: 400
    lineHeight: "20px"
  label:
    fontFamily: "Geist Variable, Geist, -apple-system, BlinkMacSystemFont, SF Pro Text, Segoe UI, system-ui, sans-serif"
    fontSize: "12px"
    fontWeight: 500
    lineHeight: "20px"
  caption:
    fontFamily: "Geist Variable, Geist, -apple-system, BlinkMacSystemFont, SF Pro Text, Segoe UI, system-ui, sans-serif"
    fontSize: "11px"
    fontWeight: 400
    lineHeight: "16px"
  code:
    fontFamily: "Geist Mono Variable, Geist Mono, ui-monospace, SF Mono, Menlo, monospace"
    fontSize: "12px"
    fontWeight: 400
    lineHeight: "20px"
    letterSpacing: "0"
    fontFeature: "\"calt\", \"ss01\""
rounded:
  inline: "4px"
  sm: "6px"
  base: "8px"
spacing:
  "4": "4px"
  "8": "8px"
  "12": "12px"
  "16": "16px"
  "24": "24px"
  control: "28px"
  control-lg: "32px"
  bar: "44px"
  rail: "280px"
  column: "720px"
  changes: "400px"
components:
  button:
    backgroundColor: "{colors.raised}"
    textColor: "{colors.text}"
    typography: "{typography.label}"
    rounded: "{rounded.sm}"
    padding: "0 12px"
    height: "{spacing.control}"
  button-hover:
    backgroundColor: "{colors.hover}"
  button-active:
    backgroundColor: "{colors.border}"
  button-primary:
    backgroundColor: "{colors.accent}"
    textColor: "{colors.accent-text}"
    typography: "{typography.label}"
    rounded: "{rounded.sm}"
    padding: "0 12px"
    height: "{spacing.control}"
  button-ghost:
    backgroundColor: "transparent"
    textColor: "{colors.secondary}"
    rounded: "{rounded.sm}"
    padding: "0 12px"
    height: "{spacing.control}"
  button-ghost-hover:
    backgroundColor: "{colors.hover}"
    textColor: "{colors.text}"
  button-danger:
    backgroundColor: "{colors.raised}"
    textColor: "{colors.error}"
    rounded: "{rounded.sm}"
    height: "{spacing.control}"
  icon-button:
    backgroundColor: "transparent"
    textColor: "{colors.muted}"
    rounded: "{rounded.sm}"
    size: "{spacing.control}"
  icon-button-hover:
    backgroundColor: "{colors.hover}"
    textColor: "{colors.text}"
  field:
    backgroundColor: "{colors.bg}"
    textColor: "{colors.text}"
    typography: "{typography.body}"
    rounded: "{rounded.sm}"
    padding: "0 10px"
    height: "{spacing.control-lg}"
  chip:
    backgroundColor: "{colors.bg}"
    textColor: "{colors.muted}"
    typography: "{typography.caption}"
    rounded: "{rounded.sm}"
    padding: "0 8px"
    height: "22px"
  row:
    backgroundColor: "transparent"
    textColor: "{colors.text}"
    rounded: "{rounded.sm}"
    padding: "0 8px"
    height: "{spacing.control-lg}"
  row-hover:
    backgroundColor: "{colors.hover}"
  row-current:
    backgroundColor: "{colors.raised}"
  card:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.text}"
    rounded: "{rounded.base}"
    padding: "16px"
    width: "{spacing.column}"
  well:
    backgroundColor: "{colors.bg}"
    textColor: "{colors.text}"
    typography: "{typography.code}"
    rounded: "{rounded.sm}"
    padding: "8px 12px"
  dialog:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.text}"
    rounded: "{rounded.base}"
    padding: "16px"
  count-badge:
    backgroundColor: "{colors.accent-soft}"
    textColor: "{colors.accent}"
    typography: "{typography.caption}"
    height: "16px"
---

# Design System: Forge

## Overview

**Creative North Star: "The Quiet Workbench"**

Forge's desktop app is the category standard for an agent workbench, played straight: a dark, flat, three-column window held to the finish of Zed, Warp, Linear and the Conductor / Codex apps. Convention is the commitment. There is no novelty identity and no quirk; the quality is in the alignment, the contrast, and the restraint. The operator supervises several agent sessions at once, so the system spends its emphasis on one question only: which session needs me, and what do I decide.

Everything else stays quiet. Surfaces are flat and separated by 1px rules. One blue carries action, selection, focus and the running state. State is never a colour alone: it is a dot plus a word. Type moves in four steps and three weights. Code, commands and diffs sit in wells cut down to the ground colour, in Geist Mono, so the agent's work reads as material and the chrome reads as a frame.

Two things this must not be. It is not a chat app: no centred chat column with a conversation list and speech bubbles; a turn is a work-log block under a one-line header, and the agent's tool calls are joined rows, not messages. It is not an IDE: no activity bar, no editor-first frame; the conversation is the centre and the changes sit beside it.

Dark is the default; a light theme ships with the same token names and the same roles. The app is a Tauri desktop window (Svelte webview) on macOS and Linux.

**Key Characteristics:**
- Three columns under one 44px bar: sessions (280px), conversation (720px column, centred), changes (400px).
- Flat panels on the panel colour; code and diff wells on the ground colour; 1px rules between them.
- One blue for primary action, selection, focus and "running"; at most one accent fill in the working area.
- State is a dot plus a word: blue running, amber waiting, red failed, green completed, grey idle, hollow queued.
- Four type sizes (11, 12, 13, 16px), three weights (400, 500, 600), no uppercase, no tracking.
- 4px spacing grid; 28 and 32px controls; 6 and 8px corners.
- Motion is 120ms colour and opacity only; reduced motion removes all of it.

## Colors

Cool graphite neutrals with one blue. The frontmatter holds the dark theme, which is the default; the light theme uses the same token names with the values in the table below. The source of truth is the pair of built-in theme files (`crates/forge-desktop/themes/forge-dark.toml`, `forge-light.toml`), mapped to CSS custom properties at runtime; `app.css` carries the same palette as defaults.

### Primary
- **Workbench Blue** (`accent`): the one accent. Primary button fill, focus outline, the selected tab's underline, the running state dot and word, the text caret, inline text actions. Its label colour is `accent-text`, computed per theme for 4.5:1 on the fill.
- **Blue Wash** (`accent-soft`): the quiet tint behind count badges, the chosen reasoning level, and informational notices. Carries `accent` or `text` on top.
- **Selection Blue** (`selection`): text selection and search-hit highlight only. Not a row state.

### Secondary
- **Link Teal** (`link`): hyperlinks in model prose and plain anchors. Kept apart from the blue so a URL in a transcript never reads as a control or as focus.

### Tertiary
- **Success Green** (`success`): the completed dot, passing checks, added and new file words.
- **Signal Amber** (`warning`): text that says something waits on the operator: "waiting", "1 waiting", the risk phrase in an approval. Also tints the decision card's border (45% mixed into `border`).
- **Waiting Amber** (`dot-waiting`): the waiting state dot. Its own token because a mark needs 3:1, not text contrast; in dark it equals `warning`, in light it is a brighter amber than the warning text.
- **Failure Red** (`error`): failed, cancelled and interrupted dots, error text, destructive button labels, deleted and conflicted file words.
- **Diff Green / Diff Red** (`add`, `remove`): full-width line grounds in diffs. Line text stays `text`.

### Neutral
- **Ground** (`bg`): the window, the conversation column, and every code, command and diff well.
- **Deep Ground** (`bg-deep`): the terminal's background and the scrim behind overlays (70% over the window).
- **Panel** (`surface`): top bar, sessions rail, changes column, cards, dialogs.
- **Raised** (`raised`): default buttons, the current row, menus that float.
- **Hover** (`hover`): pointer hover on rows and quiet buttons.
- **Rule** (`border`): the 1px hairline between and around surfaces. Decorative weight; it is not relied on for contrast.
- **Control Edge** (`border-strong`): the border of text fields and choice marks, and the scrollbar thumb. Holds 3:1 on every ground.
- **Text** (`text`), **Secondary** (`secondary`), **Muted** (`muted`): three steps of text. Muted is the floor: it still holds 4.5:1 on every ground.
- **Strong** (`strong`): bold runs inside model prose only.

### Light theme

| Token | Light | Token | Light |
| --- | --- | --- | --- |
| `bg` | `#f7f8fa` | `accent` | `#2a57c8` |
| `bg-deep` | `#eceef2` | `accent-text` | `#ffffff` |
| `surface` | `#f0f2f5` | `accent-soft` | `#e3eafc` |
| `raised` | `#ffffff` | `selection` | `#d3defa` |
| `hover` | `#e3e6ec` | `link` | `#0f5f70` |
| `border` | `#dcdfe5` | `success` | `#1d6636` |
| `border-strong` | `#757f8f` | `warning` | `#754e00` |
| `text` | `#14171c` | `error` | `#b0251d` |
| `secondary` | `#434a56` | `dot-waiting` | `#ad7200` |
| `muted` | `#565e6c` | `add` | `#d9f2e0` |
| `strong` | `#0b0d10` | `remove` | `#f8dcdc` |

### Named Rules

**The One Blue Rule.** Blue means action, selection, focus, or a session that is running, and nothing else. In the working area there is one accent fill at a time: the decision card's first button while a decision is docked, otherwise Send. When a decision docks, Send steps back to a default button. Code is the one place colour means something else: inside the editor and the diff preview, the four lexical colours mark syntax, not state or action.

**The Dot Plus Word Rule.** State is always a dot and a word, never the dot alone and never colour alone. Running is blue, waiting is amber, failed / cancelled / interrupted is red, completed is green, idle is grey, queued is a hollow grey ring. When space runs out, the word outlasts everything beside it.

**The AA Floor Rule.** Every text token holds 4.5:1 on every ground (`bg`, `bg-deep`, `surface`, `raised`), and `border-strong` holds 3:1. Tests in `crates/forge-desktop/src/themes.rs` enforce this for both built-in themes. A colour that cannot meet the floor is changed, not excused; this is why the blue is `#6e9bff` and why muted is as light as it is.

**The Ground Ladder Rule.** Grounds step in one direction: ground, panel, raised, hover. A hovered row never reads as the current one: hover is a fill alone; current is `raised` with a 1px rule.

## Typography

**Display Font:** none. There is no display face; the largest text is a 16px title.
**Body Font:** Geist Variable (with -apple-system, SF Pro Text, Segoe UI, system-ui), bundled with the app.
**Label/Mono Font:** Geist Mono Variable (with ui-monospace, SF Mono, Menlo), with `calt` and `ss01` on.

**Character:** One neutral grotesque and its mono sibling, set small and even. Hierarchy comes from weight and from the three text colours, not from size.

### Hierarchy
- **Title** (600, 16px, 24px line, -0.01em): dialog and empty-state headings, the wordmark, h1 and h2 in model prose. The only step above body.
- **Heading** (600, 13px, 20px line): the session title in the bar, panel headings ("Changes"), the decision card's title, h3 and below in prose.
- **Body** (400, 13px, 20px line): conversation prose, the composer, session names, dialog copy. The transcript column is capped at 720px.
- **Label** (500 on controls, 400 in running text; 12px, 18 to 20px line): buttons, tabs, tool rows, secondary notes, the speaker line of a turn (600).
- **Caption** (400, 11px, 16px line): state words, counts, chips, directory paths, keyboard hints, group labels. Sentence case, never tracked out.
- **Code** (Geist Mono, 400, 12px, 20px line): commands, diffs, tool names and output, file names and paths, branch chips. The embedded terminal uses the same face at 12px.

### Named Rules

**The Four Steps Rule.** Type is 11, 12, 13 or 16px. No other size exists; a new need picks one of the four and changes weight or colour instead.

**The Three Weights Rule.** 400 for reading, 500 for controls and the current item, 600 for headings and names. Nothing is set in uppercase and nothing is letter-spaced apart from the -0.01em on titles.

**The Mono Means Material Rule.** Geist Mono marks things that come from the machine: a path, a command, a tool, a diff, a branch. Interface words are never set in mono.

Numerals are tabular everywhere (`font-variant-numeric: tabular-nums` on the body), so counts and diff stats do not shift.

## Layout

A fixed frame: a 44px top bar across the window, then three columns, `280px | 1fr | 400px`, each a full-height flex column that scrolls on its own. The window itself never scrolls.

- **Bar** (44px, panel colour, rule below): at the left, after the window buttons, a **Session | Files | GitHub** switch, in the same place on every screen (GitHub only for a Git repository; Files carries its changed count); then the session's state dot, name and state word, and the plan's current step ("Step 2 of 3 · …", opens the plan); at the right two 32px icon buttons: Terminal (pressed while its drawer is open) and More. The area over the rail is at least 280px and grows to fit the switch. The bar is also the window's title bar: it is the drag region, a double-click zooms the window, and on macOS (overlay title bar) its content starts 84px in. Linux keeps its native title bar above it.
- **Rail** (280px, panel colour, rule at right): a "needs you" group when anything waits, then sessions grouped under their project and indented 12px, ending with a "+ New session" row. No search field: the command palette (⌘K) finds sessions.
- **Centre** (ground colour): one centred 720px column shared by the transcript, the strips above the composer, the decision card and the composer, with 24px gutters when the column is narrower.
- **Side panel** (400px, panel colour, rule at left): two views switched from its header, Changes and Activity. Changes: This turn and Uncommitted tabs, the file list (at most 34% of the height), and a diff preview on the ground colour; its empty states carry a Browse all files link. Activity: the plan, background shell jobs and child agents (with their approvals), and the queue; its tab carries a count and a waiting dot when a child needs approval.
- **Footer** (28px, panel colour, rule above, 11px muted text): at the left the project, the branch with ahead and behind (opens Files) and the uncommitted count (opens Changes); at the right "needs you" in amber, the run slots, the model (opens the picker), the thinking switch for models that honour it, "Approve all" in amber when on, and the context percentage (opens details; amber from 70%). Items that act underline on hover.

Spacing sits on a 4px grid and in practice uses five values: 4, 8, 12, 16 and 24px. 8px is the default gap; 12px is the inside of a compact card and the gap between its parts; 16px is the inside of a card or dialog and a panel's left inset; 24px is the transcript's outer padding. Controls are 28px tall (rows of controls, buttons, icon buttons) or 32px (text fields, session rows, palette rows); compact buttons inside a 32px strip drop to 24px.

Responsive behaviour (the window can be resized or tiled):
- **Below 1180px:** the changes column is dropped and the conversation takes its room; Files in the bar still reaches every change. The model name leaves the bar.
- **Below 760px:** one column. The rail becomes an overlay opened from the bar; the branch chip leaves the bar, and the state word stays.
- **Focus screens** (Files, the terminal at full height): the rail and the changes column are hidden and the screen spans the window under the same bar. On Files, a single open document is named in a chip in the bar with its close button beside it; the document switcher (a tab strip) appears only when two or more documents are open.

Overlays: dialogs that ask for a decision are centred (26rem; wider for the child-agent view) over a scrim; the Session panel is not one of them: it is a popover anchored under its button with no scrim, put away by a click anywhere else or by Escape; the command palette is 560px wide, 12vh from the top; the slash-command menu opens upward from the composer.

## Elevation & Depth

Flat. Depth at rest is tonal: ground, panel, raised, with 1px rules where two surfaces meet. No resting surface casts a shadow, and nothing uses blur, glass or gradients.

### Shadow Vocabulary
- **Overlay** (`box-shadow: 0 12px 32px -8px rgba(0, 0, 0, 0.55), 0 2px 6px rgba(0, 0, 0, 0.3)`; in light `0 12px 32px -8px rgba(20, 23, 28, 0.22), 0 2px 6px rgba(20, 23, 28, 0.08)`): the one shadow. Dialogs, the command palette, pickers, popover menus, and the rail when it is an overlay.
- **Composer focus ring** (`box-shadow: 0 0 0 3px` of `accent` at 32%): added to the composer card's accent border while its textarea has focus. It accompanies a border change; it is not the only indicator.
- **Scrim** (`bg-deep` at 70%): the dim behind modal overlays. Only an overlay that needs interruption or protected focus gets one.

### Named Rules

**The Overlays Only Rule.** A shadow means "this floats above the window and will go away". If a surface stays, it gets a rule and a ground, never a shadow.

**The Real Outline Rule.** Focus is a 2px solid `accent` outline (offset 2px; -2px inside rows and wells, -1px on fields), not a glow, so it survives forced-colours modes. Where a container shows focus for its child (composer, palette), its own border turns `accent`.

## Shapes

Softly squared. Two radii do nearly all the work: 6px for controls, rows, chips and inner wells; 8px for cards, dialogs, menus and joined tool groups. Small inline marks no taller than 16px (inline code, keyboard keys, the multi-choice box) use 4px. Circles are reserved for state dots (8px) and the single-choice mark; the scrollbar thumb is the only pill.

Borders are 1px, always. Emphasis never comes from a thicker border; the two exceptions are structural, not decorative: the 2px underline of the selected tab and the 2px focus outline. Joined tool rows share one container: the radius appears only at the group's two ends and the rows between are divided by a single rule. A quoted block or a reasoning excerpt is marked by a 1px rule at its left in the rule colour.

Icons are a small fixed set drawn in one style: 24px grid, 1.7px stroke, round caps and joins, no fill, `currentColor`, sized 13 to 15px in use.

## Components

Quiet and exact: components are told apart by ground and rule, and only one of them is ever loud.

### Buttons
- **Shape:** 28px tall, gently squared (6px), 12px side padding, 6px gap to an icon, 12px label at 500. Inside 32px strips the same button is 24px tall with 8px padding.
- **Default:** `raised` fill, 1px `border`, `text` label. Hover moves to `hover` and strengthens the border; pressed drops to `border` as the fill; disabled is `muted` at 60%.
- **Primary:** `accent` fill and border, `accent-text` label at 600. Hover mixes 14% of `text` into the fill. One per working area (The One Blue Rule).
- **Ghost:** no fill or border, `secondary` label; hover gives it the `hover` fill and `text`. Used for bar buttons and quiet panel actions.
- **Danger:** a default button with an `error` label. It is never filled red.
- **Icon button:** 28px square, no border, `muted` icon; hover gives `hover` fill and `text`.
- **Text action:** `accent`, underlined (3px offset), inline in a sentence ("Open the full diff").
- **Motion:** background, border and colour over 120ms `cubic-bezier(0.2, 0, 0, 1)`.

### Chips
- **Style:** 22px tall, 6px corners, `bg` fill, 1px `border`, `muted` 11px text; mono when it names a branch or a path. Informational, not a control.
- **Count badge:** 16px tall, `accent-soft` fill, `accent` 11px text at 600. For plain counts beside a label.

### Cards / Containers
- **Corner Style:** 8px.
- **Background:** `surface`, sitting on the `bg` centre column.
- **Shadow Strategy:** none (see Elevation & Depth).
- **Border:** 1px `border`.
- **Internal Padding:** 16px for decision cards, 12px for the composer, 12px 16px for the operator's message.
- **Wells:** anything the machine wrote inside a card (a command, a code block) is a well: `bg` fill, 1px `border`, 6px corners, 8px 12px padding, mono 12/20. No box sits inside a box at the same ground.

### Inputs / Fields
- **Style:** 32px tall, `bg` fill, 1px `border-strong`, 6px corners, 10px side padding, `muted` placeholder.
- **Focus:** border turns `accent` and a 2px `accent` outline sits at -1px. The caret is `accent`.
- **Composer:** the exception. The textarea is bare inside a 12px-padded card, and the card carries the focus (accent border plus the ring). Its foot holds attachments, the model (mono, opens the model picker) at the left, then Stop and Send at the right. No key hints and no context meter: context is shown as a note above the composer only from 70% full, with Compact and Details.

### Navigation
- **Top bar:** described under Layout. Ghost buttons with a 13px icon; a button whose target holds something waiting takes an amber label and an amber-tinted border.
- **Session rail rows:** 32px, 6px corners, a state dot, the name at 13px, the state word at 11px in the state's text colour. Hover is the `hover` fill; the current session is `raised` with a 1px rule and its name at 500. Project headings are 28px rows at 11px, 600, with a dot (hollow when the project is closed) and a muted summary at the right. Row actions lie over the right edge of the selected row only, so showing them moves nothing.
- **Underline tabs:** 32px, 12px text, 16px apart, `muted` at rest; the selected tab is `text` at 500 with a 2px `accent` underline. A muted 11px count follows each label. Used for switching views of one list.
- **Command palette:** a 560px dialog; a 44px search row with no field chrome, groups labelled in 11px muted, 32px result rows, the active row on `hover`. The palette's own edge turns `accent` while focused.

### State Mark
An 8px dot followed by a word. `running` and `streaming` are `accent`; `waiting` is `dot-waiting`; `completed` is `success`; `failed`, `cancelled` and `interrupted` are `error`; anything else is `muted`; `queued` is a 1.5px `muted` ring. The dot is decorative to assistive technology; the word is the state.

### Decision Card (signature)
The docked approval or question, in the conversation column directly above the composer. A `surface` card with an amber-tinted 1px border (45% `warning` into `border`), 16px padding and 12px between parts: a waiting dot and a 13px/600 title, the tool name in mono caption at the right; the command in a well; a 12px reason in `secondary` whose risk phrase is `warning` at 500; then the actions in one row, first the primary, then default buttons, then danger, with the two shortcuts as 11px key hints at the right: the platform's modifier with Shift and A for the first allow option, with Shift and D to deny. The shortcuts work from anywhere in the window and take the same path as a click, so a privileged option still stops at its confirmation. A second confirmation is an inline row of the same card under a rule, not a second card. Question options are 32px rows with a ring (one answer) or a box (several) as the choice mark; the chosen row is `raised` with a rule.

### Tool Rows (signature)
The agent's tool calls are a log, not messages: 24px rows in one bordered `surface` container, the tool name in mono 12px in a 96px column and its result in `muted` beside it. Longer output continues in the same column, mono and muted. Neighbouring calls join into one container.

**The One Scroll Rule.** Nothing inside the transcript scrolls on its own. Long output (more than 8 lines) and long reasoning (more than 12) show a clipped preview that fades out at the bottom, with a link under it: "Show all N lines", or "Show the full output" when the backend cut it short, which fetches the rest; open, it reads "Show less". Horizontal scroll inside a code block is the one exception.

### Changes List and Diff Preview (signature)
File rows are 28px: file name in mono 12px at 500, its directory in mono 11px `muted`, and a status word at the right (added and new in `success`, deleted and conflict in `error`, otherwise `muted`). Selecting a row previews its diff below without leaving the conversation: a 32px header with the path in mono, a `+n −n` stat, and Open; then the diff on `bg`, mono 12/20, with the sign in its own 16px `muted` column, added and removed lines on `add` and `remove` across the full width, and hunk headers on `surface` in `muted`. Wrapped lines continue at their own indent. Code takes the editor's four lexical colours: keywords `accent`, strings `success`, numbers `warning`, comments `muted`.

### Transcript
Each turn is a block under a one-line speaker header (12px, 600). The operator's message is a bordered `surface` card; the agent's reply is bare prose on the ground at 13/20. Inline code has a 1px rule, 4px corners and the `surface` ground; code blocks are wells. While a reply streams, an 8px `accent` square blinks below it (static under reduced motion).

## Do's and Don'ts

### Do:
- **Do** use the tokens by name (`--bg`, `--surface`, `--raised`, `--hover`, `--border`, `--border-strong`, `--text`, `--secondary`, `--muted`, `--accent`, ...). Both themes and any drop-in theme arrive through them; a literal colour in a component breaks all three.
- **Do** pair every state dot with its word, and keep the word when space runs out.
- **Do** keep one accent fill in the working area: the decision's first button, else Send.
- **Do** put machine output (commands, diffs, code, tool results) in a `bg` well in Geist Mono, and panels and cards on `surface`.
- **Do** stay on the four type sizes (11, 12, 13, 16px), the three weights (400, 500, 600) and the 4px grid (4, 8, 12, 16, 24).
- **Do** separate surfaces with a 1px `border` rule, and use `border-strong` for anything that must be seen as an edge (fields, choice marks).
- **Do** hold every text colour to 4.5:1 on every ground it can land on, and every mark to 3:1, in both themes. Extend the theme tests when a token is added.
- **Do** show focus as a 2px `accent` outline, and make every action reachable from the keyboard.
- **Do** label a shortcut for the platform it runs on ("⌘K" on macOS, "Ctrl+K" elsewhere), through `ui/src/keys.js`; never print both.
- **Do** keep motion to 120ms colour and opacity transitions, and remove it under `prefers-reduced-motion`.
- **Do** give each action one home. Session, Files and GitHub are the bar's view switch; Terminal is in the top bar; New session ends the session list in the rail; Session, Context and usage, Activity, Settings and Help live in the More menu, which holds extras only (labels without ellipses); GitHub is a view beside Session and Files; settings (theme, sound, providers) live in Settings, not in Help.
- **Do** use a link, not a button, for going somewhere or seeing more ("Browse all files", "View the queue", "Read the transcript", "Details"). Anything that decides or changes state stays a button.
- **Do** use an icon alone only for a frequent action whose meaning is conventional (files, terminal, add, fetch, pull, push, expand), always with a tooltip and an accessible label; decisions and rare actions keep their words.
- **Do** say what a waiting session needs when no card does: a note above the composer names the reply, setting or outside action it waits for.
- **Do** write labels in sentence case that say exactly what happens ("Allow once", "Browse files", "The worktree is clean").

### Don't:
- **Don't** make it read as a chat app: no centred chat column with a conversation list, no left and right speech bubbles, no avatars.
- **Don't** make it read as an IDE: no activity bar, no editor-first frame around the conversation, and no tab strip for a single document.
- **Don't** put a scrim behind a panel that only shows facts and routine actions; anchor it to its button instead.
- **Don't** put a shadow on a resting surface, and don't use gradients, blur or glass anywhere.
- **Don't** use blue for anything that is not action, selection, focus or the running state, and don't use a second accent hue for emphasis.
- **Don't** carry state by colour alone, and don't use the warning text colour for a state dot; dots use `dot-waiting`.
- **Don't** fill a button red. Destructive actions are a default button with an `error` label.
- **Don't** add a type size, an uppercase label, or letter-spacing.
- **Don't** thicken a border for emphasis or mark a card with a coloured side stripe; the decision card's tint is a full 1px border.
- **Don't** nest a card in a card on the same ground; inside a card, the next level is a well or a rule.
- **Don't** add novelty: no mascot, no illustration, no decorative motion, no "AI" sparkle. Convention is the commitment.
