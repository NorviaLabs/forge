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
