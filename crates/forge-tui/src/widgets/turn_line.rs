//! The live turn line: one animated row pinned to the bottom of the
//! transcript while a turn is in flight.
//!
//! Before this, the only indicator that anything was happening lived in the
//! footer — the far corner of the screen, ninety columns from the text the
//! reader is actually watching — and it said "Working" for every phase of
//! every turn. A capture of a real turn had eleven consecutive identical
//! frames: eight seconds in which a stalled renderer and a hung provider
//! looked exactly alike.
//!
//! This line sits where the answer is about to appear, names the phase, counts
//! up from zero, reports what has arrived, and says how to stop it.

use crate::theme;
use crate::widgets::status::BusyPhase;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

/// Spinner frames. Quarter-circles, matching the lifecycle glyphs the status
/// bar already uses, so the vocabulary stays forge's own.
const GLYPHS: [&str; 4] = ["◐", "◓", "◑", "◒"];

/// How long each spinner frame holds. Fast enough to read as motion, slow
/// enough not to strobe on a 100ms event loop.
const GLYPH_MILLIS: u128 = 140;

/// What the reader is told to press to stop the turn.
pub const INTERRUPT_HINT: &str = "esc to interrupt";

/// Everything the line needs, resolved by the caller from live app state.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnLineModel {
    /// Phase verb, already in present participle form ("Thinking").
    pub verb: String,
    pub elapsed_secs: f64,
    /// Characters of thinking + answer received so far.
    pub chars: usize,
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
        BusyPhase::Tool { name } => format!("Running {name}"),
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

/// Which spinner frame `millis` since the epoch lands on.
pub fn glyph_at(millis: u128) -> &'static str {
    GLYPHS[((millis / GLYPH_MILLIS) as usize) % GLYPHS.len()]
}

/// Round to a compact count: `842`, `1.2k`, `48k`.
fn compact(n: usize) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=9_999 => format!("{:.1}k", n as f64 / 1_000.0),
        _ => format!("{}k", n / 1_000),
    }
}

/// The metrics half of the line: elapsed, volume, rate.
///
/// Characters rather than tokens, deliberately: no provider reports token
/// usage while the stream is still open, so a token count here would be an
/// estimate presented as a measurement. The footer still shows real
/// API-reported token usage once the turn ends.
fn metrics(model: &TurnLineModel) -> String {
    let elapsed = forge_transcript::format_elapsed_tenths(model.elapsed_secs);
    if model.chars == 0 {
        return elapsed;
    }
    let mut out = format!("{elapsed} · ↓ {} chars", compact(model.chars));
    if model.elapsed_secs >= 1.0 {
        let rate = model.chars as f64 / model.elapsed_secs;
        let per_sec = compact(rate.round() as usize);
        let unit = if per_sec == "1" { "char/s" } else { "chars/s" };
        out.push_str(&format!(" · {per_sec} {unit}"));
    }
    out
}

/// Build the line, right-aligning the interrupt hint to `width`.
pub fn turn_line(model: &TurnLineModel, width: usize, millis: u128) -> Line<'static> {
    let mut spans = vec![
        Span::raw("  "),
        Span::styled(glyph_at(millis), theme::accent_style()),
        Span::raw("  "),
        Span::styled(
            format!("{}…", model.verb),
            theme::text().add_modifier(Modifier::BOLD),
        ),
        Span::raw("   "),
        Span::styled(metrics(model), theme::metadata_style()),
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
            chars: 512,
            interruptible: true,
        }
    }

    fn text(line: &Line<'static>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn the_line_names_the_phase_and_counts_up() {
        let rendered = text(&turn_line(&model(), 80, 0));
        assert!(rendered.contains("Thinking…"), "{rendered}");
        assert!(rendered.contains("3.0s"), "{rendered}");
        assert!(rendered.contains("512 chars"), "{rendered}");
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

    /// Motion must not depend on the metrics changing: a provider that sends
    /// nothing for a while still has to look alive.
    #[test]
    fn the_glyph_alone_keeps_the_line_moving() {
        let stalled = TurnLineModel {
            elapsed_secs: 4.0,
            chars: 0,
            ..model()
        };
        let a = text(&turn_line(&stalled, 80, 0));
        let b = text(&turn_line(&stalled, 80, GLYPH_MILLIS));
        assert_ne!(a, b, "the spinner did not advance");
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
            "Running bash"
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

    /// "1 chars/s" is sloppy where the reader is watching one line very
    /// closely.
    #[test]
    fn a_rate_of_one_reads_as_singular() {
        let slow = TurnLineModel {
            elapsed_secs: 30.0,
            chars: 30,
            ..model()
        };
        let rendered = text(&turn_line(&slow, 80, 0));
        assert!(rendered.contains("1 char/s"), "{rendered}");
        assert!(!rendered.contains("1 chars/s"), "{rendered}");
    }

    #[test]
    fn counts_stay_compact() {
        assert_eq!(compact(842), "842");
        assert_eq!(compact(1_240), "1.2k");
        assert_eq!(compact(48_000), "48k");
    }
}
