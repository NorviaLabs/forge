# Performance Fix 3

Worktree: `/Users/mohitranka/Projects/forge/.forge/local/worktrees/subagent-3-Fix-3--cached-transcript-projections`

Status: implemented and targeted tests passed. No commits. Retain this isolated worktree.

## Implementation

A separate owned projection survives presentation render-cache invalidations. The render path takes ownership rather than cloning historical items, attaches transient presentation decorations, renders, removes those decorations, and returns the projection to its cache. Content identity includes session UUID, transcript revision, visible message offset/count, and lifecycle. Revisions deliberately rebuild through the existing full projector, preserving its cross-message tool lookup, grouping, plan evidence, repair/failure, compaction and reset behavior. Summary records remain attached fresh on each render-cache miss; no fix-4 metadata cache was added.

## Changed files / integration

Copy ONLY these files from this worktree to the coordinator checkout (no cherry-pick needed):
- `crates/forge-tui/src/app/new.rs`
- `crates/forge-tui/src/app/render.rs`
- `crates/forge-tui/src/app/types.rs`
- `crates/forge-tui/src/app/tests/conversation_cache.rs`
- `PERFORMANCE-FIX-3.md`

No openai.rs, durable lib.rs, transcript crate, or dependency changes. Fix 4 may also edit render.rs/types.rs, so integrate this first or merge those hunks deliberately.

## Validation

- `cargo test --package forge-tui --locked conversation_cache --lib --quiet`: 11 passed.
- `cargo test --package forge-tui --locked conversation::tests --lib --quiet`: 93 passed (existing rendering/grouping/plan coverage).
- `cargo fmt --all -- --check`: passed.

New regression uses 1,000 historical messages: forced presentation misses/width changes retain both the projected item allocation and historical text allocation (deterministic no-reconstruction/no-deep-copy check). Append, same-length replacement, viewport clear offset, and reset results are compared directly with full projection. Existing same-length invalidation test now checks actual projected replacement text.

Sandbox emitted non-fatal xcrun cache-write warnings; compilation and tests succeeded.

## Residual costs / deliberate limits

Append and any transcript revision still rebuild the full projection: O(history plus payload bytes). This conservative fallback avoids implementing a second stateful tool/plan/grouping reducer, including retroactive tool-call lookup and trailing group changes. This is cached, not incremental append projection.

Presentation misses no longer invoke from_messages or deep-copy projected payloads. They still perform existing rendering/block construction, home/activity decoration scans and vector shifts, and per-frame metadata scans. The latter remain fix 4's scope. This is not a claim that the entire render path is O(viewport). Cached projection retains one additional owned representation of historical payloads.
