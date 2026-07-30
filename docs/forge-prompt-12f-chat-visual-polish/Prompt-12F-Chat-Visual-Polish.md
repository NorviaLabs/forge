# Prompt 12F — Chat Visual Polish

**Recommended Codex model:** strongest available Codex coding model  
**Reasoning level:** Medium  
**Scope:** Chat presentation and rendering only  
**Architecture changes:** Prohibited

## Objective

Polish Forge’s implemented conversation UI so it feels calm, professional, readable, and deliberately terminal-native.

V3.1 and Prompts 12A–12E are already implemented. Do not reopen their architecture.

This phase is limited to:

- spacing
- typography
- visual hierarchy
- Markdown rendering
- semantic colour usage
- submitted user-message styling
- assistant-answer styling
- progress-row styling
- completed activity-row styling

---

## Primary visual reference

Use this exact screenshot as the visual reference:

```text
references/forge-chat-visual-target.png
```

Open and inspect the screenshot before changing code.

The screenshot communicates the target direction:

- plain transcript-style user messages
- minimal chrome
- restrained cyan/teal accent
- clean assistant prose
- readable Markdown lists
- subtle separators
- compact completed activity evidence
- answer-first hierarchy
- professional spacing
- no web-style answer cards

The screenshot is a visual direction, not a pixel-perfect layout specification.

Where the screenshot conflicts with the mandatory corrections in this prompt, this prompt is authoritative.

---

## Mandatory corrections to the reference screenshot

The reference image still contains two elements that must not appear in the completed state.

### 1. Completed progress must disappear

The screenshot shows progress content similar to:

```text
I’ll quickly inspect the repo layout and key docs, then summarise it.

Inspecting repository layout and docs…
```

These may appear only while the turn is actively running.

Once the final answer completes:

- remove the live progress row
- do not retain progress narration in the default transcript
- keep useful execution evidence only inside the existing activity group

The completed-turn order must be:

```text
User message
Assistant answer
Completed activity summary
```

### 2. The permanent shortcut footer must not return

The screenshot includes a footer similar to:

```text
Chat    ⌘K help    Ctrl+K cmd    ↑/↓ nav    Esc back
```

Do not implement or preserve this permanent shortcut strip.

V3.1 uses contextual hints only.

Existing transient hints may remain when an interaction genuinely requires them.

---

## Authoritative references

Read before modifying code:

- `FORGE-DESIGN.md`
- `FORGE-V3.1-INTERACTION-CONTRACT.md`
- The implemented Prompt 12A–12E code
- Existing conversation-rendering tests
- Existing theme tokens
- Existing Markdown renderer
- `references/forge-chat-visual-target.png`

Where an older mockup conflicts with V3.1 or the current design document, V3.1 and the current design document win.

---

## Hard scope boundary

This is a rendering-only pass.

### Allowed

- spacing and indentation
- wrapping behaviour
- separators and hairlines
- semantic theme-token usage
- user-message presentation
- assistant-answer presentation
- active-progress presentation
- completed activity-summary presentation
- Markdown headings and lists
- inline-code treatment
- code-block treatment
- link and file-reference styling
- muted metadata styling
- readable prose-width adjustments
- focused accessibility fixes directly caused by the visual changes

### Prohibited

Do not change:

- transcript persistence
- session formats
- event schemas
- semantic transcript grouping
- activity classification
- progress lifecycle
- agent output generation
- tool execution
- async-event semantics
- navigation
- workspace views
- Files behaviour
- Inspector behaviour
- Run behaviour
- approval behaviour
- mouse routing
- keyboard commands
- command palette
- theme selection architecture
- provider/model behaviour
- system prompts or response prompts
- repository indexing

Do not add:

- cards
- chips
- badges
- new panels
- new overlays
- new interaction states
- new expansion behaviour
- content-specific renderers
- a special crate-list renderer
- arbitrary truncation such as `… 6 more crates`
- font-family controls
- a new theme
- permanent hint or status rows

If the visual target appears to require a prohibited change, stop and report the mismatch rather than expanding scope.

---

## Target completed-turn appearance

The target hierarchy is:

