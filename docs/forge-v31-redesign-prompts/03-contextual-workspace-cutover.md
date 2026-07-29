# Prompt 03 — Cut Over to One Contextual Workspace

**Recommended Codex model:** GPT-5.5  
**Reasoning level:** High  
**Visible UX change:** Major but tightly scoped

## Objective

Replace permanent Chat, Editor, and Diff mode tabs with one contextual workspace driven by the `WorkspaceView` state.

Do not redesign supporting panes, approvals, mouse input, or visual themes in this phase.


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

- Prompt 02 navigation state is complete.
- Existing tab actions already emit semantic navigation commands.
- Characterisation tests pass.

## Required behaviour

### Conversation

- Conversation is Home.
- Composer remains visible.
- Conversation does not display permanent Editor or Diff tabs.
- No duplicate inner heading such as `Chat · Chat`.
- No icon-only navigation rail.

### File

- Users enter File by opening a specific file.
- The contextual header shows the current path/resource.
- There is no normal “No file selected” destination.
- Opening another file replaces the current File view.
- Back returns to the previous valid view.
- Home returns to Conversation.

### Review changes

- Users enter Diff only through an explicit review action.
- Use user-facing language such as `Review changes`.
- Show contextual file/change information.
- Opening a file from Diff uses the navigation contract.
- No speculative Symbols, Outline, or permanent summary sidecars.

### Run

- A Run view can be opened explicitly using existing Run state.
- Leaving it does not cancel the process.
- Async no-hijack behaviour is handled in Prompt 04; do not expand that scope here.

## Header

Introduce one compact contextual header that owns:

- Repository identity.
- Branch and dirty state.
- Current resource/status when relevant.
- Compact actionable change/run indicator when existing data is already available.

Do not duplicate model, context percentage, task state, or repository path across multiple surfaces.

## Transitional compatibility

Remove or redirect obsolete permanent tab entry points.

Do not leave two separate navigation models active.

Command palette and existing keyboard commands must still make every view reachable.

## Tests

Cover:

- Conversation renders without permanent workspace tabs.
- OpenFile enters File.
- OpenFile while in File replaces.
- ReviewChanges enters Diff.
- OpenRun enters Run.
- Back and Home work from each view.
- No File view is entered without a path.
- Composer focus/type-to-compose remains correct.
- Files selection/open behaviour still works.
- Current Run and external-editor flows remain intact.
- `80×24` does not panic.

## Prohibited changes

Do not:

- Add mouse support.
- Change Files persistence or auto-collapse.
- Redesign Inspector or Bottom panel.
- Change async activity priority.
- Change approval policy.
- Add colours/icons/themes.
- Add new side panels.

## Acceptance criteria

- Exactly one workspace view is primary.
- Permanent Chat/Editor/Diff tabs are gone.
- All old capabilities remain reachable.
- No duplicate mode headings remain.
- Back and Home are predictable.
- Existing tests pass.

## Completion report

Report:

- Old navigation removed or redirected.
- New view rendering ownership.
- Header ownership.
- Compatibility issues.
- Tests added.
- All changed files.

Then stop.
