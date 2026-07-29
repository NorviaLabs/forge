# Prompt 09 — Add Safe Double-Click Activation

**Recommended Codex model:** GPT-5.5  
**Reasoning level:** Medium  
**Visible UX change:** Small

## Objective

Add double-click only for opening files and toggling folder rows, on top of the proven mouse hit-region foundation.


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

- Prompt 08 mouse foundation is complete.
- Single-click selection and stale-target invalidation are stable.
- Mouse capture lifecycle tests pass.

## Recognition rules

A double-click qualifies only when:

- Both events are left-button activation events.
- Both resolve to the same semantic target.
- They occur within an internal threshold, initially around 400 ms.
- No scroll occurs between them.
- No resize occurs between them.
- No modal/overlay state changes.
- No list mutation invalidates the target.
- No focus transition changes the semantic target.
- The target still exists in the current frame.

Compare semantic identity, not coordinates alone.

## Behaviour

```text
Single click file
→ SelectEntry(path)

Second qualifying click
→ OpenFile(path)
```

```text
Single click folder row
→ SelectEntry(path)

Second qualifying click
→ ToggleDirectory(path)
```

Directory chevrons remain single-click controls.

Explicit buttons and controls do not gain double-click semantics. Repeated clicks must activate at most once where duplicate execution is unsafe.

Double-click never bypasses approval or confirmation.

## State

Track only the minimal state required:

```text
semantic target
button
position if useful
timestamp
frame generation
```

Reset pending state on timeout or invalidation.

Do not expose a user preference for threshold in this phase.

## Tests

Cover:

- Two fast clicks on same file open it.
- Two slow clicks only select.
- Fast clicks on different rows do not open.
- Scroll between clicks cancels.
- Resize between clicks cancels.
- List mutation cancels.
- Modal opening cancels.
- Truncated filename still resolves by semantic identity.
- Double-click folder toggles once.
- Double-click button does not execute twice.
- Double-click destructive control cannot bypass confirmation.
- Enter and double-click emit equivalent activation commands.

## Prohibited changes

Do not:

- Add single-click file opening.
- Add triple-click.
- Add text-selection semantics.
- Add user-configurable timing.
- Add drag or right-click.
- Add double-click to destructive actions.

## Acceptance criteria

- Double-click is reliable against semantic targets.
- It is limited to file/folder row activation.
- It cannot double-trigger controls.
- It adds no new business logic.
- Tests pass.

## Completion report

Report:

- Recogniser state and threshold.
- Invalidation conditions.
- Targets enabled.
- Tests added.
- All changed files.

Then stop.
