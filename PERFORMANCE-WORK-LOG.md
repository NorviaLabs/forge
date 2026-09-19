# Performance fixes — recovery log

Requested: implement four performance findings sequentially using subagents in separate worktrees. No commits requested. Preserve every patch and record integration/test status here.

## Queue
1. Streamed tool-argument parsing: integrated from `.forge/local/worktrees/subagent-1-Fix-1--linear-streamed-arguments`; report `PERFORMANCE-FIX-1.md`; 25 targeted tests and formatting passed in worktree.
2. Durable checkpoint maintenance off critical path: integrated from `.forge/local/worktrees/subagent-2-Fix-2--asynchronous-checkpoints`; report `PERFORMANCE-FIX-2.md`; 35 tests and file formatting passed in worktree.
3. Transcript projection caching: integrated from `.forge/local/worktrees/subagent-3-Fix-3--cached-transcript-projections`; report `PERFORMANCE-FIX-3.md`; 104 targeted tests and formatting passed. Presentation misses reuse projection; transcript revisions still rebuild (deliberate correctness fallback).
4. Turn-summary render metadata caching: resumed in fresh subagent `b5165f36-cf1e-43a2-8c76-ac5dd864406f` after provider interruptions; previous agent `1d779325-149d-483c-971f-c597f3df972f` stopped with implementation pending. Recovery report `PERFORMANCE-FIX-4.md`. Coordinator must merge with fix3 (agent works from its own baseline).

Integration branch: `fix/performance-bottlenecks`. Fix1 (25 tests) and fix2 (35 tests) revalidated in coordinator checkout after recovery. All work uncommitted as requested; retained worktrees provide independent source backups.

## Recovery protocol
Each subagent records its worktree path, changed files, tests, and a recovery patch in this log (or a companion report) before handoff. Coordinator integrates patches without removing source worktrees. Do not reset or discard work. Targeted tests only; never full workspace tests. Read FORGE-DESIGN.md for TUI changes. Preserve provider compatibility and record-before-side-effect durability.
