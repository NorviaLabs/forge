# Prompt 11 — Final Shell Polish, Cleanup, and Regression Gate

**Recommended Codex model:** GPT-5.5  
**Reasoning level:** High  
**Visible UX change:** Final structural polish only

## Objective

Complete the redesign by applying restrained visual hierarchy, removing obsolete implementation paths, and running a comprehensive regression gate.

This phase must not add new product capability.


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


## Visual hierarchy

Apply layout and spacing rules already implied by the contract:

- One dominant workspace.
- Minimal major separators.
- No nested box around every component.
- Clear active/inactive focus.
- Readable Conversation content width with code/diff expansion.
- Contextual headers that describe the current resource.
- Supporting surfaces visually recede.
- Empty surfaces collapse.
- Critical state is visible without opening hidden panels.
- Colour remains semantic and restrained; full themes remain deferred.

Do not revisit palette selection.

## Remove obsolete paths

After proving replacements work:

- Remove dead permanent Chat/Editor/Diff tab logic.
- Remove duplicate state ownership.
- Remove mystery rail remnants.
- Remove permanent shortcut footer remnants.
- Remove unused compatibility adapters from Prompts 02–03.
- Remove mouse-independent coordinate hacks.
- Remove obsolete approval paths superseded by exact scope.

Do not delete code merely because it looks old; prove it is unreachable or replaced.

## Screenshot and rendering matrix

Capture or test:

```text
80×24
120×40
160×50
240×60
```

For:

- Conversation idle.
- Agent thinking.
- Files open and closed.
- File open.
- Diff.
- Background Run.
- Run explicitly open.
- Run failed.
- Approval.
- Inspector open/closed.
- Bottom surface open/closed.
- Mouse disabled.

Review for:

- Primary focus obvious within two seconds.
- No duplicated labels/state.
- No permanently empty dominant surface.
- No clipped primary action.
- No stale click regions.
- No permanent shortcut manual.
- Long paths and output remain usable.

## Regression workflows

Verify keyboard-only and mouse-assisted versions of:

```text
Open repository
→ converse
→ open file
→ review changes
→ run command
→ inspect output
→ Back
→ Home
```

Also verify:

- External editor.
- Session restore.
- Run cancellation.
- Spawn failure.
- Approval deny/allow.
- File rename/delete reconciliation.
- Diff staleness.
- Terminal resize.
- Clean shutdown with active process.
- Panic/error terminal restoration where testable.

## Documentation

Update:

- `FORGE-DESIGN.md`
- V3.1 contract status.
- Keybinding/help documentation.
- Mouse configuration.
- Known limitations.
- Screenshots only after behaviour is final.

## Prohibited changes

Do not:

- Add themes.
- Add drag/drop or context menus.
- Add pane resizing.
- Add inline editing.
- Add new Run providers.
- Add new File Explorer features.
- Add speculative side panels.
- Change product terminology outside the agreed contract.

## Acceptance criteria

- V3.1 contract is satisfied.
- Old contradictory navigation/chrome is removed.
- Keyboard-only mode is complete.
- Mouse-assisted mode uses the same commands.
- All target sizes are usable.
- Full relevant test suite passes.
- No known terminal restoration regression.
- Documentation matches implementation.

## Completion report

Provide:

1. Contract compliance checklist.
2. Regression test results.
3. Screenshot/layout review results.
4. Obsolete code removed.
5. Known limitations.
6. Every changed file.
7. Explicit recommendation: ready for beta UX testing or not ready, with blockers.

Then stop.
