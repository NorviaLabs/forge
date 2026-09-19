# Performance fix 1 recovery

Worktree: `/Users/mohitranka/Projects/forge/.forge/local/worktrees/subagent-1-Fix-1--linear-streamed-arguments`

Status: implemented and validated. No commits. Retain this worktree.

## Complete changed paths

- `/Users/mohitranka/Projects/forge/.forge/local/worktrees/subagent-1-Fix-1--linear-streamed-arguments/crates/forge-model/src/native/openai.rs`
- `/Users/mohitranka/Projects/forge/.forge/local/worktrees/subagent-1-Fix-1--linear-streamed-arguments/PERFORMANCE-FIX-1.md`

## Reasoning

Per-call incremental state tracks nesting, string mode and escaped bytes. Only structurally complete candidates go through serde, avoiding repeated parsing of incomplete object/array/string prefixes. Serde remains authoritative both for completion and final validation. Parsed snapshots reset tracking. Whitespace-only fragments retain existing behavior (including non-JSON Unicode whitespace invalidating a previously complete buffer). Scalars retain legacy complete-prefix replacement semantics; numeric overflow waits for an exponent rather than repeatedly parsing an unbounded overflowing mantissa. Parsed object/array snapshots are also validated to preserve serde recursion-limit behavior.

Targeted differential tests compare every character-boundary two-fragment split against the previous reparsing algorithm for nested JSON, strings, escapes, Unicode, primitives, malformed documents, whitespace and following snapshots. Scaling regression uses deterministic counters: one-character fragments at 1K/4K/16K repeated payload sizes must scan exactly the document byte count and parse exactly one document before unchanged final validation. Existing proxy snapshot, interleaved-call, stream event and final invalid-JSON tests remain passing.

## Validation

- `cargo test --package forge-model --locked native::openai`: PASS, 25 tests, 0 failures.
- `cargo fmt --all -- --check`: PASS.
- macOS xcrun emitted sandbox cache-write warnings; compilation and tests succeeded.

## Integration

Accessible source file is listed above. Coordinator can read it and apply its diff against the common base, or obtain the source-only diff with `git -C /Users/mohitranka/Projects/forge/.forge/local/worktrees/subagent-1-Fix-1--linear-streamed-arguments diff -- crates/forge-model/src/native/openai.rs` and apply that patch in its integration worktree. Copy this recovery document separately if desired. No other fix or unrelated formatting is included.
