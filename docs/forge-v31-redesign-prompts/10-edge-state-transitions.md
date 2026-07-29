# Prompt 10 — Implement and Verify the Nine Edge-State Transitions

**Recommended Codex model:** GPT-5.5  
**Reasoning level:** High  
**Visible UX change:** Only where required for explicit recovery states

## Objective

Implement or verify every edge-state transition required by the V3.1 contract.

Do not broaden the redesign.


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


## Required transitions

### 1. Run cancelled

- Preserve captured output.
- Final state is Cancelled, not Failed.
- Do not navigate unless already in Run.
- Keep rerun/edit options where appropriate.

### 2. Process failed to spawn

- Distinguish from non-zero process exit.
- Show executable, arguments, working directory, and cause.
- Do not fabricate an exit code.
- Offer Edit & Rerun and Back.

### 3. Network lost while agent streams

- Preserve received content.
- Mark response Interrupted.
- Keep Composer usable.
- Offer Retry or Continue.
- Do not change workspace view.

### 4. Open file renamed externally

- Verify identity where possible.
- Update path and contextual header.
- Preserve scroll/cursor state.
- Show a brief notice.
- If identity is uncertain, do not guess.

### 5. Open file deleted externally

- Keep File view.
- Show `File no longer exists`.
- Preserve any in-memory content.
- Offer Back and Locate.
- Never silently open another file.

### 6. Diff becomes stale

- Preserve current review position.
- Mark stale.
- Disable Apply.
- Require Refresh.
- Avoid applying outdated changes.

### 7. Approval at 80×24

- Use available workspace.
- Show mode, command, directory, and reason first.
- Allow details to scroll.
- Keep Allow once and Deny reachable.
- Remember option remains keyboard-accessible only when eligible.

### 8. Mouse disabled

- Complete primary workflow remains keyboard accessible.
- No mouse-specific hint takes space.
- Native terminal text selection remains available.

### 9. Hit target invalidated

- Resolve only against latest frame.
- Ignore stale targets safely.
- Resize, list mutation, modal change, and scroll cancel pending double-click state.

## Integration rules

- Recovery states must use semantic commands.
- Async errors must not auto-navigate.
- Critical blocking approvals remain overlays.
- Non-blocking failures use activity summary and explicit inspection.
- Existing session data must remain recoverable.

## Tests

Create focused unit/integration tests for every transition.

Also add one end-to-end scenario:

```text
Conversation
→ agent changes file
→ open file
→ open Diff
→ run command in background
→ Run fails
→ inspect failure
→ Back to Diff
→ file changes externally
→ Diff becomes stale
→ refresh
→ Home
```

Run it with mouse enabled and disabled where practical.

## Prohibited changes

Do not:

- Add new general-purpose notification framework.
- Add editor content editing.
- Add merge conflict resolution.
- Add automatic file relocation guesses.
- Add new mouse gestures.
- Restyle unrelated screens.

## Acceptance criteria

- All nine transitions match the contract.
- Each is distinguishable in state and UI.
- Each has automated coverage.
- No transition crashes or silently discards state.
- Navigation remains user-controlled.
- Tests pass.

## Completion report

Report each edge state separately:

- Previous behaviour.
- New behaviour.
- Tests.
- Remaining platform limitations.
- All changed files.

Then stop.
