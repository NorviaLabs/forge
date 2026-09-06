# Sandbox and PTY implementation plan

This plan turns the policy clarified in PR #525 into implementation work. It is based on `origin/main` at the planning baseline and keeps the product scope to one supported policy.

## Product contract

Forge has one supported execution policy in this work: `sandboxed`, enabled by default.

- Every model-authored command starts inside the OS sandbox.
- Filesystem access is confined to the workspace and session temp directory.
- `.git` and `.forge` retain their existing protected behavior.
- Network access starts denied and is reopened only after the user approves the requested host.
- A filesystem sandbox denial can be approved for the blocked command to retry unconfined. This includes destructive commands; the approval is explicit and scoped to the retry or the existing remembered approval rule.
- A network approval grants the requested host through the filtered egress path and keeps the filesystem sandbox active.
- Ordinary governance approval and sandbox approval remain distinguishable in the tool result and UI.

The bypassing `--approve-all` policy is out of scope. It must not gain a new TUI mode, new runtime behavior, or additional documentation in this work. `forge bench` currently contains an existing `--approve-all` option and `ApprovalPolicy::ApproveAll` plumbing; that pre-existing evaluation path needs a separate decision to remove, quarantine, or retain. This implementation must not broaden it or use it as the default behavior.

## Workstream 0: freeze the contract and baseline

Create the implementation branch from the latest `origin/main`. Before code changes, add a short design note or issue comment recording the policy above and the explicit exclusions: read-only mode, danger-full-access mode, a sandbox mode picker, and new `--approve-all` behavior.

Resolve the existing headless flag mismatch as a separate decision before changing approval code:

1. preferred: mark `--approve-all` evaluation-only and keep it out of user-facing policy/status documentation; or
2. remove the flag and its CLI test in a separate cleanup change if the evaluation path is no longer needed.

Do not silently change its security behavior as part of the sandboxed-policy work.

## Workstream 1: define one policy result and preserve current approval semantics

The current execution paths already distinguish `ToolError::SandboxDenied`, denied hosts, ordinary tool failures, and human approval. Make that distinction explicit at the boundary shared by one-shot shell and persistent shell execution.

Likely ownership:

- `crates/forge-tools/src/lib.rs`: retain the existing error variants and add only the structured fields needed by all execution paths.
- `crates/forge-tools/src/egress.rs`: keep host denial invocation-local and preserve the filtered retry path.
- `crates/forge-tools/src/builtins.rs` and `unified_exec.rs`: return the same denial metadata for one-shot and persistent commands.
- `crates/forge-core/src/session/tools.rs`: preserve the current rule that a filesystem denial may retry unconfined after explicit approval, while a denied host remains confined after host approval.
- `crates/forge-tui/src/app/approvals.rs`: render the two approval meanings separately.

Required behavior:

- A denied filesystem operation cannot be converted into success by `|| true`, a successful pipeline stage, or suppressed stderr.
- A denied host is attached to the invocation that caused it and cannot contaminate a later command.
- An approved unconfined retry does not inherit the sandbox proxy or confined credentials.
- A user rejection never grants a host or starts a retry.
- Destructive commands remain eligible for explicit approval; the sandbox is the enforcement floor rather than a command blacklist.

Add focused unit tests before changing the UI so later failures identify policy regressions rather than rendering differences.

## Workstream 2: implement `exec_command` PTY mode

`ExecCommandArgs` currently accepts `tty` and `login`, while `unified_exec.rs` rejects both. Correct this contract in two steps.

### 2a. PTY mode

Implement `tty: true` using a PTY-backed session abstraction. Keep the existing pipe-backed path for `tty: false`, but share policy setup, session ownership, denial classification, and output limits.

The PTY session must:

- allocate the PTY before spawning the already-wrapped sandbox command;
- attach the child process to the PTY slave;
- expose PTY master input through `write_stdin`;
- preserve partial output and polling through `yield_time_ms`;
- handle EOF, child exit, cancellation, and dropped sessions;
- support terminal resize when the caller supplies dimensions, or define and test a fixed default if dimensions are not yet in the tool schema;
- merge or classify PTY output without losing denial diagnostics;
- clean up the egress invocation and session temp directory on exit.

Choose the smallest cross-platform implementation that supports macOS and Linux/WSL2. Evaluate an existing PTY crate before adding platform-specific code. If the selected dependency uses blocking file descriptors, isolate its reads and writes behind a bounded blocking bridge rather than blocking the Tokio executor.

Do not fall back silently from requested PTY mode to pipes. If a platform cannot provide the requested mode, return an explicit unsupported-mode error.

