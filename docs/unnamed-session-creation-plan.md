# Unnamed session creation, name-on-submit

Implementation plan. Decision record from a design grilling session; every claim
below was verified against the code on this branch.

## Goal

Creating a session stops being a prompt-first act. `n` allocates the session and
its worktree immediately, selects it, and puts the cursor in the composer. The
session is named from its first prompt when that prompt is submitted.

## Decisions

| # | Decision |
|---|---|
| 1 | The name derives from `title_from_prompt`, capped at **4 words, hyphen-joined**. |
| 2 | The temporary name is **render-time only** — never persisted, never a UUID. |
| 3 | Label and branch slug **share the rule** so the navigator row and `forge/<slug>` never disagree. |
| 4 | The rename fires **on submit**, not after the turn completes. |
| 5 | Creation is **prompt-less**, so it never parks on the trust modal. |
| 6 | Unnamed sessions **accumulate like any other session**. Reaping is out of scope. |
| 7 | `n` becomes "create session and jump to the composer". |

## What already works (no change needed)

These were verified and are load-bearing — do not "fix" them:

- **The placeholder is the temp name, and it is not persisted.** Three sites
  synthesize a display name for an empty label at render time:
  `render.rs:331` (task strip) and `render.rs:406` (navigator) render
  `session 1` / `session 2` with an ordinal that counts unnamed sessions;
  `commands.rs:60` (`/sessions`) renders the bare word `session` with no
  ordinal. The label in the database stays `''`. Nothing new is needed to "show
  a temporary name" — a persisted UUID would *delete* this behavior, not add
  one. The ordinal is per-render and resets when the roster changes, so decide
  whether `/sessions` should adopt the ordinal copy while you are in here.
- **Prompt-less creation already exists.** `supervisor.rs:4676` calls
  `CreateSession { label: "", first_prompt: None }`. The test at
  `supervisor.rs:4681` asserts *"One-key creation must not park on a modal"* and
  panics if `TrustRequired` fires. Decision 5 is the shipped behavior.
- **Trust is inherited, not granted per session.** `is_trusted_at`
  (`forge-config/src/trust.rs:67`) canonicalizes and walks *up* the tree. A
  managed worktree lives at `<repo>/.forge/local/worktrees/session-<uuid>`, so a
  trusted repo root makes `already_trusted` true and the modal never fires. The
  modal is therefore not a security boundary for this path.
- **Branch ordering is already correct.** `SubmitPrompt`
  (`supervisor.rs:1544`) renames **before** `enqueue_prompt` and **before**
  `start_prompt_driver`, all awaited. `materialize_managed_branch` runs at the
  end of turn 1 and reads the label. So the branch gets the good name with no
  ordering change — **provided the rename stays on submit** (decision 4).
- **Worktree identity is already decoupled.** The path is
  `session-<uuid>` and `worktree.rs:75` documents that renaming a session must
  never rename its branch or worktree. No worktree change is needed.
- **`enqueue_prompt`'s trust guard is inert here.** `control.rs:684` rejects
  prompts while a `pending_operations` row sits in `awaiting_trust`. With no
  parked prompt there is no such row.

## Work

### 1. `forge-types` — the naming rule

`crates/forge-types/src/lib.rs`

- `title_from_prompt` (`:45`): cap the result at 4 words and join them with `-`.
  The existing 60-char boundary (`TITLE_MAX_CHARS`) is subsumed by the word cap
  and stays as a defensive outer bound.
- **`crates/forge-types/src/lib.rs:44` must be corrected.** It currently reads
  *"Display-only: never used as a path or Git ref."* That is **already false**:
  `unique_session_branch` (`supervisor.rs:2334`) reads `task.label` into
  `forge/<slug>`. After this change the comment would be actively misleading.
  Restate the real contract: the label feeds the branch slug on first
  materialization, is display-safe, and must survive `sanitize_label` without
  lossy rework.
- Character class: output is **lowercase ASCII alphanumerics joined by single
  hyphens** — nothing else. `_` becomes `-`, runs of non-alphanumerics collapse
  to one `-`, and leading/trailing `-` are stripped. Lowercasing is deliberate
  and replaces the current `sentence_case` behavior, because the invariant is
  now exact: **`sanitize_label(title_from_prompt(p)) == title_from_prompt(p)`**.
  The label the navigator shows and the branch Git gets are the same string.
- The 4-word cap counts **normalized tokens**, not whitespace words. Normalize
  the first sentence first, then keep the first 4 hyphen-separated tokens, so
  `src/main.rs breaks now` yields `src-main-rs-breaks` (4 tokens) rather than
  splitting into 4 raw words that normalize into 5.
