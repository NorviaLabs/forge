# Prompt 04 — Add Non-Hijacking Async Activity and One Prioritised Summary

**Recommended Codex model:** GPT-5.5  
**Reasoning level:** High  
**Visible UX change:** Focused

## Objective

Ensure background agent and Run activity never changes the user’s workspace automatically, and show at most one compact activity summary in Conversation.


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

- Contextual workspace cutover is complete.
- Run and agent events are identifiable.
- Existing Activity/history storage is understood.

## Async rules

Implement:

```text
RunStarted
→ update Run state
→ update activity
→ do not navigate

RunSucceeded
→ update Run record
→ update activity only when useful
→ do not navigate

RunFailed
→ update Run record
→ show attention activity
→ do not navigate

AgentThinking
→ retain Conversation and Composer
→ show non-blocking status
→ do not navigate

AgentStreaming
→ append to Conversation
→ do not navigate away from File, Diff, or Run

ApprovalRequired
→ open blocking overlay
→ preserve underlying workspace
```

No async event may call `PushView`, `ReplaceView`, `Back`, or `Home`.

## Activity summary

Conversation displays at most one summary.

Priority:

1. Approval required — represented by overlay.
2. Run failed.
3. Run active.
4. Changes available.
5. Agent planning/thinking.
6. Idle — no summary.

Examples:

```text
Run failed · Inspect
Running cargo test · View output
5 files changed · Review
```

Rules:

- Summary actions emit semantic commands.
- Lower-priority events remain available through the existing Activity/log surface where one already exists.
- Do not create a new dashboard.
- Do not stack multiple cards.
- Agent Thinking must not replace Conversation with a full-screen spinner.

## Run view behaviour

- `View output` explicitly opens Run.
- Failure summary explicitly opens failed Run.
- Leaving Run does not cancel.
- Run completion while viewing another workspace does not navigate.

## Tests

Cover:

- Run start while in Conversation.
- Run start while in File.
- Run failure while in Diff.
- Agent streaming while viewing File.
- Agent thinking leaves Composer usable.
- Activity priority transitions.
- Only one summary renders.
- Summary action opens the expected view.
- Async events never mutate navigation history.
- Existing Activity history remains available.
- No duplicate status ownership.

## Prohibited changes

Do not:

- Redesign approvals beyond opening the existing overlay.
- Add mouse support.
- Restyle the whole shell.
- Add notifications outside the existing app architecture.
- Build a new Activity dashboard.
- Change Run execution semantics.

## Acceptance criteria

- Background activity never hijacks the workspace.
- Conversation and Composer remain usable during non-blocking work.
- One activity summary communicates the most actionable state.
- All summary actions use semantic commands.
- Tests pass.

## Completion report

Report:

- Event routing changed.
- Activity model and priority.
- Summary ownership.
- Navigation invariants.
- Tests added.
- All changed files.

Then stop.