### 2b. Login shell decision

Do not implement login shells automatically. A login shell sources user startup files and can reintroduce credentials or unconfined environment state. Either remove `login` from the exposed schema until a sanitized design exists, or implement it only after documenting:

- which startup files are allowed;
- how provider credentials and proxy variables are removed or replaced;
- how the sandbox wrapper remains the actual parent of the shell;
- tests proving the login path cannot widen filesystem or network access.

The preferred first implementation removes the misleading advertised option and leaves login-shell support as a later issue.

## Workstream 3: make headless and TUI recovery resumable

### Headless

`forge-session/src/headless.rs` currently has `Ask`, `DenyAll`, and `ApproveAll`, and `forge-cli` exposes `--approve-all`. For the supported sandboxed policy:

- keep `Ask` as the default;
- keep denial details structured in the returned error/event;
- report tool, command, host, denial kind, retry scope, and whether filesystem confinement remains active;
- make the JSON result distinguish “human approval required” from “command failed” and “host denied”;
- do not add a new headless bypass switch.

The caller should be able to inspect a blocked result without mistaking it for a successful model turn. Resuming or supplying an approval token can be designed separately if the existing headless API cannot support it without broadening scope.

### TUI

Update the existing approval presentation rather than adding a policy picker:

- show `sandboxed` in the status or approval context;
- label filesystem approval as “approve this command retry”;
- label network approval as “allow this host while staying sandboxed”;
- show whether the choice is once, session-scoped, or persisted personally;
- keep the current deny path resumable and visible;
- preserve destructive-command context in the approval payload.

Likely files are `crates/forge-tui/src/app/approvals.rs`, conversation approval rendering, chrome/status rendering, and their focused tests.

## Workstream 4: add the PTY regression matrix

Build one disposable Forge-only harness. Optional Codex/OpenCode rows may run locally when those binaries exist, but CI must not depend on them.

Each case records rendered PTY output, process exit state, tool-visible result, and host-side side effect independently:

1. write inside workspace;
2. write `.git` and `.forge`;
3. read and write outside workspace;
4. outside write with `2>/dev/null || true`;
5. outside write behind a successful pipeline;
6. denied network with visible stderr;
7. denied network with suppressed stderr and swallowed status;
8. approve a host and confirm the retry remains filesystem-confined;
9. approve a filesystem denial and confirm only the approved command runs unconfined;
10. run an ordinary `false` after a denied host and confirm no stale denial is attached;
11. start a persistent pipe session, poll it, send stdin, and collect exit;
12. repeat the session cases with `tty: true`;
13. close a running PTY and verify child and proxy cleanup.

Use unique disposable workspaces outside the repository. Never use the repository's own `.forge` runtime data as a fixture.

## Workstream 5: documentation and validation

Update the README and system prompt only for the supported `sandboxed` policy:

- default filesystem boundary;
- default network denial;
- host approval behavior;
- approved unconfined command retry behavior;
- destructive commands and explicit approval;
- PTY command behavior and limitations;
- the deferred status of `--approve-all`.

Validation gates:

1. `cargo fmt --all -- --check`;
2. focused `forge-tools` unit and sandbox enforcement tests;
3. focused `forge-session` headless tests;
4. focused `forge-tui` approval/status tests;
5. locked clippy for changed crates;
6. locked workspace tests;
7. the live PTY matrix with independent host-side checks;
8. `git diff --check` and a clean diff review for accidental policy expansion.

## Delivery order

1. Contract note and existing `--approve-all` scope decision.
2. Shared policy-result and retry regression tests.
3. PTY-backed `exec_command` implementation.
4. Headless structured results and TUI approval/status presentation.
5. Live PTY matrix, documentation, and full validation.

Keep each step independently reviewable. The PTY implementation should land before changing approval presentation so the UI is built against the final session lifecycle. Documentation should land with the behavior it describes.

## Definition of done

- The default policy is always `sandboxed` and remains the only supported product policy.
- Approved filesystem retries can run destructive commands without weakening unrelated calls.
- Approved network calls use the filtered host grant and retain filesystem confinement.
- `tty: true` works for supported platforms, or is rejected before execution with an accurate capability error.
- `login` is either safely implemented and tested or removed from the exposed schema.
- No new `--approve-all` behavior, full-access mode, read-only mode, or sandbox picker is added.
- Denials and approvals are consistent across one-shot commands, persistent sessions, TUI, headless output, and journal events.
- The focused tests, full validation, and live PTY matrix pass.