- This fixes a latent bug: `--help fails` currently produces branch
  `forge/-help-fails`, because `trim_matches('-')` only strips the ends and the
  leading `-` survives. The leading `-` must be gone.
- The existing `title_from_prompt` tests at `:891`–`:946` pin sentence-cased
  output (`Rewrite the lexer`, `--help fails`, `v1.2.3`, `API key missing`).
  They will fail by design and must be rewritten to the lowercased form —
  including the "preserves code-like openings" test, which now asserts these
  survive *normalization* rather than verbatim casing.

### 2. `forge-types` — `/resume` agreement

`crates/forge-core/src/helpers.rs:481` — `session_title_hint` builds the
`/resume` row with its own 60-char truncation. It must apply the same rule, or
`/resume` and the navigator will disagree about the same session. Prefer reusing
the `forge-types` function over duplicating the logic.

### 3. `forge-session` — the slug stays one function

- `unique_session_branch` (`supervisor.rs:2334`) keeps reading `task.label`;
  that is the single source for the branch name (decision 3).
- `sanitize_label` (`worktree.rs:223`) **stays as the security boundary.** It
  guards other callers too — `create_worktree` (`:372`) builds
  `subagent-<id>-<slug>` for subagents, where labels are model-authored and the
  4-word rule does not apply. Do not weaken it, and do not route subagent labels
  through `title_from_prompt`.
- After step 1 the label is already `[a-z0-9-]`, so `sanitize_label` becomes a
  no-op for session labels while still containing `../` and ref-reserved
  characters from any other caller. Extend
  `sanitize_label_keeps_only_safe_characters` (`:486`) to pin that.
- **`forge/untitled-session`:** a prompt that is all punctuation, or a session
  whose creation has not yet been named, falls back to `"Untitled session"` and
  slugifies into a clean, permanent branch name. This is new exposure — the old
  flow always had a real prompt before a branch could materialize. Accept it, or
  add a short-id suffix to the fallback. **Decide explicitly; do not leave it
  implicit.**

### 4. `forge-session` — auto-select the new session

Selection is **supervisor-authoritative**. `CreateSession` emits `Roster`, then
calls `set_selected`, then emits `Selected(Some(session_id))` — in that order.
The order is load-bearing, not decoration: the TUI's `Selected` arm resolves the
session out of `supervisor.snapshots`, which only the `Roster` arm fills, so a
`Selected` emitted first is a silent no-op.

- Selection happens **before** the `if first_prompt.is_some() && !already_trusted`
  branch. The trust overlay only opens when the new session is already selected;
  selecting afterwards would make the modal unreachable for the one case it
  still exists for.
- After registration, confirm the primary session's role is unaffected.
  `set_selected` writes only `repository_state.selected_session_id`; `ownership`,
  `slot` and `lifecycle` live on the `sessions` row. The primary is the trust
  ancestor and the roster root, and selecting a managed session must not disturb
  it.
- **`rollback_creation` must clear a selection that names the rolled-back
  session**, resetting it to `None` and emitting `Selected(None)`. Selecting
  before trust is confirmed means a cancelled creation would otherwise leave the
  durable selection naming a removed row. `close_session` already produces this
  state.

#### The TUI must not select

A TUI-side selection leg was implemented during development and then removed:
with `CreateSession` selecting authoritatively, a second roster-diff selection in
the TUI is redundant, and in one window actively harmful. `poll_supervisor_events`
stops at `MAX_SUPERVISOR_EVENTS_PER_TICK` (128); if `Roster` lands at the cutoff
and `Selected` on the next tick, a TUI-side "select the newest session not in my
pre-command set" can mis-pick — for instance an `a`-attached session. The TUI
therefore only *focuses*, via `focus_created_session`, and only once the
supervisor has already made the new session selected. That degrades to "no focus
hand-off" rather than to selecting the wrong session.

Composer focus is the one thing the supervisor cannot express: the seeded
first-visit view happens to default to the composer, but nothing states that
intent, so `n` would regress silently if that default ever moved.

### 5. `forge-tui` — `n` creates and jumps

`crates/forge-tui/src/app/input.rs`

- `n` (`:153`) currently sets `navigator_new_session = Some(String::new())` and
  opens an inline composer whose text becomes the first prompt. Change it to
  submit `CreateSession { label: String::new(), first_prompt: None }` directly,
  keeping the existing navigator-tab/focus/peek-setup lines.
