//! Debounced entrance for the pinned busy status line ("Waiting for the
//! model…" / "Thinking…" / "Running {tool}…") — see `ux-proposal`'s motion
//! timing plan (P1 Slice 4). Hiding must never be debounced; only entrance.

use super::prelude::*;
use std::time::{Duration, Instant};

fn start_busy_turn(app: &mut TuiApp, turn_started: Instant) {
    app.busy_state.start(BusyPhase::Model);
    app.timing.turn_started = Some(turn_started);
    app.timing.started = Some(turn_started);
}

#[tokio::test]
async fn busy_status_line_stays_hidden_before_the_debounce_threshold() {
    let (_dir, mut app) = focus_test_app().await;
    start_busy_turn(&mut app, Instant::now());

    let text = render_app_text(&mut app, 100, 30);
    assert!(
        !text.contains("Waiting for the model") && !text.contains("esc to interrupt"),
        "status line should not appear before the debounce threshold:\n{text}"
    );
}

#[tokio::test]
async fn busy_status_line_appears_once_the_debounce_threshold_passes() {
    let (_dir, mut app) = focus_test_app().await;
    start_busy_turn(
        &mut app,
        Instant::now()
            .checked_sub(Duration::from_millis(200))
            .expect("clock underflow"),
    );

    let text = render_app_text(&mut app, 100, 30);
    assert!(
        text.contains("Waiting for the model"),
        "status line should be visible past the debounce threshold:\n{text}"
    );
}

#[tokio::test]
async fn busy_status_line_hides_instantly_when_busy_ends_regardless_of_debounce_state() {
    let (_dir, mut app) = focus_test_app().await;
    start_busy_turn(
        &mut app,
        Instant::now()
            .checked_sub(Duration::from_millis(200))
            .expect("clock underflow"),
    );
    let visible = render_app_text(&mut app, 100, 30);
    assert!(visible.contains("Waiting for the model"), "{visible}");

    app.busy_state.stop();
    let text = render_app_text(&mut app, 100, 30);
    assert!(
        !text.contains("Waiting for the model") && !text.contains("esc to interrupt"),
        "hiding must be instant, not debounced:\n{text}"
    );
}

#[tokio::test]
async fn tool_call_transitions_do_not_re_trigger_the_debounce() {
    let (_dir, mut app) = focus_test_app().await;
    start_busy_turn(
        &mut app,
        Instant::now()
            .checked_sub(Duration::from_millis(200))
            .expect("clock underflow"),
    );
    let visible = render_app_text(&mut app, 100, 30);
    assert!(visible.contains("Waiting for the model"), "{visible}");

    // A step transition mid-turn (a tool call starting) does not touch
    // `turn_started` in real usage — only `set_phase`, never `start`.
    app.busy_state.set_phase(BusyPhase::Tool {
        name: "read_file".into(),
    });
    let text = render_app_text(&mut app, 100, 30);
    assert!(
        text.contains("Reading files"),
        "the line should stay visible and update in place, not re-hide:\n{text}"
    );
}
