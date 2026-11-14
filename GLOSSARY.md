# Glossary

Domain terms that name modules and seams in Forge. Use these names in code,
docs and reviews.

- **Session targeting**: resolving a session id to the open project that owns
  it, whether the session is live or closed. An id no open project knows is a
  failure, never a fallback to the project on screen.
- **Model selection**: a session's model, provider profile and reasoning
  effort, together with the capabilities derived for that model from the model
  catalog (context window, output reserve, image input). The model and its
  capabilities always change together.
- **View record**: the desktop page's per-session state (open documents,
  drafts, file tree), keyed by session id. Selecting a session chooses which
  record renders.
- **Session snapshot**: the backend's projection of the on-screen session
  (transcript, turn state, pending decisions, plan, queue), replaced whole on
  each update.
- **Model tier**: `fast`, `default` or `strong` — a provider/model/effort
  combination named by what it is for rather than by model id. Forge derives
  each provider's tiers from models.dev metadata for the models the account
  can run; users never configure them. A failed subagent turn is re-run once
  on the strong tier (**escalation**).
- **Turn record**: the one row a finished turn — top-level or subagent —
  writes to its session's journal: model, effort, tier, outcome and reason,
  escalation, tokens, duration, and the turn's **signals** (what happened
  during it: approvals, verification commands, cancellation, …).
