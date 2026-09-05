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

fn shimmer_phase(state: &throbber_widgets_tui::ThrobberState) -> usize {
    throbber_widgets_tui::Throbber::default()
        .label("")
        .throbber_set(throbber_widgets_tui::HORIZONTAL_BLOCK)
        .to_line(state)
        .spans
        .into_iter()
        .next()
        .and_then(|span| {
            span.content.chars().find_map(|character| {
                "▏▎▍▌▋▊▉█"
                    .chars()
                    .position(|candidate| candidate == character)
            })
        })
        .unwrap_or_default()
}

/// Apply a traveling brightness wave to the letters of a label. The throbber
/// state remains the shared clock, but the animation is intentionally carried
/// by the words rather than a separate block glyph.
pub(crate) fn shimmer_text(
    state: &throbber_widgets_tui::ThrobberState,
    label: &str,
) -> Vec<Span<'static>> {
    let phase = shimmer_phase(state);
    label
        .chars()
        .enumerate()
        .map(|(index, character)| {
            let distance = (index + 8 - phase) % 8;
            let style = if character.is_whitespace() {
                theme::text_secondary()
            } else if distance == 0 {
                theme::accent_style().add_modifier(Modifier::BOLD)
            } else if distance == 1 || distance == 7 {
                theme::text_secondary()
            } else {
                theme::muted()
            };
            Span::styled(character.to_string(), style)
        })
        .collect()
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

/// Round to a compact count: `842`, `1.2k`, `48k`.
fn compact(n: usize) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=9_999 => format!("{:.1}k", n as f64 / 1_000.0),
        _ => format!("{}k", n / 1_000),
    }
}

/// The metrics half of the line: elapsed and volume.
///
/// Characters rather than tokens, deliberately: no provider reports token
/// usage while the stream is still open, so a token count here would be an
/// estimate presented as a measurement.
///
/// No rate. Characters per second measures the wrong thing — it moves with
/// how verbose the model is being, not with how fast it is going, so the
/// number swung between paragraphs and code without anything having changed.
/// The turn summary reports tokens per second once the turn ends, where the
/// provider's own usage figures make it a real measurement.
fn metrics(model: &TurnLineModel) -> String {
    let elapsed = forge_transcript::format_elapsed_tenths(model.elapsed_secs);
    if model.chars == 0 {
        return elapsed;
    }
    format!("{elapsed} · ↓ {} chars", compact(model.chars))
}

/// Build the line, right-aligning the interrupt hint to `width`.
pub fn turn_line(model: &TurnLineModel, width: usize, millis: u128) -> Line<'static> {
    let mut state = throbber_widgets_tui::ThrobberState::default();
    for _ in 0..((millis / 140) % 8) {
        state.calc_next();
    }
    turn_line_with_throbber(model, width, &state)
}

pub fn turn_line_with_throbber(
    model: &TurnLineModel,
    width: usize,
    throbber: &throbber_widgets_tui::ThrobberState,
) -> Line<'static> {
    let mut spans = vec![
        Span::raw("  "),
        Span::raw(" "),
        Span::raw("   "),
        Span::styled(metrics(model), theme::metadata_style()),
    ];
    spans.splice(1..1, shimmer_text(throbber, &model.verb));
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
        assert!(rendered.contains("Thinking"), "{rendered}");
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
        let a = turn_line_with_throbber(
            &stalled,
            80,
            &throbber_widgets_tui::ThrobberState::default(),
        );
        let mut next = throbber_widgets_tui::ThrobberState::default();
        for _ in 0..32 {
            next.calc_next();
            let b = turn_line_with_throbber(&stalled, 80, &next);
            if a.spans[1..]
                .iter()
                .map(|span| span.style)
                .collect::<Vec<_>>()
                != b.spans[1..]
                    .iter()
                    .map(|span| span.style)
                    .collect::<Vec<_>>()
            {
                return;
            }
        }
        panic!("the spinner did not advance");
    }

    #[test]
    fn shimmer_is_applied_to_each_letter() {
        let line = turn_line_with_throbber(&model(), 80, &Default::default());
        assert_eq!(line.spans[1].content, "T");
        assert_eq!(line.spans[2].content, "h");
        assert!(line.spans.len() > 6);
        assert!(line.spans[1].style != line.spans[2].style);
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

    /// Characters per second moved with how verbose the model was being
    /// rather than how fast it was going, so it swung between a paragraph and
    /// a code block with nothing having changed. The turn summary reports
    /// tokens per second instead, where the provider's usage makes it real.
    #[test]
    fn the_live_line_reports_no_rate() {
        let slow = TurnLineModel {
            elapsed_secs: 30.0,
            chars: 30,
            ..model()
        };
        let rendered = text(&turn_line(&slow, 80, 0));
        assert!(!rendered.contains("/s"), "{rendered}");
        // The volume it is a rate of is still there.
        assert!(rendered.contains("30 chars"), "{rendered}");
    }

    #[test]
    fn counts_stay_compact() {
        assert_eq!(compact(842), "842");
        assert_eq!(compact(1_240), "1.2k");
        assert_eq!(compact(48_000), "48k");
    }
}
