//! Waiting-for-first-token placeholder, backed by
//! [`tui_skeleton`](https://crates.io/crates/tui-skeleton).
//!
//! While a turn is in flight but nothing has arrived yet, the transcript
//! shows only the turn line (`Waiting for the model…`) above an empty pane —
//! a stall and a slow provider look identical. These placeholder rows mark
//! where the answer will land: a calm shimmer, no spinner duplication, no
//! extra chrome. Shown only behind the existing busy debounce, so instant
//! turns never flash it.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use tui_skeleton::{AnimationMode, SkeletonText};

/// Placeholder rows painted while waiting. Three short rows read as "an
/// answer is coming" without pretending to know its shape.
pub const WAITING_ROWS: u16 = 3;

/// Ragged trailing edge so the block reads as prose-to-come, not a bar.
const WIDTHS: [f32; 3] = [0.85, 0.65, 0.45];

/// Narrowest pane the placeholder bothers with — below this the turn line
/// alone carries the waiting state.
const MIN_WIDTH: usize = 16;

/// Build placeholder lines for `width` columns, animated by `elapsed_ms`
/// since the turn started. Pure: same inputs, same lines.
pub fn waiting_lines(elapsed_ms: u64, width: usize) -> Vec<Line<'static>> {
    if width < MIN_WIDTH {
        return Vec::new();
    }
    // Cap the paint cost on very wide panes; the ragged widths carry the
    // shape, not the full row.
    let paint_w = (width.min(120)) as u16;
    let area = Rect::new(0, 0, paint_w, WAITING_ROWS);
    let mut buf = Buffer::empty(area);
    SkeletonText::new(elapsed_ms)
        .mode(AnimationMode::Sweep)
        .line_widths(&WIDTHS)
        .render(area, &mut buf);
    (0..WAITING_ROWS)
        .map(|y| {
            let mut spans: Vec<Span<'static>> = Vec::new();
            for x in 0..paint_w {
                let cell = &buf[(x, y)];
                let symbol = cell.symbol().to_string();
                let style = cell.style();
                match spans.last_mut() {
                    Some(last) if last.style == style => {
                        last.content.to_mut().push_str(&symbol);
                    }
                    _ => spans.push(Span::styled(symbol, style)),
                }
            }
            Line::from(spans)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paints_three_rows_at_width() {
        let lines = waiting_lines(1_000, 60);
        assert_eq!(lines.len(), WAITING_ROWS as usize);
        for line in &lines {
            assert!(!line.spans.is_empty());
        }
    }

    #[test]
    fn narrow_panes_get_nothing() {
        assert!(waiting_lines(1_000, MIN_WIDTH - 1).is_empty());
        assert!(!waiting_lines(1_000, MIN_WIDTH).is_empty());
    }

    #[test]
    fn animation_is_a_pure_function_of_time() {
        let text = |lines: Vec<Line<'static>>| {
            lines
                .iter()
                .flat_map(|line| {
                    line.spans
                        .iter()
                        .map(|span| span.content.to_string())
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
                .join("")
        };
        // Same timestamp, same output — safe to rebuild every frame.
        assert_eq!(text(waiting_lines(500, 60)), text(waiting_lines(500, 60)));
    }

    #[test]
    fn placeholder_marks_cells_beyond_plain_space() {
        // The sweep must actually paint: at least one cell differs from a
        // blank buffer, or this is decoration pretending to load.
        let lines = waiting_lines(1_000, 60);
        let flat: String = lines
            .iter()
            .flat_map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.to_string())
                    .collect::<Vec<_>>()
            })
            .collect();
        assert!(
            flat.chars().any(|c| c != ' '),
            "placeholder should paint cells"
        );
    }
}
