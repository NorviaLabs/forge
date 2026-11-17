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
  writes to its session's journal: model, effort, tier, **task kind**, outcome
  and reason, escalation, tokens, duration, and the turn's **signals** (what
  happened during it: approvals, verification commands, cancellation, …).
- **Task kind**: what ran a turn — `main` (a top-level session), `subagent`
  (a built-in subagent, writer or read-only alike) or `custom:<name>` (a
  child started as that custom agent, in either mode). A turn recorded before
  kinds existed gets the kind its journal shows: a child (an agent profile, or
  a parent) naming an agent is `custom:<name>`, any other child `subagent`,
  anything else `main`.
- **Model report**: `forge models report` and
  `forge_session::model_report` — turn records grouped by task kind and by
  model, provider and effort, scored on seven measures and ranked per kind by
  mean rank; groups with fewer than the minimum turns are **not enough data**.
- **Model choice**: why a subagent started without a tier or a model runs on
  its model — `ranking` (the leader of the ranking for its kind, among the
  parent's provider's runnable models), `exploration` (the parent's model,
  kept on purpose about one spawn in ten so other models keep being tried) or
  `fallback` (the parent's model: thin evidence, `inherit`, an untrusted
  workspace, no ranking yet). Journaled with the child's profile, returned by
  `spawn_agent`, and recorded as the `model_chosen` turn signal.
- **Auto**: the main session's model choice when the user lets Forge pick:
  each turn starts on the ranked best for main-session work, or on the
  **fallback** (the model the user had) when the evidence is thin, and
  climbs the **step-up ladder** after failed turns. Each turn's choice is an
  **Auto decision** (`ranked`, `fallback`, `stepped_up`, `stepped_down`).
- **Step-up ladder**: Auto's order of escalation, effort before model — the
  start, each higher effort on the same model, then the provider's
  strong-tier model and its higher efforts. Up one step after a failed turn,
  down one after two successful turns in a row.
- **Remote control**: `forge-remote`, the off-by-default service that lets
  one **paired device** follow sessions, send prompts, stop a turn and
  answer approvals over pinned TLS. Nothing listens while it is off.
- **Paired device**: the one device holding a working remote token. Forge
  stores only the token's SHA-256; pairing another device replaces it, and
  revoking it ends its connections at once.
- **Pairing code**: eight Crockford base32 characters (`ABCD-EFGH`) the
  desktop shows to pair a device. Memory only, single use, five minutes,
  destroyed after five wrong guesses. The **pairing link**
  (`forge-remote://host:port/?code=…&fp=sha256:…`) carries it with the
  certificate **pin**.
- **Pairing request**: what a redeemed pairing code opens. The device waits
  on it (polling with a secret **pairing id**) until the desktop allows or
  refuses it, for at most two minutes; the token is created only when an
  allowed device collects it.
- **Confirmation number**: the six digits the desktop and the device both
  show for a pairing request, so you allow only the device in your hand.
- **Event log**: a followed session's remote events, numbered without gaps
  (`<epoch>.<seq>`). A reconnect replays from it, or gets a **resync**
  snapshot when it cannot.
- **Execution target**: where a session's tools run — this machine (`local`,
  under Forge's sandbox) or a host from the user's SSH config and a folder on
  it (`ssh`). Saved with the session; every workspace tool goes through it
  (`forge_tools::target::Target`).
- **Remote root**: the canonical path of an SSH session's folder on its host,
  found when connecting. The remote counterpart of the workspace root.
- **Remote temp**: an SSH session's private temp directory on its host (the
  commands' `$TMPDIR`), made when connecting and removed when the session
  closes.
- **Ask before each command**: an SSH session's setting (on by default) that
  makes every command and `git` call on the host wait for approval.
