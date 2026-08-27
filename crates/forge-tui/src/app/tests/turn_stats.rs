//! Per-turn timing archive tests — `record_turn_summary`'s ordinal-keyed
//! archive, and the specific desync scenario (a cancelled turn) it exists
//! to avoid. See `ux-proposal`'s per-turn timing persistence plan.

use super::prelude::*;
use std::time::Instant;

fn push_turn(app: &mut TuiApp, request: &str, reply: &str) {
    app.session
        .messages
        .push(Message::new(MessageRole::User, request));
    app.session
        .messages
        .push(Message::new(MessageRole::Assistant, reply));
    app.transcript_view.refresh(&app.session);
}

fn complete_turn(app: &mut TuiApp, chars: usize, tools: usize) {
    app.timing.turn_started = Some(Instant::now());
    app.timing.chars = chars;
    app.timing.tools = tools;
    app.timing.completion_tokens_at_start = 0;
    app.record_turn_summary();
}

#[tokio::test]
async fn record_turn_summary_archives_by_turn_ordinal() {
    let (_dir, mut app) = focus_test_app().await;

    push_turn(&mut app, "first", "first reply");
    complete_turn(&mut app, 10, 1);
    assert_eq!(app.turn_stats.len(), 1);
    assert!(app.turn_stats.contains_key(&0));

    push_turn(&mut app, "second", "second reply");
    complete_turn(&mut app, 20, 2);
    assert_eq!(app.turn_stats.len(), 2);
    assert!(app.turn_stats.contains_key(&0));
    assert!(app.turn_stats.contains_key(&1));
    assert_eq!(app.turn_stats[&1].chars, 20);
    assert_eq!(app.turn_stats[&1].tools, 2);
}

/// The direct regression test for the desync bug this design avoids: a
/// cancelled turn never calls `record_turn_summary` (mirroring real
/// cancellation, `app/turn.rs`'s `was_cancel` branch), yet the turn AFTER
/// it must still archive under its own correct ordinal, not the cancelled
/// turn's — a naive "next list position" scheme would get this wrong.
#[tokio::test]
async fn a_cancelled_turn_does_not_desync_later_ordinals() {
    let (_dir, mut app) = focus_test_app().await;

    push_turn(&mut app, "first", "first reply");
    complete_turn(&mut app, 10, 0);

    // Turn 2: cancelled — its User message lands in the transcript, but
    // record_turn_summary is deliberately never called for it.
    app.session
        .messages
        .push(Message::new(MessageRole::User, "second (cancelled)"));
    app.transcript_view.refresh(&app.session);
    app.timing.turn_started = None;

    push_turn(&mut app, "third", "third reply");
    complete_turn(&mut app, 30, 3);

    assert!(app.turn_stats.contains_key(&0), "turn 1 archived");
    assert!(
        !app.turn_stats.contains_key(&1),
        "turn 2 was cancelled — no entry"
    );
    assert!(
        app.turn_stats.contains_key(&2),
        "turn 3 archived at its own correct ordinal (2), not misattached to turn 2's slot"
    );
    assert_eq!(app.turn_stats[&2].chars, 30);
}
