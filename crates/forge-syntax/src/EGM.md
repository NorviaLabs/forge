# forge-syntax/src

## Overview

Implementation of tree-sitter based language detection, query selection, span
highlighting, and a bounded memoisation cache. Files:

- `lib.rs`: crate facade; declares private modules and re-exports the public
  surface. `HighlightTheme` is also exported as `Theme`.
- `lang.rs`: `SyntaxLanguage` enum, name/extension map, parser construction,
  and content heuristics.
- `queries.rs`: one lazily compiled tree-sitter highlight query per language.
- `highlight.rs`: walks the parse tree with the query, merges captures into
  `HighlightSpan`s, and converts them to per-line styled segments.
- `cache.rs`: process-global LRU cache keyed on the full `(lang, code, theme)`
  input.
- `kotlin.scm`, `dockerfile.scm`: highlight queries embedded with
  `include_str!`.

## Interface

- `SyntaxLanguage`: `Rust`, `TypeScript`, `Tsx`, `JavaScript`, `Python`, `Go`,
  `Json`, `Html`, `Css`, `Bash`, `Toml`, `Yaml`, `Ini`, `C`, `Cpp`, `Java`,
  `CSharp`, `Ruby`, `Php`, `Swift`, `Kotlin`, `Sql`, `Make`, `Dockerfile`,
  `Scss`, `Less`, `Unknown`; implements `Display`, `FromStr`, `Hash`, `Copy`.
- `SyntaxLanguage::tree_sitter(&self) -> tree_sitter::Language` and
  `as_str(self) -> &'static str`.
- `get_parser(lang: SyntaxLanguage) -> Parser`: fresh parser bound to the
  language's grammar.
- `detect_language(input: &str) -> Result<SyntaxLanguage, &'static str>`:
  shebang, JSON-shape, and keyword heuristics.
- `detect_from_path(path: &str) -> SyntaxLanguage`: filename/extension lookup
  (`Makefile`, `Dockerfile*`, dotted extensions); case-insensitive.
- `HighlightSpan { range: Range<usize>, style: HighlightStyle }` with
  `text<'a>(&self, source: &'a str) -> &'a str`.
- `HighlightStyle { class: HighlightClass }` with `rgb(&HighlightTheme) ->
  (u8, u8, u8)`, `is_bold()`, `is_italic()`.
- `HighlightClass`: `Default`, `Comment`, `Keyword`, `String`, `Number`,
  `Function`, `Type`, `Variable`, `Operator`, `Punctuation`, `Property`, `Tag`,
  `Attribute`.
- `HighlightTheme` (alias `Theme`): one `(u8, u8, u8)` field per class;
  `Default` is dark, `HighlightTheme::light()` is the light palette.
- `highlight(lang: &str, code: &str, theme: &HighlightTheme) -> Vec<HighlightSpan>`.
- `parse_and_capture(lang: &str, code: &str, _query: &str) -> Result<Tree, String>`:
  parses only; the query argument is unused and the language's built-in query
  is not applied.
- `highlight_to_lines(lang: &str, code: &str, theme: &HighlightTheme) -> HighlightedLines`:
  cached per-line segments.
- `HighlightedSegment = (String, (u8, u8, u8), bool, bool)` (text, RGB, bold,
  italic); `HighlightedLines = Arc<Vec<Vec<HighlightedSegment>>>`.
- `HighlightCacheStats { hits, misses, evictions, entries, bytes }`,
  `highlight_cache_stats()`, `clear_highlight_cache()`.

## Invariants

- Every returned span range is a valid byte range into the supplied `code`;
  adjacent spans of the same style are merged.
- An unparsable/unknown language returns exactly one `Default` span spanning
  `0..code.len()`; `highlight_to_lines` then emits that text as default-colored
  segments.
- Span-offset → line mapping is derived from `\n` positions in `code`; every
  input line yields at least one segment, so the output line count matches
  `code.lines()`.
- Each language's query is compiled once and cached in a `OnceLock`; an
  invalid embedded query panics rather than silently disabling highlighting.
- The cache key holds the full source text (not a hash) to rule out collisions
  that would render visibly wrong colors, and includes the theme, so a theme
  switch cannot serve stale colors.
- Cache entries are bounded by total segment bytes (4 MiB cap) and evicted
  least-recently-used first; stats and entry accounting stay consistent on
  eviction.
- The cache is global and mutex-protected because async highlighting can
  migrate across worker threads; the lock is never held during parsing.
- Returned `Arc<Vec<...>>` values are shared immutably; callers must not expect
  ownership.
- `detect_language` caps JSON-shape probing (`MAX_JSON_DETECT_BYTES`) so a huge
  `{`/`[`-delimited block is not parsed on every render.
- `get_parser` expects a valid grammar binding and panics otherwise; callers
  must pass a real language (not `Unknown`).

## Side Effects

- Mutates the process-global highlight cache (memory, counters) on every
  `highlight_to_lines` call.
- `clear_highlight_cache` drops all cached segments and resets counters;
  intended for tests.
- Tests that touch the global cache must hold the crate-private
  `cache::lock_cache()` guard.
- No disk, network, or process side effects.

## Verification

- Run: `cargo test --package forge-syntax --locked`
