# Performance fix 4 recovery

Worktree: coordinator integration worktree (subagent repeatedly stalled before editing; coordinator completed the narrowly scoped implementation directly).

Changed files: `crates/forge-tui/src/app/types.rs`, `new.rs`, `input.rs`, `turn.rs`, `chrome.rs`, `commands.rs`, `render.rs`.

Implementation: cache filtered turn summaries and their render digest keyed by session UUID and an explicit revision counter. Increment revision on insert/replace, supervised completion, resume/clear removal, and preserve it through session view save/restore. Same-count replacements invalidate correctly; steady frames reuse the cached metadata.

Validation: `cargo fmt --all` passed. TUI tests were previously passing for fixes 1–3, but final rerun was blocked because Cargo could not download/read missing `anyhow`/`chrono` dependencies in the current sandbox cache. Do not discard source changes; retained fix1–3 worktrees and reports remain available.
