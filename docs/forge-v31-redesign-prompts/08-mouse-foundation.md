# Prompt 08 — Add the Mouse Foundation: Click, Focus, Selection, Controls, and Scroll

**Recommended Codex model:** GPT-5.5  
**Reasoning level:** High  
**Visible UX change:** Input expansion only

## Objective

Add a narrow, command-driven mouse foundation without double-click, dragging, hover, context menus, or mouse-only capabilities.


## Authoritative references

Read these before making changes:

- `FORGE-V3.1-INTERACTION-CONTRACT.md`
- `FORGE-DESIGN.md`
- Existing architecture and test documentation

Where an older mockup conflicts with the V3.1 Interaction Contract, the contract is authoritative.

## Global safety rules

- Preserve all behaviour outside this prompt’s explicit scope.
- Do not begin the next phase.
- Do not perform opportunistic redesigns or unrelated refactors.
- Do not replace working architecture merely to match suggested type names.
- Reuse existing abstractions when they already express the required semantics.
- Keep Forge buildable and usable at the end of this phase.
- Run focused tests while iterating and the relevant full test suite before completion.
- If the repository materially differs from the assumptions in this prompt, stop and report the mismatch before forcing the design.
- Record every changed file and why it changed.


## Preconditions

- Semantic command dispatcher is authoritative.
- Simplified shell and responsive layout are stable.
- Terminal lifecycle ownership is documented and tested.

## Terminal lifecycle

Implement configurable mouse capture:

```text
mouse_capture = true | false
```

Mouse-disabled mode must remain fully functional.

Capture must be disabled during:

- Normal shutdown.
- Panic/error restoration paths.
- External-editor launch.
- Terminal handoff to interactive child processes.
- Any path that relinquishes Forge’s terminal ownership.

Restore capture only after Forge safely regains ownership.

## Semantic hit regions

During rendering, register semantic hit regions containing:

```text
area
semantic target
frame generation
z-order/overlay priority
```

Examples:

```text
Pane(target)
FileEntry(path)
DirectoryChevron(path)
ActivitySummary(action)
VisibleControl(command)
Composer
OverlayAction(action)
```

Resolve mouse coordinates against the latest completed frame.

Do not place business logic in coordinate checks inside widgets.

## Supported interactions

- Left-click pane: focus.
- Left-click row: select.
- Left-click directory chevron: toggle directory.
- Left-click visible control/button: emit its semantic command once.
- Left-click activity summary: inspect/open.
- Left-click Composer: focus it; cursor placement only if existing text input supports it reliably.
- Wheel: scroll the pane beneath the pointer without changing keyboard focus.
- Overlay targets take precedence and block the workspace beneath.

## Explicitly unsupported

Do not process:

- Double-click activation.
- Drag.
- Right-click.
- Middle-click.
- Hover/movement state.
- Context menus.
- Pane resizing.
- In-app text selection.
- Multi-selection.
- Mouse-only functionality.

## Stale-target safety

- Each render increments or updates a frame generation.
- Mouse events resolve only against current hit regions.
- Missing/stale targets are ignored safely.
- Resize, overlay changes, and list mutation invalidate old regions.
- A stale click must never execute a command for the item previously at those coordinates.

## Tests

Cover:

- Click pane focuses it.
- Click row selects it.
- Click chevron toggles.
- Click visible control emits one command.
- Wheel scrolls hovered pane without changing focus.
- Click behind approval overlay does nothing.
- Mouse disabled preserves keyboard workflow.
- Stale hit region after resize is ignored.
- Stale row after list mutation is ignored.
- External editor disables/restores capture.
- Shutdown restores terminal state.
- Unsupported buttons/events do nothing safely.
- `80×24` hit regions remain accurate.

## Prohibited changes

Do not:

- Add double-click.
- Add drag, hover, or context menus.
- Change semantic command behaviour.
- Create mouse-only actions.
- Make mouse capture impossible to disable.
- Redesign the shell again.

## Acceptance criteria

- Mouse input is an adapter to semantic commands.
- Keyboard-only mode is complete.
- Hovered-pane scrolling works without focus change.
- Overlays block underlying targets.
- Terminal restoration is reliable.
- Tests pass across supported platforms/terminals where practical.

## Completion report

Report:

- Capture lifecycle.
- Hit-region architecture.
- Supported targets.
- Unsupported events.
- Tests and terminals/platforms checked.
- All changed files.

Then stop.
