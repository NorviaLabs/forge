# Desktop ↔ TUI feature parity

Both frontends drive the same `forge-session` supervisor. The desktop is
deliberately narrower: it owns pointers, multi-file editing and long transcripts;
the TUI owns the keystroke-power surface. Everything below is measured against
code, not against the docs.

Status legend: **yes** = shipped, **partial** = shipped with a limit,
**no** = absent, **n/a** = does not apply to a GUI.

## Desktop scope

- Target: equivalent user workflows, using the shared session/workspace backend.
- Scratchpad is intentionally excluded from desktop parity.
- Keep the desktop editor edit-only and button-free; do not copy Vim modes or
  terminal-specific key grammar.
- A control alone does not establish parity: its complete workflow must work.

## Sessions

Implementation update (2026-11-07): prioritized items 1–17 now have desktop
surfaces. Limits below remain explicit; this is not a claim of complete TUI
parity. Desktop tests and the real-webview self-test cover the core flows.

| Capability | TUI | Desktop | Notes |
| --- | --- | --- | --- |
| Create session | yes | yes | `create_session(label)` |
| Select session | yes | yes | sidebar click |
| Archive session | yes | yes | confirm dialog |
| Archive and remove a managed worktree | yes | yes | archive dialog option; dirty checkouts are retained until the operator ticks discard |
| Rename session | yes | yes | sidebar and inspector |
| Pin session to slot | yes | yes | first free slot; click again to unpin |
| Fork session | yes | yes | inspector and palette |
| Resume another session | yes | yes | closed-session list; requires a current session |
| Close without archiving | yes | yes | sidebar and inspector; the last open session refuses to close |
| Stop another session's turn | yes | yes | stop button on any running row; composer stops the selected one |
| Attach a worktree | yes | yes | sidebar/palette dialog; supervisor validates repository membership and branch; attached checkouts are never removed |
| Auto-named labels | yes | **partial** | desktop sends the label it is given |
| Queue: view | yes | yes | durable and actor queue rows |
| Queue: cancel one | yes | yes | source + stable ID; never cancels a replacement by position |
| Queue: edit / recover interrupted | yes | yes | moves the prompt into the composer after cancelling |

## Turn control

| Capability | TUI | Desktop | Notes |
| --- | --- | --- | --- |
| Submit prompt | yes | yes | |
| Stop turn | yes | yes | |
| Continue after a stop | yes | yes | inspector / palette |
| Approve once / deny | yes | yes | `ApprovalCard` |
| Approve-all mode | yes | yes | session-only inspector toggle |
| Typed grant decision (once / session / always, feedback) | yes | yes | Rust-supplied options; privileged choices need a second confirmation |
| Clear session approvals | yes | yes | inspector when rules exist |
| Grant egress host | yes | **partial** | grants on pending host requests; no proactive host editor |
| **Answer `ask_user_question`** | yes | yes | choices, custom answers, skip; backend validation |
| Thinking toggle | yes | yes | inspector |
| Compact context | yes | yes | inspector / palette |
| Image attachments | yes | yes | paste into composer, 4 × 5 MiB maximum; backend content validation |
| Attach the open file to a prompt | yes | yes | palette toggles it; cursor line and 150-line excerpt |
| Reasoning and full tool output | yes | yes | reasoning in a collapsible block; tool rows expand past the 2k snapshot limit |
| Background agents/tasks list | yes | yes | inspector; child transcript reader and result insertion |
| Cancel background task / run | yes | yes | run-scoped: a stale click cannot stop a replacement that reused the task id |
| Child-agent approval | yes | yes | exact `request_id` + `call_id`; arguments shown; privileged choices require confirmation |
| Trust prompt for an untrusted workspace | yes | yes | deferred launch; workspace writes begin only after consent |

## Context and status

| Capability | TUI | Desktop | Notes |
| --- | --- | --- | --- |
| Context usage meter | yes | yes | inspector |
| Model + effort picker | yes | yes | `⌘M` |
| Provider connect / add model | yes | **partial** | custom provider add/edit/remove; reuses a Codex/OpenCode/Grok sign-in found on the machine; typed keys win over environment variables; native hidden key prompt on macOS; OAuth browser flow has no cancel (it expires) |
| Disconnect provider | yes | yes | explicit confirmation; environment credentials remain unchanged |
| Token counts, cache hits, tool count, skills | yes | yes | inspector and expanded status; cost/usage via existing provider service |
| Plan view | yes | yes | newest `plan_update` checklist |
| Clear the transcript view | yes | yes | palette; view-only, the model keeps its context |

## Files and editing

| Capability | TUI | Desktop | Notes |
| --- | --- | --- | --- |
| Browse workspace | yes | yes | tree, status and last-turn files poll every 3s while visible |
| Open file / edit / save | yes | yes | edit-only by product decision |
| Stale-write refusal | yes | yes | fingerprint |
| Reload a stale buffer in place | yes | yes | command palette; unsaved reload requires app-level discard confirmation |
| Binary / non-UTF-8 / oversized guard | yes | yes | read-only banner |
| CRLF preservation | yes | yes | |
| Create / rename / delete file | yes | yes | confined paths; deletion only files or empty folders |
| Content search across files | yes | yes | literal case-insensitive search, capped at 200 hits; a hit opens at its line |
| Find in file, jump to line, replace | yes | yes | shortcuts and command palette; replace one or all from the find bar; literal, case-insensitive |
| Inline diff of working tree / staged / last turn | yes | yes | palette chooses the side; last turn comes from the transcript's diff cards |
| Discard a file's changes | yes | yes | one path, or one combined-view hunk at a time |
| Syntax highlighting | yes | **partial** | lexical colors for common code/config; plain above 64 KiB; edit mode only |

