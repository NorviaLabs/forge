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
| Image attachments | yes | yes | paste into composer, 4 × 5 MiB maximum; backend content validation; history chips reopen the image |
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
| Browse long transcript history | yes | yes | desktop renders 100 messages per page; Older, Newer and Latest retain access to the full history |

## Files and editing

| Capability | TUI | Desktop | Notes |
| --- | --- | --- | --- |
| Browse workspace | yes | yes | tree, status and last-turn files poll every 3s while visible |
| Open file / edit / save | yes | yes | edit-only by product decision |
| Stale-write refusal | yes | yes | fingerprint |
| Reload a stale buffer in place | yes | yes | command palette; unsaved reload requires app-level discard confirmation |
| Binary / non-UTF-8 / oversized guard | yes | yes | read-only banner |
| Image preview and image attachments | yes | yes | file pane renders PNG/JPEG/GIF/WebP up to 5 MiB; transcript chips reopen pasted and `view_image` images |
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
| Theme picker | yes | yes | built-ins, `~/.config/forge/themes` and workspace `.forge/themes`; System follows the OS. Preference is local to the desktop |
| Notifications when a session needs attention | yes | yes | sidebar badge and document title; when the window is unfocused, the OS is asked for attention (dock/taskbar). No notification-centre banner |
| Quit guard | yes | yes | unsaved buffers, running/waiting turns and queued work |
| Multi-file tabs | no | yes | desktop advantage |
| Mouse selection / scrollback | partial | yes | desktop advantage |

## Remaining gaps

Scratchpad stays out by product decision. Every other TUI workflow now has a
desktop surface. What is left is explicit limits, not missing workflows:

- **Auto-named labels** are sent by the desktop but not derived from the first
  prompt the way the TUI derives them.
- **Egress hosts** can be granted when a pending request asks; there is no
  proactive host editor.
- **Provider connect** covers custom providers, reused CLI sign-ins and typed
  keys; an OAuth browser flow has no cancel and expires on its own.
- **Syntax highlighting** is lexical for common code/config, plain above 64 KiB,
  and edit-mode only.
- **Attention** badges the sidebar and title and asks the OS for attention while
  unfocused; no notification-centre banner is raised.
- **Scratchpad** is intentionally absent.

Before release, verify the live workflows that automated checks cannot cover:

- [ ] Real provider sign-in, billing/usage and GitHub account access.
- [ ] Theme switching (including System) and installed/user/workspace themes.
- [ ] Managed worktree attach, archive-with-removal, dirty refusal and discard.
- [ ] Image preview and pasted-attachment reopening in a real webview.
- [ ] Native confirmation dialogs and OS attention on an unfocused window.

The desktop command surface is kept in step with the TUI by tests: every IPC
command is listed in `commands_list.rs`, `capabilities/main.json`,
`isolation/index.html` and `startup.rs`, and config tests fail if one is missing.

## Not worth copying from the TUI

Vim grammar, `Ctrl-*` global chords, `/resize`, and the terminal overlays are
keyboard workarounds; a GUI should not grow a second modal vocabulary to reach
the same state.

## Start here

Validate live provider sign-in, GitHub account access and native visual behavior
before release. Automated checks do not exercise real credentials or billing.
