//! The live turn line: one stable row pinned above the composer while a turn
//! is in flight (2026 design system, DESIGN-006).
//!
//! Before this, the only indicator that anything was happening lived in the
//! footer — the far corner of the screen, ninety columns from the text the
//! reader is actually watching — and it said "Working" for every phase of
//! every turn. A capture of a real turn had eleven consecutive identical
//! frames: eight seconds in which a stalled renderer and a hung provider
//! looked exactly alike.
//!
//! The line sits where the answer is about to appear: `[>]` in the activity
//! token, the evidence-backed phase in bold primary, elapsed time secondary.
//! Motion comes from the elapsed tick alone — no per-letter shimmer, no
//! character counter, no duplicate busy state in the footer.

use crate::theme;
use crate::widgets::status::BusyPhase;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

/// What the reader is told to press to stop the turn.
pub const INTERRUPT_HINT: &str = "esc to interrupt";

/// Everything the line needs, resolved by the caller from live app state.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnLineModel {
    /// Phase verb, already in present participle form ("Thinking").
    pub verb: String,
    pub elapsed_secs: f64,
    /// Whether Esc will actually interrupt right now.
    pub interruptible: bool,
}

/// Name the phase the turn is in.
///
/// `BusyPhase` was already tracked and then flattened to the single word
/// "Working" for display, which threw away the one thing the reader wanted to
/// know: whether it is talking to the provider, running a command, or writing.
pub fn phase_verb(phase: &BusyPhase, thinking: bool, answering: bool) -> String {
    match phase {
        BusyPhase::Connect => "Connecting".into(),
        BusyPhase::Tool { name } => crate::widgets::status::tool_progress_description(name),
        BusyPhase::Other(label) if !label.trim().is_empty() => {
            let label = label.trim();
            let mut chars = label.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => "Working".into(),
            }
        }
        _ if answering => "Writing the answer".into(),
        _ if thinking => "Thinking".into(),
        _ => "Waiting for the model".into(),
    }
}

/// Elapsed time, secondary. No character counter and no rate: no provider
/// reports token usage while the stream is still open, and characters per
/// second measures verbosity rather than speed. Volume and rate belong to
/// the finished turn summary, where the provider's usage makes them real.
fn elapsed(model: &TurnLineModel) -> String {
    forge_transcript::format_elapsed_tenths(model.elapsed_secs)
}

