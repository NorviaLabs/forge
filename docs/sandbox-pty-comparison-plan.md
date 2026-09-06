# Sandbox PTY comparison and implementation plan

Date: 2026-09-06

This review was run from Forge `main` at `92198d1159841914c6422142e3129d2b5a08191d` using the rebuilt `target/debug/forge`. The command scenarios ran inside tmux PTYs, and every claimed filesystem result was checked from the host after the agent process exited.

## Observed behavior

| Scenario | Forge | Codex | OpenCode |
| --- | --- | --- | --- |
| Write inside the workspace | Succeeds. | Succeeds under `workspace-write`. | Succeeds. |
| Write outside the workspace | Blocked with `approval required` and a filesystem-specific sandbox explanation; the host-side file was absent. | Blocked with `operation not permitted` under `workspace-write`; the host-side file was absent. | Succeeds under `run --auto`; the host-side file was present. |
| Network request to `www.iana.org` | Blocked with a host-specific explanation: the destination is not in personal `host(...)` permissions. | Failed as `curl: (6) Could not resolve host`; the explanation did not identify the sandbox. | Succeeded under `run --auto`, downloading 6,253 bytes. |
| Suppressed filesystem failure (`2>/dev/null || true`) | Still escalates as a sandbox denial; no outside file was created. | A suppressed failure can appear successful to the agent; the direct unsuppressed case exposed the OS denial. | Appears successful because the command is unrestricted in `--auto`. |
| Pipeline with an outside write | The current denial-attribution tests and headless path preserve the denial even when the final pipeline status is zero. | A pipeline can report exit zero, so the agent can describe it as successful unless the side effect is checked. | Pipeline and side effect both succeeded in `--auto`. |

The Codex `/tmp` result was intentionally treated as a control correction: Codex's `workspace-write` posture includes `/tmp`, so an earlier test that wrote under `/private/tmp` did not test an outside path. The corrected path under `/Users/mohitranka/Projects` was denied.

Forge's current interactive startup also adds three steps before a task can run in a fresh isolated home: theme selection, folder trust, and provider selection. Codex shows model, directory, approval policy, and sandbox posture in its startup header. OpenCode shows the active model and context state in its footer and exposes `--auto` at the CLI.

## Bugs and friction areas

### P0: no regression found in the tested confinement boundary

The latest `main` correctly prevented the tested outside writes and denied unapproved network access. The prior zero-status redirection and stale-host attribution failures appear fixed. Keep these cases as live PTY regressions because unit tests alone cannot prove the visible tool result and host-side side effect agree.

### P1: `exec_command` advertises PTY and login modes that always fail

`ExecCommandArgs` exposes `tty` and `login`, but `unified_exec.rs` rejects both with “not supported yet.” This is a contract bug for agents that use the persistent shell interface, and it prevents Forge from matching the interactive command behavior users expect from Codex and OpenCode. The fix needs a real PTY-backed session, terminal resize and EOF handling, and the same spawn-time sandbox policy as the pipe-backed session.

### P1: policy controls are too coarse

The TUI has no visible sandbox mode selector. The headless command has only `Ask` and blanket `--approve-all`; there is no explicit read-only mode, network-only grant, or deliberate full-access mode. This makes safe automation harder to express and makes `--approve-all` do too much when a test needs only one host or one command family.

### P1: the network denial is clear in Forge, but recovery is still turn-hostile

Forge gives the model a useful host-specific error and can grant a host while keeping the retry confined. In headless mode, however, an unanswered denial terminates the run immediately. There is no structured “blocked, awaiting policy” result that a caller can inspect and resume, and no command-line way to grant one host for one run.

### P2: sandbox posture is not continuously visible

Forge's steady-state chrome shows model and workspace state, but it does not keep the active filesystem and network policy visible. Users must infer the boundary from documentation or wait for a denial. Codex makes the sandbox and approval posture visible at launch, while OpenCode makes its dangerous `--auto` choice explicit at the command line.

### P2: the first-run path delays the security model

Theme, trust, provider, and model setup are separate overlays. They are understandable individually, but the user reaches the task surface before seeing a concise summary of “workspace writes allowed, network denied, `.git` read-only.” A compact startup policy summary would reduce uncertainty and make PTY testing easier.

