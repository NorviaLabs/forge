# Forge multisession hi-fi mocks

Interactive high-fidelity design study for Forge's concurrent **session** experience.

The user-facing abstraction is now intentionally simple:

```
Repository
└── Session
    ├── conversation + root agent
    ├── worktree + branch
    ├── current task/turn + queue
    ├── background jobs + subagents
    ├── file browser/search + editor/viewer/diff
    └── terminal
```

A user switches **sessions**. Worktrees, branches, agents, queue items, background jobs, and subagents are owned details of a session rather than peer concepts in the main navigation.

Open `index.html` in a browser. The prototype includes five states:

1. **Workspace** — persistent Sessions strip during normal work.
2. **Session switcher** — searchable repository-wide session management with status grouping and preview.
3. **Needs attention** — a waiting session can ask for approval without stealing focus from the active session.
4. **Instant create** — `n` from the Sessions surface immediately creates an unnamed session/worktree; no creation modal.
5. **Keyboard** — explicit keyboard-ownership contract across every input surface.

## UX decisions

- Rename the top-level multisession concept from **Task** to **Session**.
- Keep task/turn, queue items, background tasks and subagents as execution concepts **inside** a session.
- The persistent strip is **SESSIONS**, not TASKS.
- `F3` is the reserved Forge command for the Sessions switcher.
- `n` creates a new session only while the Sessions strip/switcher owns focus. It is never a global binding.
- `F3` works from composer, files, file search, editor/viewer, diff, terminal, approvals/questions and non-destructive pickers.
- Dirty-buffer and destructive confirmations remain atomic and block switching until resolved/cancelled.
- The embedded terminal becomes session-local and is rooted in that session's worktree.
- File explorer/search/editor/viewer/diff state is session-local and restored when switching back.
- Provider credentials, theme and repository supervision remain global.
- Model choice remains session-local.

## Creation flow

```
n
→ provisional "session 1" appears immediately
→ allocate session identity
→ create stable managed branch/worktree
→ bind AgentSession
→ enable composer
→ first prompt gives the session its display label
```

The display label may change; Git identity should not churn merely because the user-facing title changes.

## Keyboard contract

`F3` must be intercepted at the Forge routing layer **before** focused surfaces such as the embedded PTY consume it. The current `Ctrl+Shift+T` design is not sufficient because the terminal path converts Ctrl chords to PTY bytes before global command routing.

Recommended reserved function keys:

- `F1` Help
- `F3` Sessions
- `F4` Model

Everything else remains contextual to the focused surface.

## Architectural follow-through

The clean implementation direction is to eventually remove the primary-session special case and put every top-level session behind the same repository supervisor actor model. That would make primary/sibling behavior consistent for queues, background jobs, approvals, filesystem state and session switching.