/// Build the line, right-aligning the interrupt hint to `width`.
///
/// Pure in the model: the same phase and elapsed second always render the
/// same line, so settled frames stay cacheable and only the elapsed tick
/// invalidates chrome. `_millis` is retained for call-site compatibility.
pub fn turn_line(model: &TurnLineModel, width: usize, _millis: u128) -> Line<'static> {
    let mut spans = vec![
        Span::styled("[>]", theme::activity().add_modifier(Modifier::BOLD)),
        Span::raw(" "),
        Span::styled(
            model.verb.clone(),
            theme::text().add_modifier(Modifier::BOLD),
        ),
        Span::raw(" · "),
        Span::styled(elapsed(model), theme::metadata_style()),
    ];
    if model.interruptible {
        let used: usize = spans.iter().map(Span::width).sum();
        let hint_w = INTERRUPT_HINT.chars().count();
        // Drop the hint rather than wrap the line when the pane is too narrow
        // to hold both halves — a wrapped status line reads as content.
        if used + hint_w + 2 <= width {
            spans.push(Span::raw(" ".repeat(width - used - hint_w)));
            spans.push(Span::styled(INTERRUPT_HINT, theme::dim()));
        }
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> TurnLineModel {
        TurnLineModel {
            verb: "Thinking".into(),
            elapsed_secs: 3.0,
            interruptible: true,
        }
    }

    fn text(line: &Line<'static>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn the_line_names_the_phase_and_counts_up() {
        let rendered = text(&turn_line(&model(), 80, 0));
        assert!(rendered.contains("[>]"), "{rendered}");
        assert!(rendered.contains("Thinking"), "{rendered}");
        assert!(rendered.contains("3.0s"), "{rendered}");
    }

    #[test]
    fn the_marker_uses_the_activity_token() {
        let line = turn_line(&model(), 80, 0);
        assert_eq!(
            line.spans[0].style,
            theme::activity().add_modifier(Modifier::BOLD)
        );
    }

    /// Esc has been bound for a long time and nothing on screen ever said so.
    #[test]
    fn the_interrupt_is_advertised_while_a_turn_runs() {
        let rendered = text(&turn_line(&model(), 80, 0));
        assert!(rendered.trim_end().ends_with(INTERRUPT_HINT), "{rendered}");
        let mut quiet = model();
        quiet.interruptible = false;
        assert!(!text(&turn_line(&quiet, 80, 0)).contains(INTERRUPT_HINT));
    }

    /// A wrapped status line reads as content, so the hint drops instead.
    #[test]
    fn a_narrow_pane_drops_the_hint_rather_than_wrapping() {
        let narrow = turn_line(&model(), 30, 0);
        let wide = turn_line(&model(), 200, 0);
        assert!(!text(&narrow).contains(INTERRUPT_HINT));
        assert!(text(&wide).contains(INTERRUPT_HINT));
        assert!(
            narrow.width() < wide.width(),
            "the narrow line was padded anyway: {}",
            narrow.width()
        );
        assert!(
            !text(&narrow).trim_end().ends_with(' '),
            "trailing padding on a line with no hint"
        );
    }

    /// The invariant the whole change exists for: while a turn is in flight,
    /// no two frames two seconds apart may be identical. A capture of the old
    /// TUI had eleven identical frames in a row.
    #[test]
    fn no_two_frames_two_seconds_apart_are_identical() {
        for step in 0..40 {
            let at = step as f64 * 0.25;
            let mut early = model();
            early.elapsed_secs = at;
            let mut late = model();
            late.elapsed_secs = at + 2.0;
            let early_ms = (at * 1000.0) as u128;
            assert_ne!(
                text(&turn_line(&early, 80, early_ms)),
                text(&turn_line(&late, 80, early_ms + 2_000)),
                "frames 2s apart matched at {at}s"
            );
        }
    }

    /// Motion comes from the elapsed tick: the line is pure in the model, so
    /// a stalled provider still advances once a second, and identical inputs
    /// render identical lines (settled frames stay cacheable).
    #[test]
    fn the_elapsed_tick_alone_keeps_the_line_moving() {
        let mut stalled = model();
        stalled.elapsed_secs = 4.0;
        let a = text(&turn_line(&stalled, 80, 0));
        stalled.elapsed_secs = 5.0;
        let b = text(&turn_line(&stalled, 80, 0));
        assert_ne!(a, b, "the elapsed tick must move the line");
        // Same inputs, same line — no hidden clock inside the renderer.
        assert_eq!(
            text(&turn_line(&model(), 80, 0)),
            text(&turn_line(&model(), 80, 999))
        );
    }

    #[test]
    fn the_verb_comes_from_the_phase() {
        assert_eq!(phase_verb(&BusyPhase::Connect, false, false), "Connecting");
        assert_eq!(
            phase_verb(
                &BusyPhase::Tool {
                    name: "bash".into()
                },
                false,
                false
            ),
            "Running command"
        );
        assert_eq!(phase_verb(&BusyPhase::Model, true, false), "Thinking");
        assert_eq!(
            phase_verb(&BusyPhase::Model, true, true),
            "Writing the answer"
        );
        assert_eq!(
            phase_verb(&BusyPhase::Idle, false, false),
            "Waiting for the model"
        );
    }

    /// No character counter and no rate on the live line: volume and rate
    /// belong to the finished turn summary, where the provider's usage
    /// makes them real measurements.
    #[test]
    fn the_live_line_reports_no_rate_or_volume() {
        let mut slow = model();
        slow.elapsed_secs = 30.0;
        let rendered = text(&turn_line(&slow, 80, 0));
        assert!(!rendered.contains("/s"), "{rendered}");
        assert!(!rendered.contains("chars"), "{rendered}");
        assert!(!rendered.contains('↓'), "{rendered}");
        assert!(rendered.contains("30s"), "{rendered}");
    }
}
