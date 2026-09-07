# Forge multisession hi-fi mocks

Interactive high-fidelity design study for Forge's concurrent multi-session experience.

Open `index.html` in a browser. The top controls switch between four representative states:

1. **Workspace** — persistent task strip while working normally.
2. **Task switcher** — searchable repository-wide task management with status grouping and a detail/attention preview.
3. **Needs attention** — non-disruptive background attention while the current task keeps focus.
4. **Instant create** — pressing `n` immediately creates a provisional task, then shows branch/worktree creation and composer readiness without a modal.

## Design synthesis

### From Codex

- Status-oriented grouping: tasks needing input are promoted above working/ready tasks.
- Searchable session/task picker rather than requiring users to remember identifiers.
- Rich metadata for branch/CWD/session identity.
- A detail preview so switching is an informed action rather than blind navigation.
- Management actions stay in the picker instead of bloating the permanent workspace.

### From OpenCode

- Fast, compact session switching as a first-class terminal workflow.
- Recency/activity information is kept visually quiet but available while scanning.
- Dense list rows optimize for keyboard-driven selection rather than card-heavy dashboard UI.

### Forge-specific decisions

- Keep the **persistent task strip** as ambient context. This is Forge's advantage over a resume-only picker.
- Preserve Forge's semantic status glyphs: `[ ] [>] [✓] [!] [?]`.
- Attention never steals keyboard focus. It appears in the strip, a small toast, and the switcher.
- One active agent session remains bound to one worktree; branch/worktree identity is explicit in the switcher.
- Model is task-local; provider/authentication stays global.
- The switcher is the dense management surface: switch, search, rename, pin, archive, attach, inspect, and reach advanced creation options.
- The normal workspace remains calm and does not become a multi-agent dashboard.\n- **`n` is the default creation path:** allocate identity → create `forge/task-<id>` + managed worktree from the current committed HEAD → bind the session → focus the composer.\n- The first prompt renames only the Forge display label. The Git branch/worktree path remains stable.\n- Prompt submission stays disabled/queued while creation is in flight; creation failure removes the provisional task and reports the Git error.\n- Advanced/custom creation and Attach remain secondary flows in the task switcher.

## Main UX recommendation

Treat multisession as two layers:

- **Task strip = glance + fast switching.**
- **Task switcher = search + inspection + lifecycle management.**

Do not put full progress cards, logs, or per-task controls permanently on screen. That would fight Forge's existing dense-but-calm design system and reduce space for actual coding work.
