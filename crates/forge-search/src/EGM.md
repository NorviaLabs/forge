# forge-search source

## Overview

Workspace file/content search for agents and UI, backed by `fff-search`. Wraps
a long-lived, incrementally updated file picker with frecency, then adds
Quick Open re-ranking and a merged name+content result list for
search-as-you-type.

## Interface

`index.rs`:

- `DEFAULT_SEARCH_MAX_FILES: usize = 200`, `MIN_CONTENT_QUERY_CHARS: usize = 2`.
- `SearchError::{Init, ScanTimeout, Lock}`.
- `WorkspaceIndexOptions { watch: true, scan_timeout: 10s, wait_for_scan: true }`.
- `WorkspaceIndex::{open, open_with_options, workspace_root, wait_for_scan, find_files, find_files_quick_open, grep, grep_scoped, search_files, note_file_changed, note_file_opened}`.
- `merge_search_results(name: FindResponse, content: GrepResponse, max_files) -> MergedSearch`.

`quick_open.rs`:

- `score_quick_open(path, query) -> Option<QuickOpenScore>` (public; `QuickOpenScore { score, match_ranges }` is not re-exported, so callers cannot name it).
- Crate-internal: `rerank_quick_open_hits(hits, query) -> Vec<FileSearchHit>`.

`types.rs`: `FileSearchHit { path, score, relevance, match_ranges }`, `GrepSearchHit { path, line, column, text, context, relevance, is_definition }`, `FindResponse { hits, total_matched, total_files }`, `GrepResponse { hits, total_matched }`, `MergedSearch { paths, truncated }`, `GrepQueryMode::{Plain, Literal, Regex, Fuzzy}`.

## Invariants

- `max_results == 0` returns an empty response without touching the index; an empty/whitespace grep pattern also returns empty.
- Every query path calls `wait_for_scan` first, so a slow initial scan surfaces as `SearchError::ScanTimeout` (default 10s) rather than partial results.
- `relevance` is normalized against the **top hit of the same result set** (`score / top_score`, clamped to `0.0..=1.0`), not against an absolute scale. `GrepSearchHit.relevance` is `Option<f32>`: normalized against the max `fuzzy_score`, and `None` for hits without one (every non-`Fuzzy` mode).
- `find_files_quick_open` pulls a wider candidate set (`max_results * 8`, clamped to `100..=400`) from fff, re-ranks with Quick Open scoring, then truncates; an empty query delegates to plain `find_files` so frecency ordering survives. Its `total_matched` counts re-ranked hits within that candidate window, not fff's total match count as in `find_files`.
- Content matching waits for `MIN_CONTENT_QUERY_CHARS` (2) characters; one character is a whole-workspace scan whose only possible output is noise.
- Grep hit text is capped at `MAX_GREP_LINE_CHARS` (240) and context at `MAX_GREP_CONTEXT_CHARS` (320) in the returned hit. In `Literal` mode, a hit without match offsets re-reads its source line from disk unbounded before truncation, so a very long (e.g. minified) line is read whole into memory.
- `path` and `include` filters are applied as FFF query constraints **before** pagination. Post-filtering the first page would drop every hit when the first `max_results` matches live outside the requested directory.
- `merge_search_results` orders name-tier paths before content-only paths, dedupes, and caps at `max_files`. `truncated` is true when the cap was reached or the content tier returned a full page (`CONTENT_PAGE_MULTIPLIER`, 8), because either means the listing is a prefix of the result set.
- `score_quick_open` returns `None` for an empty query. Multi-piece (whitespace) and path-aware (contains `/`) queries take the word-boundary scoring path; basename and path-segment bonuses outrank raw fuzziness.
- `rerank_quick_open_hits` drops hits that do not score at all, so its output can be shorter than its input.

## Side Effects

- Scans and watches the workspace filesystem; spawns scanner threads inside `fff-search`.
- Reads file contents for grep and for building hit context.
- `note_file_opened` updates frecency state; `note_file_changed` updates the index for a path.
- Holds a read lock on the shared picker for the duration of a query.
- Resolves paths relative to the workspace root passed to `open`.

## Verification

- Run: `cargo test --package forge-search --locked`
