# Prompt 05 — Make Files Independent, Contextual, and Responsive

**Recommended Codex model:** GPT-5.5  
**Reasoning level:** Medium  
**Visible UX change:** Focused

## Objective

Make Files visibility an independent user preference, remove any mystery icon rail, and add predictable responsive collapse behaviour.


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

- `FilesVisibility` exists independently of `WorkspaceView`.
- Contextual workspace navigation is complete.

## Visibility rules

Files must be:

- Open or closed by explicit user intent.
- Remembered per repository.
- Independent of Conversation, File, Diff, and Run.
- Temporarily auto-collapsed when width is insufficient.
- Restored when width becomes sufficient, unless the user explicitly closed it.
- Reachable through a labelled visible control where space permits.
- Reachable through a semantic command and global command palette.
- Fully keyboard accessible.

Opening a file must not automatically open Files unless the action originated from an already-open Files pane.

## Responsive policy

Define layout priorities rather than fixed screenshot coordinates.

At minimum test:

```text
80×24
120×40
160×50
240×60
```

At narrow widths:

1. Preserve current workspace.
2. Preserve critical status/action.
3. Collapse Files.
4. Remove secondary metadata.
5. Truncate paths visually without mutating stored values.

## No icon rail

Do not add or retain a permanent icon-only navigation strip.

A compact icon may accompany a visible label, but must not be the sole default affordance.

## Persistence

- Persist explicit open/closed preference per repository.
- Do not persist temporary auto-collapse as a user choice.
- Restore safely across schema versions.
- Do not couple width preference to workspace view.

## Tests

Cover:

- Files open/closed in each workspace view.
- Switching views preserves preference.
- Narrow width auto-collapses.
- Width restoration reopens only when user preference is open.
- Explicit close remains closed after resizing.
- Per-repository isolation.
- Old session migration.
- Toggle command and command palette access.
- No File action becomes unreachable with Files closed.
- `80×24` rendering.

## Prohibited changes

Do not:

- Add mouse click handling.
- Add file operations.
- Redesign File Explorer colours/icons.
- Change filesystem behaviour.
- Collapse Inspector or Bottom.
- Add pane resizing.

## Acceptance criteria

- Files visibility is truly independent.
- No workspace transition changes it arbitrarily.
- Narrow layouts remain usable.
- No mystery rail exists.
- Keyboard-only access is complete.
- Tests pass.

## Completion report

Report:

- Preference model.
- Responsive thresholds/policies.
- Persistence migration.
- Entry points.
- Tests added.
- All changed files.

Then stop.
