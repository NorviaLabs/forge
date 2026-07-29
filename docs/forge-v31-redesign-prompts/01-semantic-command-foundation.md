# Prompt 01 — Introduce the Semantic Command Foundation

**Recommended Codex model:** GPT-5.5  
**Reasoning level:** High  
**Visible UX change:** None

## Objective

Create one canonical semantic-command path so keyboard, command palette, and future mouse input can trigger the same application actions.

This phase must preserve the current layout and current user-facing navigation.


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


## Precondition

Read `docs/design/V31-REDESIGN-AUDIT.md`.

Stop if the audit is absent or if existing event ownership is still unclear.

## Scope

Introduce or generalise semantic commands for the behaviours already present.

The command vocabulary should cover, where currently supported:

```text
GoHome
GoBack
OpenFile(path)
ReviewChanges(context)
OpenRun(run_id)
ToggleFiles
FocusComposer
SubmitMessage
InsertComposerNewline
OpenSlashCommands
OpenGlobalCommandPalette
SelectEntry(path)
ToggleDirectory(path)
OpenSelectedEntry
CancelCurrentInteraction
ConfirmCurrentInteraction
```

Adapt names to repository conventions.

## Required architecture

Input handling should move toward:

```text
Input event
→ resolved semantic command
→ one command dispatcher/state transition
```

Requirements:

- Existing keyboard behaviour routes through semantic commands.
- Command handlers own state transitions.
- Widgets do not duplicate business logic.
- Commands may carry typed identifiers and paths.
- Invalid commands fail safely.
- Command execution remains testable without rendering a terminal frame.

## Type-to-compose protection

Do not introduce bare printable global bindings that compete with Composer/type-to-compose.

Event precedence must remain:

1. Modal or blocking overlay.
2. Transient input.
3. Focused control/pane.
4. Application command bindings.
5. Type-to-compose for unhandled printable input.

## Slash commands versus global palette

Represent these as separate intents:

- Composer-local slash commands.
- Application-wide command palette.

Do not merge them into one ambiguous “commands” action.

## Tests

Add tests proving:

- Existing key paths emit the expected semantic command.
- Semantic commands perform the existing state transition.
- Printable keys still reach Composer correctly.
- Modal and transient precedence remains intact.
- Invalid/stale identifiers do not panic.
- Command dispatch works without a rendered frame.
- Existing navigation and Run interactions still work.

## Prohibited changes

Do not:

- Remove Chat/Editor/Diff tabs.
- Add navigation history.
- Change Files visibility.
- Add mouse capture or hit regions.
- Change approval scope.
- Restyle the shell.
- Add new user-facing shortcuts.

## Acceptance criteria

- Current interaction behaviour is preserved.
- Existing keyboard handlers no longer own duplicated business logic.
- Future mouse actions can emit the same commands.
- All relevant tests pass.

## Completion report

Report:

- Command types introduced or extended.
- Event paths migrated.
- Remaining direct state mutations and why they remain.
- Tests added.
- All changed files.

Then stop.
