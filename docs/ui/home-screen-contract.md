# Home screen contract

The home screen is the idle state shown after a session is created and before
the first user message is submitted. `docs/ui/images/01-home.png` is the
visual reference for this state.

## Idle content

The idle screen must communicate, in this order:

1. Forge is ready and identify the active workspace.
2. Report the loaded `AGENTS.md` file and session journal path.
3. Tell the operator that a task can be entered and `/` opens commands.
4. Show that the assistant is waiting for the first message.
5. Provide the task input and the idle keyboard hints.

The sidebar must expose four distinct summaries: session metadata, context
budget, tool permissions, and recent journal events.

## Runtime content

Busy state, provider connection state, feedback, notices, command completion,
message queues, tool execution, approvals, and conversation history remain
dynamic. They may replace or extend the idle content when active, but must not
make the idle state look busy when no user turn is running.

Model system prompts are never rendered as operator-facing welcome content.

## Responsive behavior

The sidebar may be hidden on narrow terminals. The conversation and input must
remain usable, and overlays, feedback strips, queues, and multiline input must
continue to respect the existing terminal-size safeguards.
