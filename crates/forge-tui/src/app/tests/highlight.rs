//! Syntax-highlight cache invalidation integration tests.
//!
//! Split out of `app/tests/mod.rs` per #19. Moved verbatim.

use super::prelude::*;

/// Highlighting does not depend on terminal width, so a resize must reuse it.
/// A resize flips the conversation render key and rebuilds visible lines;
/// before this cache that re-ran tree-sitter over every code block.
#[tokio::test]
async fn resize_reuses_cached_highlights() {
    let (_dir, mut app) = app_with_code("resize").await;
    // Take the serialising guard only after the last await: holding a std
    // guard across an await point is a clippy error and a real deadlock risk.
    let _guard = lock_highlight_cache();
    draw_app(&mut app, 100, 30);
    let before = forge_syntax::highlight_cache_stats();

    // A wide delta guarantees the chat width changes, so the render key flips
    // and the transcript is rebuilt from scratch.
    draw_app(&mut app, 170, 40);
    let after = forge_syntax::highlight_cache_stats();

    assert!(
        after.hits >= before.hits + CACHED_BLOCKS as u64,
        "a resize must serve every block from cache (hits {} -> {})",
        before.hits,
        after.hits
    );
}

/// Scrolling changes which lines are visible, never their colours. The scroll
/// offset is not part of the conversation render key, so a scroll does not
/// rebuild the transcript and must not recompute any highlight.
///
/// Asserted as an upper bound with tolerance: a genuine re-highlight would add
/// exactly `CACHED_BLOCKS` misses, whereas a concurrent `source_viewer` test
/// contributes at most one or two.
#[tokio::test]
async fn scrollback_does_not_recompute_highlights() {
    let (_dir, mut app) = app_with_code("scroll").await;
    let _guard = lock_highlight_cache();
    draw_app(&mut app, 100, 30);
    let before = forge_syntax::highlight_cache_stats();

    app.conversation_view.follow = false;
    app.conversation_view.scroll = 3;
    draw_app(&mut app, 100, 30);
    let after = forge_syntax::highlight_cache_stats();

    assert!(
        after.misses < before.misses + CACHED_BLOCKS as u64,
        "scrolling must not recompute the transcript's highlights \
         (misses {} -> {})",
        before.misses,
        after.misses
    );
}

/// Reopening a session re-renders the same transcript text in a fresh
/// `TuiApp`. The cache is keyed on content, not on app identity, so the
/// second app must not pay for highlighting again.
#[tokio::test]
async fn session_reload_reuses_cached_highlights() {
    // Both apps are built before the guard is taken, for the same reason.
    let (_dir, mut first) = app_with_code("reload").await;
    // A separate app and session carrying identical transcript text.
    let (_dir2, mut reloaded) = app_with_code("reload").await;
    let _guard = lock_highlight_cache();
    draw_app(&mut first, 100, 30);
    let before = forge_syntax::highlight_cache_stats();

    draw_app(&mut reloaded, 100, 30);
    let after = forge_syntax::highlight_cache_stats();

    assert!(
        after.hits >= before.hits + CACHED_BLOCKS as u64,
        "a reloaded session must reuse cached highlights (hits {} -> {})",
        before.hits,
        after.hits
    );
}
