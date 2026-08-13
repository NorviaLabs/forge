## Verification

When asked to test TUI behavior interactively, do the PTY/tmux driving yourself — do NOT delegate interactive testing to a background subagent. Subagents may be used for parallel capture/build work only.

### Answering questions about behavior

If asked 'is X bound / does Y exist / what does Z do', read the source first. Only launch a PTY/tmux run when the question is about observable runtime rendering that code reading cannot settle.

## Build & Test

## Editing rules

Never use sed/regex/line-range deletions for multi-line Rust code. Use the Edit tool with exact anchored strings, one edit per site. Blanket renames must be scoped to the target crate and reviewed file-by-file.

## Definition of done

Every code change ships with: (1) a new or updated test covering the change, (2) `cargo fmt --all`, (3) `cargo clippy --all-targets`, (4) `cargo test --workspace` green — before opening a PR.

## Git & GitHub workflow

### Issue and PR references

'Fix #N' means: first run `gh pr view N` and `gh issue view N` to determine whether N is an issue or an existing PR. If a PR already exists for the work, fix that branch — never reimplement on main.

## UI / TUI

### TUI layout conventions

Columnar TUI content uses ratatui `Table`, never hand-padded `List` strings. Hints and secondary context are right-aligned per forge convention. Do not add navigation hints unless explicitly requested.
