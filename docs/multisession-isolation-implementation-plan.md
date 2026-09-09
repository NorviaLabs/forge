# Multisession isolation implementation plan

This plan turns the 2026-09-09 multisession isolation audit into ordered,
reviewable changes. The audit found that session-owned transcript, model,
tool, persistence, and TUI state are structurally separate, but it did not
certify workspace binding, dispatcher availability, event recovery, queue
recovery, resource retirement, inactive-session servicing, or Git authority.

## Isolation contract

Forge uses cooperative isolation within one trusted repository operator:

- Each session owns its transcript, model choice, context, approvals,
  questions, cancellation, background tasks, agent shell registry, journal,
  TUI view state, and workspace-bound tool context.
- A managed session's MCP process is started in that session's workspace and
  receives only the documented session environment. MCP servers remain
  external programs; their own remote services and side effects are outside
  Forge's local workspace guarantee.
- Provider credentials and the model catalog are operator-wide. They are shared
  services, not session or tenant boundaries.
- Git worktrees isolate checked-out files and per-worktree indexes. The common
  repository directory, refs, objects, and repository configuration remain
  shared Git authority. Forge serializes and ownership-checks Forge-managed
  repository mutations; a tool explicitly granted Git access can still affect
  shared repository state, so this mode is not a strict security boundary.
- Strict isolation requires separate repository clones or a broker that owns
  every Git mutation. It is outside this implementation slice and must not be
  implied by the word “isolated” in product copy.

## Ordered implementation slices

Each slice is completed and validated before the next one begins. Tests are
part of the slice, and the final matrix is only considered passing when the
result is recorded against a clean commit.

| Order | Finding | Deliverable | Acceptance evidence |
|---|---|---|---|
| 0 | Contract | This document and user-facing boundary language | Source/docs describe cooperative worktree isolation and shared Git authority |
| 1 | F1 | Bind every MCP stdio child to the assembled session workspace; carry the context needed by calls | Two fake servers report distinct cwd values and write only their own workspace sentinel |
| 2 | F2 | Move long per-session supervisor work out of the shared command receive loop while preserving actor ordering and tracked shutdown | A blocked operation in A does not delay B submit/select/approval; failure and shutdown release operation state |
| 3 | F3 | Make broadcast lag trigger authoritative UI reconciliation and keep recoverable control state independent from disposable stream deltas | Overflow from A converges B's roster, selection, trust, and question/approval state without a second user action |
| 4 | F5 | Persist prompt dispatch attempts and reconcile claims that cannot complete | Crash/error points before permit, around message append, and after tool execution leave explicit interrupted/ambiguous state |
| 5 | F7 | Add a retirement handshake for actor work, background jobs, agent shells, operator terminals, watchers, and dirty editors | Archive/remove refuses active resources or drains them through an explicit lifecycle; removed sessions release saved UI state |
| 6 | F6 | Service all live operator terminals with bounded, fair draining independent of selection | A noisy inactive terminal finishes and its output remains attached to its session |
| 7 | F8 | Bound/coalesce watcher notifications and refresh inactive workspaces on selection | Sustained inactive churn stays bounded and refreshes the correct workspace |
| 8 | F4 | Encode and test the cooperative Git boundary and serialize Forge-managed mutations | Sibling ref/config/worktree operations have explicit ownership/serialization behavior; strict isolation remains documented as unsupported |
| 9 | Certification | Add the two-session integration and PTY matrix from the audit | Focused tests, workspace checks, and PTY captures at 160x50, 120x40, and 80x18 are recorded |

## Test matrix

The integration matrix uses two disposable sessions with distinct workspace,
transcript, and model markers. It covers model/effort changes, shell-handle
registry rejection, approvals, questions, cancellation, background work,
resume/fork, archive/restore, dirty-worktree cleanup, restart/storage
failures, and concurrent switching. It also verifies that only documented
shared resources cross the session boundary.

The PTY pass covers concurrent streaming, switching, dirty editors, terminals,
pending interactions, and the minimum supported dimensions. The certification
record must include the exact commit, commands, dimensions, and any remaining
platform/provider limitations.

## Explicit exclusions

Provider-specific caches, delegated-agent internals, remote MCP service state,
all platform sandbox implementations, and arbitrary crash points are not
fully certified by the local changes. New coverage should name those limits
instead of treating a passing unit test as proof of strict isolation.