```text
› Summarise the codebase

Forge is a Rust workspace for an AI coding-agent harness.

• forge-cli — CLI entrypoint; launches the full-screen TUI.
• forge-tui — Main terminal UI with conversation, Files, Run, and review.
• forge-core — Agent loop, sessions, tools, and governance.

At a high level: CLI → session → TUI → agent loop → tools/providers.

▸ Inspected repository · 3 operations
```

The exact model answer will vary.

The renderer must make arbitrary valid Markdown look good without relying on this exact content.

---

## User-message styling

Submitted user messages should look like transcript entries, not shell-history blocks.

Target:

```text
› Summarise the codebase
```

Rules:

- no full-width filled background
- no heavy border
- no box around each message
- one small muted prompt marker
- primary readable text
- wrapped lines align with the text, not under the marker
- clear but compact spacing before the assistant response
- consecutive user messages remain distinguishable without cards

Do not introduce a permanent `You` heading solely for this task.

---

## Assistant-answer styling

The final answer must be the dominant visual object.

Rules:

- render normal prose directly on the base canvas
- no full-width answer card
- no vertical bar beside every paragraph
- no grey slab behind every line or paragraph
- use bold sparingly for an opening summary or Markdown heading
- never bold full paragraphs
- use primary/off-white text for answer content
- use muted text only for genuinely secondary details
- keep paragraph spacing consistent
- place completed activity after the final answer

A small muted `Forge` speaker label may be retained only if it already exists cleanly. Do not create a new component solely for it.

---

## Active-progress styling

While the turn is running, show one subtle live row:

```text
● Inspecting repository layout…
```

Rules:

- no full-width box
- no red styling for routine activity
- no bold white narration strip
- use the existing progress/info semantic token
- keep it quieter than answer prose
- update the same row rather than appending new rows
- remove it once the final answer completes
- preserve Prompt 12B’s existing progress semantics

Do not alter progress lifecycle or grouping.

---

## Completed activity styling

Render completed activity after the answer.

Collapsed target:

```text
▸ Inspected repository · 3 operations
```

Rules:

- use the existing collapsed-by-default behaviour
- routine completed activity is neutral
- harmless zero-result searches must not render as red failures
- avoid a heavy full-width card
- use at most one subtle separator or hairline
- keep vertical height compact
- summary uses secondary text
- counts and metadata use muted text
- focused state remains unmistakable
- preserve existing keyboard and mouse expand/collapse behaviour

Expanded evidence may use more structure but must remain visually quieter than the final answer.

Do not change activity semantics, grouping, IDs, ordering, or expansion state.

---

## Markdown and list polish

Improve generic Markdown rendering. Do not teach the renderer about this particular codebase summary.

### Lists

- align wrapped continuation lines cleanly
- keep bullet indentation compact
- avoid large blank gaps between entries
- preserve nested-list hierarchy
- descriptions use primary or secondary prose colours
- use accent only for existing semantic links or interactive references
- do not colour every technical noun cyan
- do not force a two-column layout
- do not align descriptions using hard-coded character positions

### Inline code

- use the existing semantic inline-code role
- keep contrast clear but restrained
- avoid oversized pill-like backgrounds
- do not colour code cyan merely because it is code

### Headings

- create hierarchy using spacing and sparse bold
- avoid banner-like treatment
- avoid uppercase-only headings for ordinary response sections

### Code blocks

- use the existing subtle code surface
- preserve syntax highlighting
- do not redesign File, Diff, or Run views
- leave wrapping and horizontal-overflow semantics unchanged unless fixing a direct visual defect

### Links and file references

- preserve current activation behaviour
- use existing accent and focus semantics
- keep unfocused references readable
- do not underline every technical token

---

## Colour discipline

Use semantic theme tokens only.

Do not hard-code component colours.

For Forge Dark:

- primary answer text: warm off-white
- secondary prose: soft grey
- metadata: muted grey
- interactive links and file references: restrained cyan/teal
- progress: restrained info colour
- success: restrained green only for consequential outcomes
- warning: amber only for meaningful caution
- failure: red only for genuine failure or blockage

Reduce accent usage compared with the current implementation.

Do not colour crate names or technical identifiers unless they are already semantic links or interactive references.

The same renderer must continue working in existing Light, System, and ANSI modes through theme tokens.

