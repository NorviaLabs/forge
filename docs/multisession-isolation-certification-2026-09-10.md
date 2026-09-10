# Multisession isolation certification attempt

Date: 2026-09-10

Commit under test: `7aff870` (`test: satisfy workspace clippy in PTY coverage`)

Status: **not certified for complete isolation**

This record covers the implementation slices in
`multisession-isolation-implementation-plan.md`. It records the validation
performed against the identified commit without upgrading the cooperative
worktree contract into a strict security guarantee.

## Automated checks

| Check | Result |
|---|---|
| `cargo fmt --all -- --check` | Passed |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | Passed |
| `cargo test --workspace --all-targets --locked --no-fail-fast` | Partial: all targets passed except the known timing-sensitive `forge-session` test `primary_and_managed_actors_share_hitl_question_and_continue_commands`; its focused rerun passed once |
| `cargo build --release --locked --package forge-cli` | Passed |
| `./target/release/forge --version` | Passed: `forge 0.1.0-beta.10` |

The workspace run exercised the new MCP workspace, dispatcher, event
reconciliation, prompt recovery, retirement, inactive-terminal, watcher, and
Git-mutation tests. The failing supervisor test can leave its cancelled
background shell observed as `Running` under one scheduler interleaving, so
the complete suite is not recorded as green.

## Real-PTY smoke matrix

The release binary was launched in disposable, separately trusted temporary
Git workspaces through real tmux PTYs. Each size reached a stable home frame
after the trust prompt:

| PTY | Observed |
|---|---|
| 160x50 | Two-pane home view, task strip, model/provider/workspace identity, composer, and footer rendered |
| 120x40 | Same home view rendered in a separate workspace without overlap or panic |
| 80x18 | Compact single-pane home view rendered with clipped low-priority identity fields and a usable composer/footer |

This smoke matrix did not run a provider-backed concurrent stream, session
switch, dirty editor, pending approval/question, restart, or storage-failure
scenario. Those remain outside this local PTY record. At 80x18 the trust modal
cannot display every explanatory/control row before the home frame is reached.

## Remaining boundary

Forge currently provides cooperative isolation: session-owned runtime state is
separate, while provider credentials and common Git refs/configuration remain
operator-wide. A tool with explicit Git authority can still affect shared
repository state. Strict isolation still requires separate repository clones
or a brokered Git boundary.

The known supervisor timing failure, the unrun live interaction matrix, and
provider, external MCP service, platform sandbox, and crash-recovery coverage
gaps prevent a complete isolation certification.