### P2: error wording differs by execution path

The one-shot shell path and persistent `exec_command` path share the same denial classifier, but their output and retry behavior are separate. Add one canonical policy-result structure so a filesystem denial, host denial, ordinary command failure, and user rejection render consistently in TUI, headless JSON, and journal records.

## Recommended implementation order

### 1. Preserve the current boundary with a reusable PTY matrix

Add a disposable PTY test harness under `scripts/` or the existing integration-test area. It should run the same scenario matrix against Forge, and optionally Codex and OpenCode when those binaries are installed:

1. workspace write;
2. `.git` write;
3. outside write with visible stderr;
4. outside write with `2>/dev/null || true`;
5. outside write hidden behind a successful pipeline;
6. denied network with visible stderr;
7. denied network with suppressed stderr and a swallowed status;
8. allowed host after a session grant;
9. ordinary `false` after a denied host;
10. persistent shell start, poll, stdin, exit, and cleanup.

Record the rendered PTY output, exit status, and host-side side effects separately. Add a machine-readable result format so CI can run the Forge rows without requiring Codex or OpenCode.

### 2. Make the policy explicit and orthogonal

Introduce a typed runtime policy with independent fields for:

- filesystem: `read-only`, `workspace-write`, or explicitly unconfined;
- network: denied, session host allow-list, or personal unrestricted host allow;
- approval: ask, session rule, or headless fail/auto behavior.

Keep the OS sandbox as the floor for the default and for ordinary approvals. Require an explicit CLI flag and a visible TUI confirmation for unconfined execution. Keep repository permissions unable to widen a personal network or filesystem policy.

Expose the same policy through the TUI, `forge bench`, session state, and the startup/status chrome. Replace `--approve-all` with a compatibility alias that clearly states whether it approves a tool request, grants a host, or retries unconfined.

### 3. Implement a real PTY-backed `exec_command`

Create a terminal-session abstraction shared by the current pipe-backed executor and the PTY executor. Apply sandboxing before the PTY process starts, retain invocation-local egress state, and route `write_stdin` through the session object. Define behavior for resize, EOF, child exit, cancellation, output truncation, and denied commands.

Do not silently fall back from requested PTY mode to a pipe. Return a validation or execution error that names the unsupported platform only when the platform genuinely cannot provide the requested mode.

### 4. Improve denial recovery and presentation

Use one policy result for filesystem denial, denied host, ordinary command failure, user rejection, and approved retry. In the TUI:

- show the active policy on the approval/host-grant row;
- distinguish “grant this host while staying sandboxed” from “retry outside the sandbox”;
- keep the turn resumable after a denial;
- show the exact next action for a blocked headless run.

In headless mode, emit a JSON event with the denial kind, command, host when known, retry scope, and side-effect guarantee. Add a flag for fail-fast versus returning a resumable blocked result.

### 5. Validate and document the contract

Add focused tests in `forge-tools` for the policy matrix, PTY sessions, egress isolation, pipeline attribution, and retry environment cleanup. Add CLI parsing and JSON-schema tests in `forge-cli`, rendered approval and status tests in `forge-tui`, and one full-workspace validation pass.

Update README and the system prompt with the actual policy vocabulary, the `/tmp` or session-temp boundary, the network grant behavior, and the difference between a sandbox grant and an unconfined retry. The documentation should include the safe PTY reproduction commands used by the regression harness.

## Acceptance criteria

- Every matrix row reports both the model-visible result and the independently observed side effect.
- A zero-status shell construct cannot hide a denied filesystem or network operation.
- A host grant retries the same command inside the filesystem sandbox and never leaks the sandbox proxy into an unconfined retry.
- Requested PTY sessions work for supported shells, preserve interactive input/output, and remain confined from spawn through exit.
- TUI and headless modes expose the same policy concepts and denial kinds.
- The default path remains workspace-confined with network denied until a personal or session host grant exists.
- `cargo fmt --all -- --check`, focused crate tests, locked clippy, workspace tests, and the PTY matrix pass before delivery.
