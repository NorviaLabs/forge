# Prompt 00 — Audit and Characterise the Existing Interaction System

**Recommended Codex model:** GPT-5.5  
**Reasoning level:** High  
**Production behaviour changes:** None

## Objective

Create a reliable map of the current Forge interaction architecture and lock down the behaviours that must survive the redesign.

This phase exists to prevent a visual refactor from accidentally breaking focus, Composer input, navigation, session restoration, Run, external-editor handling, terminal restoration, or existing file workflows.


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


## Scope

Inspect and document:

- Application shell and layout ownership.
- Current Chat, Editor, and Diff tab implementation.
- Workspace focus and `FocusTarget`.
- Composer input ownership and type-to-compose.
- Files pane state, selection, expansion, scrolling, and persistence.
- Inspector state and rendering.
- Bottom panel state, tabs, height, and focus.
- Run/Validation integration and navigation.
- Async agent, tool, and Run events.
- Current approval and confirmation flow.
- Existing command/keybinding abstractions.
- Terminal lifecycle and external-editor transitions.
- Current mouse dependencies or event handling, if any.
- Responsive layout behaviour.
- Session and repository-scoped persisted UI state.
- Existing unit, integration, snapshot, and terminal tests.

## Required deliverable

Create:

```text
docs/design/V31-REDESIGN-AUDIT.md
```

The audit must include:

1. Current state model.
2. Current event-routing path.
3. Current layout ownership.
4. Current navigation behaviour.
5. Current focus precedence.
6. State duplicated across multiple surfaces.
7. Components that can be reused.
8. Components requiring adapters.
9. Components likely to be removed later.
10. Migration risks and dependencies.
11. A proposed phase-by-phase code migration map aligned with this prompt sequence.

## Characterisation tests

Add or strengthen tests for current behaviour without changing visible UX:

- Chat, Editor, and Diff can be reached using current controls.
- Files selection and expansion remain stable.
- Composer owns printable input when focused.
- Type-to-compose works only for otherwise unhandled printable input.
- Modal/transient input wins over background panes.
- Bottom-panel focus and return behaviour.
- External editor restores Forge correctly.
- Terminal restoration after normal exit and failure paths already supported.
- Run/Validation output does not corrupt focus.
- Session restoration preserves currently supported UI state.
- `80×24` renders without panic.

Prefer behavioural tests over brittle pixel snapshots.

## Prohibited changes

Do not:

- Introduce `WorkspaceView`.
- Remove tabs.
- change visible layout.
- Add mouse support.
- Alter approval behaviour.
- Rename user-facing features.
- Change persistence schemas unless required only for test fixtures.
- “Clean up” old code before its responsibilities are documented.

## Acceptance criteria

- The audit document is complete and references concrete files/types.
- Current critical interaction behaviour is covered by tests.
- No intended visible behaviour changes.
- Existing tests pass.
- The next phase can identify exactly where semantic commands should enter the event path.

## Completion report

Report:

- Architecture discovered.
- Tests added.
- Gaps that could not be characterised.
- Risks that may alter later prompts.
- All changed files.

Then stop.
