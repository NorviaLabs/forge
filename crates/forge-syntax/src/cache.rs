//! Bounded cache for highlight results.
//!
//! Highlighting is a pure function of (language, source text, theme), but the TUI
//! recomputes it constantly. The conversation render cache is invalidated by a
//! terminal resize, a theme switch, a scrollback change and every busy-phase
//! transition during a turn; each invalidation rebuilds the transcript and
//! re-runs tree-sitter over *every* code block in it. Caching on those three
//! inputs turns the repeat work into a lookup.
//!
//! Design notes:
//!
//! - The key stores the full source text rather than a hash of it. A hash would
//!   be smaller but admits collisions, and a collision here renders visibly wrong
//!   colours. Segment output is larger than its input, so exactness is cheap.
//! - Global and lock-protected rather than thread-local: highlighting is reached
//!   from async code whose task can migrate between worker threads, which would
//!   silently split a thread-local cache.
//! - The lock is never held across a parse. Compute happens unlocked, so
//!   concurrent highlighting of different blocks does not serialise.
//! - Bounded by total bytes with least-recently-used eviction, so a long session
//!   cannot trade a CPU leak for a memory leak.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use crate::highlight::HighlightTheme;

/// Total cached segment bytes allowed before eviction begins. Large enough to
/// hold the code blocks of a long session, small enough to stay unremarkable.
const MAX_BYTES: usize = 4 * 1024 * 1024;

/// Per-segment bookkeeping charged on top of the text itself: the `String`
/// header, the colour triple and the two style flags.
const SEGMENT_OVERHEAD: usize = 32;

type Segment = (String, (u8, u8, u8), bool, bool);
type Lines = Vec<Vec<Segment>>;

#[derive(PartialEq, Eq, Hash)]
struct Key {
    lang: String,
    code: String,
    theme: HighlightTheme,
}

struct Entry {
    /// Shared with every caller that looks this entry up, so neither a hit nor an
    /// insert copies the segments.
    lines: Arc<Lines>,
    bytes: usize,
    used: u64,
}

#[derive(Default)]
struct Cache {
    map: HashMap<Key, Entry>,
    bytes: usize,
    hits: u64,
    misses: u64,
    evictions: u64,
}

/// Observable cache counters. Exposed so invalidation behaviour can be asserted
/// in tests rather than inferred from timings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HighlightCacheStats {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub entries: usize,
    pub bytes: usize,
}

static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
static TICK: AtomicU64 = AtomicU64::new(0);