Do not broadly retune those themes in this prompt.

---

## Spacing and readable width

Use whitespace rather than boxes to create hierarchy.

Rules:

- one controlled blank row between major transcript blocks
- avoid repeated empty rows
- keep user and assistant turns visually connected
- separate completed evidence slightly from the answer
- retain the existing readable prose-width policy
- do not centre conversation content
- allow code, tables, diffs, and raw output to use available width
- remain usable at `80×24`
- do not create enormous empty margins at normal widths

---

## Remove or restyle from the current implementation

- full-width user-message strips
- grey slabs behind normal assistant prose
- heavy left bars around ordinary answers
- red failure styling for neutral repository exploration
- retained progress narration in completed turns
- permanent shortcut footer/manual
- repeated borders around routine transcript blocks
- excessive cyan on non-interactive technical text
- generic follow-up offers presented as a special UI component

Do not delete answer content. Change presentation only.

---

## Screenshot fidelity rules

Match the visual spirit of:

```text
references/forge-chat-visual-target.png
```

Preserve:

- calm dark canvas
- answer-first hierarchy
- plain transcript treatment
- restrained accent
- subtle separators
- compact evidence
- clean list wrapping
- professional spacing

Do not reproduce:

- completed progress narration
- permanent footer
- content-specific column alignment
- exact answer wording
- exact dimensions or coordinates
- special styling for generic `I can also…` text
- pixel-perfect layouts

---

## Regression matrix

Test at minimum.

### Turn states

- user message only
- active progress
- streaming answer
- completed answer without activity
- completed answer with collapsed activity
- completed answer with expanded activity
- neutral zero-result search
- genuine activity failure
- interrupted response
- legacy transcript

### Content

- one short paragraph
- several paragraphs
- long bullet list
- wrapped bullets
- nested list
- inline code
- code block
- file links
- Markdown headings
- table
- long path
- plain text without Markdown

### Layouts

```text
80×24
100×30
120×40
160×50
240×60
```

### Themes

- Forge Dark
- Forge Light
- System
- ANSI fallback

### Interaction

- keyboard activity expansion
- mouse-assisted activity expansion
- focused and unfocused activity row
- scrolling during progress updates
- scroll anchoring when progress disappears

Prefer focused semantic snapshots and a small number of full-screen integration snapshots.

Do not create one giant brittle golden screenshot.

---

## Required visual comparison

Capture:

1. Current implementation before this prompt.
2. Completed answer with collapsed activity.
3. Active-progress state.
4. Expanded activity.
5. Light-theme equivalent.
6. `80×24`.

Compare the completed dark screenshot directly with:

```text
references/forge-chat-visual-target.png
```

The completion report must explain:

- where the result intentionally matches
- where terminal or runtime constraints require deviation
- whether completed progress was removed
- whether the permanent footer was removed

---

## Acceptance criteria

The task is complete when:

- submitted user messages no longer use full-width background strips
- ordinary assistant prose no longer renders as grey slabs or boxed rows
- the final answer is visually dominant
- active progress is subtle
- progress disappears after completion
- completed activity appears after the answer
- neutral activity is not styled as failure
- activity remains expandable through existing behaviour
- the permanent shortcut footer is absent
- Markdown lists wrap cleanly
- accent colour is reserved mainly for interaction
- no content-specific renderer was introduced
- no transcript or session schema changed
- no V3.1 navigation, execution, approval, or mouse behaviour changed
- Dark, Light, System, and ANSI remain usable
- all relevant tests pass

---

## Completion report

Report:

### Visual changes

- user-message treatment
- assistant-answer treatment
- progress treatment
- activity-row treatment
- Markdown/list changes
- spacing/readable-width changes
- semantic token changes

### Reference comparison

- matched elements
- intentional deviations
- confirmation that completed progress is removed
- confirmation that the permanent footer is removed

### Safety

- transcript persistence unchanged
- semantic grouping unchanged
- navigation unchanged
- Run unchanged
- approval unchanged
- mouse behaviour unchanged

### Validation

- test commands
- snapshot states
- size matrix
- theme matrix
- known visual limitations

### Files

List every changed file and why it changed.

Then stop.

Do not proceed to another redesign, theme expansion, or interaction phase.