## Git

| Capability | TUI | Desktop | Notes |
| --- | --- | --- | --- |
| Status / diff | yes | yes | |
| Stage / unstage a file | yes | yes | inspector |
| Commit (message + validation) | yes | yes | staged files or stage-all; empty message/index refused |
| Stage-all-and-commit prompt | yes | yes | commit dialog offers "stage every change" |
| Branch list / switch / merge | yes | yes | unsaved buffers block switching/creation |
| Rename / delete branch, abort merge | yes | yes | branch dialog; merged-branch deletion refuses without force |
| Fetch / pull / push | yes | yes | inspector and palette; typed verbs only |
| GitHub issues, PR, CI | yes | yes | issue-linked sessions, explicit push/PR, refresh and feedback, checks and bounded logs; requires gh |

## Shell and app chrome

| Capability | TUI | Desktop | Notes |
| --- | --- | --- | --- |
| Interactive shell | yes | yes | xterm.js, per session |
| Shell survives a session switch | yes | yes | per-session mounted emulator; real-webview regression test |
| Resize pane geometry | yes | n/a | window resize |
| Command palette | yes | yes | `⌘K` |
| Help / key reference | yes | yes | sidebar Help and palette |
| Theme picker | yes | **partial** | Forge Dark/Light only, local preference; no installed/system themes |
| Notifications when a session needs attention | yes | **partial** | sidebar attention badge and document title; no OS notification/dock integration |
| Quit guard | yes | yes | unsaved buffers, running/waiting turns and queued work |
| Multi-file tabs | no | yes | desktop advantage |
| Mouse selection / scrollback | partial | yes | desktop advantage |

## Prioritized gaps

The numbered list below records the first implementation pass, not completed
parity. Remaining work is tracked here:

- [x] Git: truthful staged/worktree review, stage-all commit, remote sync,
  branch deletion, merge abort, hunk and last-turn review.
- [x] Sessions: durable close/resume, per-session views, worktree attach and
  managed-worktree cleanup with dirty retention.
- [x] Agents/queue: child reader, result insertion, run-scoped cancellation,
  queued-text editing and interrupted-prompt recovery.
- [x] Providers/models: custom provider add/edit/remove, synchronized model
  capabilities, reusing another CLI's completed sign-in, and a typed key
  winning over environment variables for the process.
- [x] GitHub: issue-linked sessions, explicit PR creation and feedback workflows.
- [ ] Context/transcript: attachment/image inspection in history.
- [ ] Chrome: installed/system themes and OS attention notifications.
- [ ] Verify workflow correctness and safety regressions before claiming parity.

Ordered by "how badly does the missing piece break the product", not by size.

### P0 — the turn cannot finish

1. **Answer `ask_user_question`.** `SessionSnapshot.pending_question` exists and
   `ResolveQuestion` is ready, but `DesktopEvent::Session` drops it, so the GUI
   shows a session that looks idle while it waits forever. Fix: carry
   `pending_question` in the `Session` event, render a question card
   (single/multi choice, free text), send `ResolveQuestion`. Small; nothing else
   unblocks a stalled turn.

### P1 — correct, safe, and recoverable

2. **Approve-all + typed grant decisions.** `SetApproveAll` and `DecideApproval`
   already exist; the GUI only has once/deny, so long agent runs stop every few
   seconds. Also `ClearSessionApprovals` and `GrantEgressHost` for the escape
   hatches.
3. **Session lifecycle commands.** `ContinueTurn`, `CloseSession`, `ForkSession`,
   `ResumeSession`, `RenameSession`, `PinSession`. Each is a small IPC wrapper plus
   one menu item.
4. **Reload a stale buffer.** Right now the only recovery from "changed on disk"
   is close-and-reopen, which loses the tab. Add `read_file` re-fetch on the
   refusal, with an explicit discard confirmation.
5. **Attention for sessions you are not looking at.** The supervisor already
   emits `Attention`; the sidebar should badge it and the window should stop
   being silent (title/dock). Otherwise parallel sessions are a trap: work sits
   waiting on a decision in a hidden tab.

### P2 — the daily workflow is cut in half

6. **Git writes: stage, unstage, commit, branch switch/merge.** The GUI can read
   git but never finishes the loop the agent started, so users drop to a terminal
   mid-task.
7. **File create / rename / delete + workspace search.** `write_file` already
   refuses paths outside the workspace; extend the same confinement.
8. **Background tasks and child approvals.** Without it, parallel agents exist in
   the supervisor and are invisible in the GUI — the exact failure mode a desktop
   app should make better than the TUI.
9. **Context commands: `/status`, `/context`, `/compact`, `/plan`, `/thinking`.**
   Mostly rendering fields the snapshot already carries; `update_plan` events are
   currently dropped.
10. **Trust prompt in-app.** `TrustRequired` is emitted and ignored; a first-run
    on a new repo dead-ends into "open a terminal first".
11. **Keep the PTY alive across session switches.** Recreating a shell per switch
    loses running commands, which is a data-loss-shaped bug for long builds.

### P3 — parity polish

12. Provider connect / add model / disconnect.
13. Editor find + jump-to-line + syntax highlighting.
14. GitHub issues / PR / CI.
15. Image paste into the composer.
16. Token/cost/tool/skill detail, theme picker, help page.
17. Queue rows with per-prompt cancel.

## Not worth copying from the TUI

Vim grammar, `Ctrl-*` global chords, `/resize`, and the terminal overlays are
keyboard workarounds; a GUI should not grow a second modal vocabulary to reach
the same state.

## Start here

Validate live provider sign-in, GitHub account access and native visual behavior
before release. Automated checks do not exercise real credentials or billing.
