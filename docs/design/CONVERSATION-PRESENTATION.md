# Forge Conversation Presentation

This note documents the authoritative presentation hierarchy for transcript and
activity surfaces under the implemented V3.1 interaction model.

## Presentation Blocks

- `Answer`
- `Progress`
- `ActivityGroup`
- `Callout`
- `CodeBlock`
- `DiffBlock`
- `Metadata`

## Hierarchy

Default priority:

1. Final answer
2. Current blocking state
3. Current progress
4. Completed evidence
5. Metadata

Rules:

- The final answer is the dominant visual object.
- Active progress is one mutable unit, not a permanent stream.
- Completed tool activity is grouped and collapsed by default.
- Tool evidence remains available on demand.
- Zero-result searches are neutral unless they block the task.
- Genuine failures remain visible.
- Routine tool activity is quieter than the answer.
- No repeated full-width boxes around every message.
- No permanent progress narration.
- User messages and assistant answers remain readable without decorative chrome.

## Transcript Taxonomy

Presentation order:

1. User message
2. Assistant answer
3. Active progress
4. Completed activity group
5. Callout
6. Code or diff block
7. Metadata

## Typography

- Forge inherits the user's terminal font.
- Forge may use terminal attributes only: bold, dim, underline, foreground,
  and background.
- Bold is sparse.
- Italics are optional and must not be required.
- Prose has a preferred readable width.
- Code, tables, diffs, and raw output may use wider space.
- Decorative Unicode must have ASCII fallbacks.

## Activity Grouping

Collapsed activity:

```text
Assistant
The refactor is in place.

Activity · 3 items · collapsed
```

Expanded activity:

```text
Assistant
The refactor is in place.

Activity · 3 items
  Read FORGE-DESIGN.md
  Ran cargo test --workspace --all-targets --locked
  Updated app.rs
```

Streaming progress:

```text
Assistant
Working through the rename path…

Progress
  Reconciled source viewer
  Checking workspace state
```

Blocking failure:

```text
Assistant
The external rename notice was cleared by a refresh path.

Callout
  Failed: notice lost after workspace refresh
```

Code or diff block:

```text
diff --git a/crates/forge-tui/src/app.rs b/crates/forge-tui/src/app.rs
@@
+        self.source_viewer.notice = Some(notice);
```

## Theme Policy

Forge supports:

- `Forge Dark`
- `Forge Light`
- `System`
- `ANSI fallback`

Rules:

- Themes are semantic token mappings, not plugin formats.
- `Forge Dark` is the default when the terminal does not express a stronger
  system preference.
- `Forge Light` preserves the same hierarchy with lighter surfaces.
- `System` follows the terminal or platform default where available.
- `ANSI fallback` is the minimum guarantee for limited-color terminals.
- Theme choice must not change runtime semantics, navigation, persistence, or
  command availability.

## Responsive Behavior

- Preserve the current workspace first.
- Preserve critical status or current action.
- Collapse Files before the workspace.
- Remove secondary metadata before removing primary content.
- Truncate paths visually without mutating stored values.

Targets:

- `80×24`
- `120×40`
- `160×50`
- `240×60`
