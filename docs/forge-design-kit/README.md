# Forge design kit

Contents:

- `FORGE-DESIGN.md` — authoritative Forge TUI design system.
- `references/warp/DESIGN.md` — downloaded reference analysis.
- `references/opencode.ai/DESIGN.md` — downloaded reference analysis.
- `references/ollama/DESIGN.md` — downloaded reference analysis.
- `REFERENCES-LICENSE` — MIT licence from the reference repository.

## Recommended repository placement

```text
docs/design/FORGE-DESIGN.md
docs/design/references/warp/DESIGN.md
docs/design/references/opencode.ai/DESIGN.md
docs/design/references/ollama/DESIGN.md
```

Keep `FORGE-DESIGN.md` authoritative. The other files are inspiration only.

For the planned active-block and shortcut work, tell the coding agent:

```text
Read docs/design/FORGE-DESIGN.md before editing. Implement only the scope of
Prompt 17. Preserve the focus and navigation contract from Prompts 16 and
16.5. Use the reference files only for background; do not copy their product
identity or redesign unrelated components.
```