- **`n` must not pass typed text as `first_prompt`.** That would re-couple
  creation to trust: `first_prompt.is_some() && !already_trusted` is what parks
  the modal, and reintroducing it puts a modal in front of the operator at a
  moment they are not expecting one.
- Remove the composer buffer machinery: `navigator_new_session`
  (`types.rs:1689`) and its Esc/Backspace/Enter/Char arms (`input.rs:172`–`:205`).
  Remove the `SessionList::new_session` field
  (`widgets/navigator.rs:178`) and the `render.rs:469` argument that feeds it.
- Focus the composer once the session is selected. `restore_session_view_state`
  (`input.rs:602`) already seeds a blank first-visit view from the current model;
  as part of this task it may stop being called only from the `Selected`
  handler, so its callers need a look.

### 6. Tests

- **Rewrite `crates/forge-tui/src/app/tests/multi_task.rs:941`.** The existing
  `n_opens_the_inline_composer_and_starts_a_named_session` asserts the old
  behavior end to end, including `task.label == "Rename the parser"`. It will
  fail by design. Replace with: `n` creates a prompt-less session, the session
  becomes selected, and the composer has focus.
- Add a `forge-session` test: a prompt-less creation then a first prompt yields
  the 4-word label **and** `forge/<4-word-slug>` on the same turn — this is the
  ordering guarantee from decision 4 and the one regression worth pinning.
- Add a `forge-session` test: a prompt-less creation fires no `TrustRequired`
  (extend the existing check at `supervisor.rs:4681`).
- Extend the `forge-types` `title_from_prompt` cases at `:891`–`:946` for the
  4-word cap, hyphen joining, underscore normalization, and the
  `Untitled session` fallback's slug.
- Extend the `forge-storage` `sanitize_label` test at `:486`.
- Check `crates/forge-tui/src/app/tests/commands.rs` for the `/sessions` row
  placeholder copy (`commands.rs:60`).

### 7. Docs

- `FORGE-DESIGN.md:452` — §7.7 says `n` "opens an inline composer whose typed
  task becomes the session's first prompt (and names it)". Rewrite for the new
  binding.
- `FORGE-DESIGN.md §12` (Session Worktrees) — the "never a random UUID" rule now
  describes a prompt-less creation path; state how the branch is named when a
  session is created before its first prompt.
- This document.

## Hazards

1. **Renaming after the turn would break branch naming.** Turn 1 writes a file →
   `materialize_managed_branch` runs → branch is named from the label. If the
   label is still empty at that point the branch is `forge/untitled-session`, and
   because `set_session_branch` is one-way, the branch and the session name
   diverge permanently. Cleanup verifies binding by branch-or-detached
   (`remove_clean_worktree_if_branch` / `_if_detached`), so retitling a branch
   later means reworking binding verification. **Keep the rename on submit.**
   Pinned by a `forge-session` test that creates prompt-less, submits a prompt,
   and asserts both the label and `forge/<same-slug>`.
2. **`n` carrying a first prompt re-pins trust to a modal.** See step 5.
3. **Underscore divergence.** See step 1.
4. **`forge/untitled-session` is permanent.** See step 3.

## Known limits of this change

- **The `sanitize_label` invariant holds only for names ≤ 40 characters.**
  `sanitize_label` truncates to 40 and `title_from_prompt` caps at
  `TITLE_MAX_CHARS = 60`, so a name that is a single over-long token (or an
  adversarially long first word) is shortened on the way into Git and the label
  no longer matches the branch. In practice the 4-token cap keeps ordinary
  prompts well under 40. Closing this properly means aligning the two caps, which
  crosses into the `forge-storage` security bound — deliberately left alone here.
- **`_` is preserved by `sanitize_label` but never produced by
  `title_from_prompt`.** Subagent labels reach the same sanitizer through
  `create_worktree` and are model-authored, so the sanitizer keeps its existing
  character class. The behavior is pinned by test rather than changed.
- **`AttachWorktree` still registers without selecting.** The same class of bug
  as the `CreateSession` one fixed here, and out of scope for this change.

## Out of scope

- Reaping unnamed sessions with empty journals on startup. It would delete
  worktrees and interact with durable rows; deferred with the worktree-space
  requirement.
- Anything that merges a session branch back to its base.
- The storage cost of accumulating empty worktrees.

## Validation

Targeted only — never the full workspace suite.

```sh
cargo test --package forge-types --locked title_from_prompt
cargo test --package forge-storage --locked sanitize_label
cargo test --package forge-session --locked materialize
cargo test --package forge-session --locked trust
cargo test --package forge-tui --locked multi_task
cargo test --package forge-tui --locked commands

cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
```
