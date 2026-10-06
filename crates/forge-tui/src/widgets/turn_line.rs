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
//! The line sits where the answer is about to appear: a braille spinner in
//! the activity token (fixed width, stepped once per event-loop tick), the
//! evidence-backed phase in bold primary, elapsed time secondary.
//! Motion comes from the elapsed tick plus the spinner alone — no
//! per-letter shimmer, no character counter, no duplicate busy state
//! in the footer.

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

/// The running marker's frame set: heavy braille, one cell per frame.
///
/// Shared with the navigator's session rows, so the two surfaces that mean
/// "work is happening" cannot drift apart. One step per event-loop tick.
pub(crate) const SPINNER_FRAMES: [&str; 8] = ["⣾", "⣽", "⣻", "⢿", "⡿", "⣟", "⣯", "⣷"];

/// Build the line, right-aligning the interrupt hint to `width`.
///
/// The leading marker is a braille spinner frame (`⣾⣽⣻⢿⡿⣟⣯⣷`),
/// stepped once per 200ms event-loop tick. Every frame is one cell wide in
/// the activity token, so the row never shifts width.
pub fn turn_line(model: &TurnLineModel, width: usize, millis: u128) -> Line<'static> {
    let frame = SPINNER_FRAMES[(millis / 200 % SPINNER_FRAMES.len() as u128) as usize];
    let marker_style = theme::activity().add_modifier(Modifier::BOLD);
    let mut spans = vec![
        Span::styled(frame, marker_style),
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
