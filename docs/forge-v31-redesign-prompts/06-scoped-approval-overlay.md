# Prompt 06 — Implement the Scoped Approval Overlay

**Recommended Codex model:** GPT-5.5  
**Reasoning level:** High  
**Visible UX change:** Security-critical and bounded

## Objective

Bring approval behaviour into compliance with the V3.1 contract without changing unrelated command execution or security policy.


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


## Required overlay details

Display:

```text
Execution mode
Executable or shell
Argument vector or shell command string
Working directory
Environment delta
Source/provenance
Reason approval is required
```

The default focused action is:

```text
Allow once
```

Other action:

```text
Deny
```

## Remembered approval

Only exact structured `Direct` invocations are eligible.

Identity must include:

```text
executable
argument vector
working directory
environment delta
workspace identity
current Forge session
```

Required wording:

```text
Remember this exact Direct invocation in this workspace
for the remainder of this Forge session.
```

The following cannot be remembered:

- Shell-mode invocations.
- Ambiguous shell text.
- Invocations whose identity is not exact.
- Destructive operations with dedicated confirmation.
- Approval-sensitive environment data that cannot be safely matched.

## Safety behaviour

- Approval is a blocking overlay.
- Underlying keyboard events are blocked.
- Future mouse events must also be blocked by overlay precedence.
- Clicking outside will eventually do nothing; prepare overlay semantics now.
- `Esc` denies or safely closes, never approves.
- Repeated confirm events cannot approve twice.
- Approval does not enter workspace history.
- Home/Back cannot bypass a required approval.

## 80×24

At minimum:

- Essential fields remain visible first.
- Detail can scroll.
- Allow once and Deny remain reachable.
- Remember option remains keyboard-accessible when eligible.
- Long values truncate visually but are inspectable.

## Tests

Cover:

- Direct allow once.
- Eligible remembered Direct invocation.
- Exact identity match.
- Different argument/cwd/env/workspace does not match.
- Remembered approval expires with session.
- Shell mode has no remember option.
- Default focus is Allow once.
- Esc never approves.
- Duplicate confirmation is idempotent.
- Underlying commands are blocked.
- Overlay does not alter workspace history.
- 80×24 rendering and navigation.
- Secret redaction follows existing policy.

## Prohibited changes

Do not:

- Broaden sandbox/security permissions.
- Add mouse capture.
- Redesign Run.
- Add broad “allow all session” capability.
- Persist remembered approval beyond the current Forge session.
- Infer command safety from display text.

## Acceptance criteria

- Approval identity is exact and testable.
- Shell approvals are one-time only.
- Overlay is safely blocking.
- Narrow-screen approval is usable.
- Existing approval integrations still work.
- Tests pass.

## Completion report

Report:

- Approval model.
- Matching identity.
- Session lifetime.
- Overlay event precedence.
- Tests added.
- All changed files.

Then stop.
