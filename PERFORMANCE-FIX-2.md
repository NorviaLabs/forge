# Performance fix 2: asynchronous replay checkpoints

Worktree: `/Users/mohitranka/Projects/forge/.forge/local/worktrees/subagent-2-Fix-2--asynchronous-checkpoints`

## Scope and design (complete)
Only forge-durable optional checkpoint maintenance changes. Event INSERT remains awaited before append returns. Inspection found checkpoint replay, projection cloning, and serialization awaited every 256 events; the journal pool has one connection and existing tests assume immediate checkpoints.

Implemented solution: a lazily started dedicated maintenance thread with its own Tokio runtime and SQLite connection, plus a bounded one-slot request channel shared across Journal clones. CPU work cannot occupy the caller runtime and maintenance cannot monopolize the append connection. Overflow requests are skipped (checkpoints are optional); future interval boundaries retry. Preserve existing replay and stale-sequence upsert guard. The worker owns its runtime, so caller runtime shutdown cannot strand a blocking task awaiting that runtime. Dropping all senders closes the worker after at most active plus queued work; no join on append/drop.

Tradeoffs: one extra thread/connection per journal that reaches a checkpoint boundary; checkpoint visibility becomes eventual; checkpoint work can finish after the last Journal drops. SQLite write-lock/disk contention remains possible. No event durability or replay format change and no dependency on fix 1.

## Validation
Passed `cargo test --package forge-durable --locked`: 32 unit tests and 3 integration tests; 1 reporting benchmark ignored. Passed `rustfmt --edition 2021 --check crates/forge-durable/src/lib.rs` without formatting changes. Sandbox xcrun cache-write warnings did not prevent compilation or tests.

New tests cover an unconsumed one-slot queue across four checkpoint boundaries (append completes, clone shares queue, all 1024 records replay), checkpoint-plus-suffix correctness, closed-worker failure isolation, stale-sequence protection, and caller-runtime shutdown followed by checkpoint/replay verification on a new runtime. Existing automatic-checkpoint, corrupt-checkpoint fallback, and concurrent append tests now explicitly wait for eventual checkpoint completion.

Maintenance is bounded per opened Journal and its clones, not globally across independently opened Journals. Requests contain only a session ID; a queued pass reads the latest persisted history rather than a captured stale snapshot. A worker startup failure disables optional maintenance for that Journal; event writes/replay remain available. No flush API or shutdown wait is required because the event log remains authoritative.

## Integration
After review, copy `crates/forge-durable/src/lib.rs` and this document into the integration worktree. Do not copy Cargo.lock, build artifacts, or runtime data. Run `cargo test --package forge-durable --locked`. No commits; this worktree is retained. Source-only recovery patch export was attempted, but the git tool rejects `--output`; the retained source is the recovery artifact. If needed, the coordinator can export with `git diff -- crates/forge-durable/src/lib.rs > PERFORMANCE-FIX-2.patch` from this worktree. No fix-1 files or dependencies are included.
