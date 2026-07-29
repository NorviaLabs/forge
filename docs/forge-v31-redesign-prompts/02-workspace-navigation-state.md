# Prompt 02 — Add Workspace View and Navigation State

**Recommended Codex model:** GPT-5.5  
**Reasoning level:** High  
**Visible UX change:** Minimal or none

## Objective

Introduce the V3.1 workspace state and navigation contract underneath the current UI before removing permanent tabs.


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

- Prompt 00 audit is complete.
- Prompt 01 semantic command routing is complete.
- Current tests pass.

## Scope

Introduce or adapt state equivalent to:

```text
WorkspaceView
    Conversation
    File(path)
    Diff(context)
    Run(run_id)

FilesVisibility
    Open(width)
    Closed

Overlay
    None
    Approval(request)
    Confirmation(action)
    Error(details)
```

Use existing domain types instead of duplicating them where possible.

## Navigation operations

Implement:

```text
PushView(view)
ReplaceView(view)
Back
Home
OpenFile(path)
ReviewChanges(context)
OpenRun(run_id)
ToggleFiles
CloseOverlay
```

Rules:

- Conversation is Home.
- `Home` always returns to Conversation.
- `Back` returns to the previous valid workspace view.
- If no valid history entry exists, Back returns to Conversation.
- Opening a new kind of activity pushes.
- Changing the resource within the same activity replaces.
- Overlays do not become workspace-history entries.
- Files visibility is independent from workspace history.
- Leaving Run does not cancel it.

## Compatibility adapter

Keep the current Chat/Editor/Diff UI working during this phase.

Existing tabs or shortcuts may temporarily emit the new navigation commands.

Do not remove the old visible navigation yet.

## Persistence

Persist only what is already safe and useful:

- Current workspace view only if current session persistence already supports similar state.
- Files visibility independently.
- Navigation history may remain session-local and bounded.

Use versioned, non-destructive migration for persisted state.

## Tests

Cover:

- Conversation is initial Home.
- Conversation → File uses push.
- File A → File B uses replace.
- File → Diff uses push.
- Diff → File follows the requested navigation rule.
- Back skips invalid/deleted resources.
- Home returns to Conversation.
- Overlay open/close does not alter view history.
- Files visibility does not change when switching views.
- Leaving Run does not cancel Run.
- Restoring old session state remains safe.

## Prohibited changes

Do not:

- Remove permanent tabs yet.
- Redesign headers.
- Change async event behaviour.
- Add mouse input.
- Collapse Inspector or Bottom.
- Change approval UI.
- Add visual polish.

## Acceptance criteria

- Workspace navigation is represented explicitly.
- Existing UI drives the new state without regressions.
- Files visibility is no longer conceptually owned by a workspace tab.
- Back/Home semantics are covered by tests.
- Existing tests pass.

## Completion report

Report:

- State types introduced or reused.
- Navigation stack behaviour.
- Persistence changes.
- Compatibility adapters.
- Tests added.
- All changed files.

Then stop.
