# Prompt 07 — Simplify Supporting Surfaces and Application Chrome

**Recommended Codex model:** GPT-5.5  
**Reasoning level:** Medium  
**Visible UX change:** Major visual simplification, no new capability

## Objective

Reduce default-screen density by making Inspector and Bottom contextual, removing duplicated status, and replacing permanent shortcut manuals with one contextual hint slot.


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


## Inspector

- Closed by default unless an existing user preference explicitly opens it.
- Open on demand.
- Size to useful content within responsive limits.
- Do not occupy a permanent full-height column when empty.
- Critical attention must remain visible through the owning activity/status surface while Inspector is closed.
- Preserve existing Inspector information and commands.

## Bottom surface

- Remove the user-facing label `BOTTOM`.
- Use purpose-oriented entries such as Run, Terminal, Diagnostics, and Activity where they already exist.
- Collapse when inactive or empty.
- Opening content is explicit or driven by a blocking interaction.
- Run starting in background does not expand it automatically.
- Preserve user-resized state only if the existing architecture already supports it safely.
- Use one structural separator rather than a heavy enclosing box.

## Contextual hints

Create one application-owned hint slot.

Show it only during a transient interaction where it materially helps:

```text
Enter confirm · Esc cancel
Tab move · Enter allow once · Esc deny
```

Do not show a permanent global shortcut manual.

Help remains available through the global command palette, contextual help, onboarding, and documentation.

## Status ownership

Remove duplicate display of:

- Task completion.
- Model.
- Context percentage.
- Repository path.
- Provider/reasoning details.
- Token counts.
- Quota details.
- Unexplained abbreviations.

Keep only current-actionable status in default chrome.

Move diagnostics/usage details on demand without deleting the underlying data.

## Borders and headings

- Major regions may use subtle separators.
- Nested widgets should not each render their own full box by default.
- Remove repeated headings that restate the active context.
- Preserve obvious focus indication.
- Do not rely only on colour.

## Tests

Cover:

- Inspector closed/open.
- Inspector empty/content.
- Bottom collapsed/open.
- Background Run does not expand Bottom.
- Critical Run failure remains discoverable.
- Contextual hint appears only in relevant state.
- All removed status remains reachable on demand where required.
- Focus navigation with supporting surfaces open/closed.
- `80×24`, `120×40`, and wide layouts.
- No current commands become unreachable.

## Prohibited changes

Do not:

- Add mouse support.
- Introduce themes.
- Add animations.
- Remove diagnostic data from the application.
- Redesign the File Explorer tree.
- Add new supporting panels.

## Acceptance criteria

- Default screen contains one dominant workspace.
- Empty supporting surfaces do not consume permanent space.
- No permanent shortcut footer remains.
- Duplicate state has one owner.
- Focus remains obvious.
- Keyboard workflows remain complete.
- Tests pass.

## Completion report

Report:

- Surfaces made contextual.
- Status ownership changes.
- Hint-slot implementation.
- Removed duplicated chrome.
- Tests added.
- All changed files.

Then stop.