/// A poisoned lock must not cascade: one panicking highlight would otherwise
/// break every later render. Recover the guard and carry on.
fn cache() -> MutexGuard<'static, Cache> {
    CACHE
        .get_or_init(|| Mutex::new(Cache::default()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn measure(lines: &Lines) -> usize {
    lines
        .iter()
        .map(|line| {
            line.iter()
                .map(|(text, ..)| text.len() + SEGMENT_OVERHEAD)
                .sum::<usize>()
        })
        .sum()
}

/// Return the highlight for `(lang, code, theme)`, computing it via `compute`
/// only on a miss.
///
/// The result is shared rather than copied. A hit costs a refcount bump; storing
/// a fresh entry costs one too. Previously both paths deep-copied every segment,
/// which meant the cache still allocated a `String` per token on every lookup —
/// cheaper than re-parsing, but far from free.
///
/// `compute` runs with the lock released, so a slow parse does not block lookups
/// for other blocks.
pub(crate) fn cached_or_compute<F>(
    lang: &str,
    code: &str,
    theme: &HighlightTheme,
    compute: F,
) -> Arc<Lines>
where
    F: FnOnce() -> Lines,
{
    let key = Key {
        lang: lang.to_string(),
        code: code.to_string(),
        theme: *theme,
    };

    {
        let mut guard = cache();
        if let Some(entry) = guard.map.get_mut(&key) {
            entry.used = TICK.fetch_add(1, Ordering::Relaxed);
            let lines = Arc::clone(&entry.lines);
            guard.hits += 1;
            return lines;
        }
        guard.misses += 1;
    }

    let lines = Arc::new(compute());
    let bytes = measure(&lines);

    // An entry larger than the whole budget can never be retained; hand it back
    // without disturbing the cache.
    if bytes > MAX_BYTES {
        return lines;
    }

    let mut guard = cache();
    // A concurrent caller may have inserted the same key while we computed.
    if !guard.map.contains_key(&key) {
        let used = TICK.fetch_add(1, Ordering::Relaxed);
        guard.bytes += bytes;
        guard.map.insert(
            key,
            Entry {
                lines: Arc::clone(&lines),
                bytes,
                used,
            },
        );
        evict_to_budget(&mut guard);
    }
    lines
}

fn evict_to_budget(guard: &mut MutexGuard<'static, Cache>) {
    while guard.bytes > MAX_BYTES {
        let victim = guard
            .map
            .iter()
            .min_by_key(|(_, entry)| entry.used)
            .map(|(key, _)| Key {
                lang: key.lang.clone(),
                code: key.code.clone(),
                theme: key.theme,
            });
        let Some(victim) = victim else { break };
        if let Some(entry) = guard.map.remove(&victim) {
            guard.bytes = guard.bytes.saturating_sub(entry.bytes);
            guard.evictions += 1;
        } else {
            break;
        }
    }
}

/// Current cache counters.
pub fn highlight_cache_stats() -> HighlightCacheStats {
    let guard = cache();
    HighlightCacheStats {
        hits: guard.hits,
        misses: guard.misses,
        evictions: guard.evictions,
        entries: guard.map.len(),
        bytes: guard.bytes,
    }
}

/// Drop every cached highlight and reset the counters. Intended for tests that
/// need a known starting point.
pub fn clear_highlight_cache() {
    let mut guard = cache();
    guard.map.clear();
    guard.bytes = 0;
    guard.hits = 0;
    guard.misses = 0;
    guard.evictions = 0;
}

/// Serialises every test that touches the process-global cache.
///
/// The cache tests clear it and then assert on absolute counters, so any test
/// that reaches [`cached_or_compute`] must hold this guard — including the ones
/// that reach it indirectly through [`crate::highlight::highlight_to_lines`],
/// which are in a different module but the same test binary. This mirrors the
/// repo's established pattern for process-global state (`lock_env` in
/// `editor.rs`, `ScopedEnvGuard` in `app.rs`). Poisoning is recovered rather
/// than propagated so a single failing test does not cascade into the rest.
#[cfg(test)]
pub(crate) fn lock_cache() -> MutexGuard<'static, ()> {
    static GUARD: Mutex<()> = Mutex::new(());
    GUARD
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines_of(text: &str) -> Lines {
        vec![vec![(text.to_string(), (1, 2, 3), false, false)]]
    }

    #[test]
    fn second_lookup_hits_without_recomputing() {
        let _guard = lock_cache();
        clear_highlight_cache();
        let theme = HighlightTheme::default();

        let first = cached_or_compute("rust", "fn a() {}", &theme, || lines_of("computed"));
        let second = cached_or_compute("rust", "fn a() {}", &theme, || {
            panic!("must not recompute on a hit")
        });

        assert_eq!(first, second);
        let stats = highlight_cache_stats();
        assert_eq!((stats.hits, stats.misses), (1, 1));
    }

    /// A hit must hand back the *same* allocation, not a copy of it. Pointer
    /// identity is the direct check: before results were shared, every lookup
    /// rebuilt a `String` per token, so the cache saved the tree-sitter parse but
    /// still allocated proportionally to the block.
    #[test]
    fn lookups_share_one_allocation() {
        let _guard = lock_cache();
        clear_highlight_cache();
        let theme = HighlightTheme::default();

        let first = cached_or_compute("rust", "fn a() {}", &theme, || lines_of("computed"));
        let second = cached_or_compute("rust", "fn a() {}", &theme, || {
            panic!("must not recompute on a hit")
        });

        assert!(
            Arc::ptr_eq(&first, &second),
            "a hit must share the cached allocation, not clone it"
        );
        // The stored entry is the same allocation too, so inserting did not copy:
        // two live handles plus the cache's own.
        assert_eq!(Arc::strong_count(&first), 3);
    }

    #[test]
    fn different_theme_is_a_separate_entry() {
        let _guard = lock_cache();
        clear_highlight_cache();
        let dark = HighlightTheme::default();
        let light = HighlightTheme {
            keyword: (0, 0, 0),
            ..HighlightTheme::default()
        };

        cached_or_compute("rust", "fn a() {}", &dark, || lines_of("dark"));
        let got = cached_or_compute("rust", "fn a() {}", &light, || lines_of("light"));

        assert_eq!(*got, lines_of("light"), "theme must not alias");
        assert_eq!(highlight_cache_stats().entries, 2);
    }

    #[test]
    fn different_language_is_a_separate_entry() {
        let _guard = lock_cache();
        clear_highlight_cache();
        let theme = HighlightTheme::default();

        cached_or_compute("rust", "x", &theme, || lines_of("as-rust"));
        let got = cached_or_compute("python", "x", &theme, || lines_of("as-python"));

        assert_eq!(*got, lines_of("as-python"));
        assert_eq!(highlight_cache_stats().entries, 2);
    }

    #[test]
    fn eviction_keeps_the_cache_within_budget() {
        let _guard = lock_cache();
        clear_highlight_cache();
        let theme = HighlightTheme::default();
        // Each entry carries ~64KiB of segment text, so the 4MiB budget is
        // exceeded well before the last insert.
        let chunk = "x".repeat(64 * 1024);

        for i in 0..96 {
            let code = format!("{i}");
            cached_or_compute("rust", &code, &theme, || lines_of(&chunk));
        }

        let stats = highlight_cache_stats();
        assert!(
            stats.bytes <= MAX_BYTES,
            "cache exceeded its budget: {} bytes",
            stats.bytes
        );
        assert!(stats.evictions > 0, "expected evictions under pressure");
    }

    #[test]
    fn oversized_entry_is_returned_but_not_retained() {
        let _guard = lock_cache();
        clear_highlight_cache();
        let theme = HighlightTheme::default();
        let huge = "x".repeat(MAX_BYTES + 1);

        let got = cached_or_compute("rust", "big", &theme, || lines_of(&huge));

        assert_eq!(*got, lines_of(&huge), "caller still gets its result");
        assert_eq!(
            highlight_cache_stats().entries,
            0,
            "an entry larger than the budget must not be stored"
        );
    }
}
