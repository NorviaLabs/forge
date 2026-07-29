# Forge V3.1 Redesign — Prompt Sequence

This pack breaks the redesign into small, reviewable phases. Run each prompt in a fresh Codex session. Do not allow a session to continue automatically into the next prompt.

## Model recommendation

Use **GPT-5.5 in Codex** for the full sequence.

- Use **High reasoning** for architecture, security, event routing, lifecycle, and final integration.
- Use **Medium reasoning** for bounded UI-policy and interaction additions whose architecture is already established.
- Standard mode is preferred. Fast mode is optional for time-sensitive, bounded phases, but is not required.

## Sequence

| # | Prompt | Reasoning | Main outcome | Visible risk |
|---|---|---:|---|---|
| 00 | Audit and characterisation | High | Architecture map and regression baseline | None |
| 01 | Semantic command foundation | High | One command path for all inputs | None |
| 02 | Workspace navigation state | High | View stack, Back/Home, independent Files state | Minimal |
| 03 | Contextual workspace cutover | High | Remove permanent Chat/Editor/Diff tabs | High |
| 04 | Async activity and non-hijack | High | Background activity never navigates | Medium |
| 05 | Files visibility and responsive policy | Medium | Per-repo Files preference and narrow collapse | Medium |
| 06 | Scoped approval overlay | High | Exact Direct approvals; Shell allow-once | Security-critical |
| 07 | Supporting surfaces and chrome | Medium | Contextual Inspector/Bottom and no hint clutter | High |
| 08 | Mouse foundation | High | Click/focus/select/control/scroll | Lifecycle-critical |
| 09 | Double-click activation | Medium | File/folder activation only | Low |
| 10 | Edge-state transitions | High | Nine required recovery behaviours | High |
| 11 | Final polish and regression gate | High | Cleanup, screenshots, full acceptance | Integration-critical |

## Hard gates

Do not proceed to the next prompt unless:

1. The current prompt’s acceptance criteria pass.
2. Existing critical workflows remain functional.
3. The relevant test suite passes.
4. The completion report lists no unresolved blocker that invalidates the next phase.
5. The working tree is reviewed and checkpointed.

## Recommended checkpoints

Create one reviewed commit after every prompt.

Suggested commit themes:

```text
test(ui): characterise pre-v3 interaction behaviour
refactor(input): route interactions through semantic commands
refactor(workspace): add contextual view navigation
feat(ui): cut over to contextual workspace
fix(activity): prevent async workspace hijacking
feat(files): make pane visibility independent and responsive
fix(security): scope approval overlay and remembered commands
refactor(ui): simplify supporting surfaces and chrome
feat(input): add semantic mouse interactions
feat(input): add safe double-click activation
fix(ui): implement v3.1 edge-state recovery
refactor(ui): complete v3.1 shell and regression gate
```

## Rollback rule

If a phase requires broad unrelated changes, stop. Update the audit and split that phase rather than expanding the prompt.

## Authoritative specification

`FORGE-V3.1-INTERACTION-CONTRACT.md` is authoritative. Earlier mockups are illustrative.
