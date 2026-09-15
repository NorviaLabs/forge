//! The sidebar's background-activity strip.
//!
//! Docked between the outbound-message queue and the composer, scoped to the
//! sidebar's width. The footer's counts-only chip says *that* work is running;
//! this says *what* — one row per task, with the state marker, the label, an
//! elapsed column and, for a task blocked on an operator decision, the tool and
//! request underneath it.
//!
//! The strip is deliberately not a log: a `[✓]` row retires on its own after
//! [`DONE_ROW_TTL_SECS`], and everything else waits for `x`.

use crate::tasks_strip::{BackgroundStrip, StripRow, StripState};
use crate::theme;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::Widget;

/// Cells reserved for the selection pointer, matching the navigator's rows.
const GUTTER: usize = 2;
/// Cells for a §5.3 state marker. Every marker is exactly three.
const MARKER: usize = 3;

pub struct BackgroundStripWidget<'a> {
    pub strip: &'a BackgroundStrip,
    /// Index of the row the operator has selected with `↑↓`. This is the whole
    /// reason the strip exists: the keys already worked, but with nothing drawn
    /// the selection was invisible.
    pub selected: Option<usize>,
    /// Whether the Sidebar block owns the keyboard. A selection inside an
    /// inactive block stays visible but muted and must not imply ownership.
    pub focused: bool,
}

impl Widget for BackgroundStripWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let width = area.width as usize;
        self.render_header(area, buf, width);
        if area.height < 2 {
            return;
        }
        let mut lines_above = 0u16;
        for (index, row) in self.strip.rows.iter().enumerate() {
            let y = area.y + 1 + lines_above;
            if y >= area.y + area.height {
                break;
            }
            let selected = self.selected == Some(index);
            self.render_row(row, selected, area.x, y, width, buf);
            // A row with a second line is two lines tall; advancing by one
            // would draw the next row over that line.
            lines_above += 1 + u16::from(row.detail.is_some());
        }
    }
}

impl BackgroundStripWidget<'_> {
    /// `  Background · 4 ──────── +2 more`.
    ///
    /// The count is the total that survived expiry, so the header stays
    /// truthful even when the row cap is what is hiding the difference.
    fn render_header(&self, area: Rect, buf: &mut Buffer, width: usize) {
        let total = self.strip.total.to_string();
        let mut spans = vec![
            Span::styled("  Background", theme::border_muted()),
            Span::styled(" · ", theme::border_muted()),
            Span::styled(total, theme::text_secondary()),
            Span::styled(" ", theme::border_muted()),
        ];
        let used: usize = spans.iter().map(Span::width).sum();

        let tail = if self.strip.hidden > 0 {
            format!(" +{} more ", self.strip.hidden)
        } else {
            String::new()
        };
        let tail_width = tail.chars().count();
        let fill_to = width.saturating_sub(tail_width);
        if fill_to > used {
            spans.push(Span::styled(
                "─".repeat(fill_to - used),
                theme::border_muted(),
            ));
        }
        if !tail.is_empty() {
            spans.push(Span::styled(tail, theme::dim()));
        }
        buf.set_line(area.x, area.y, &spans.into(), area.width);
    }

    fn render_row(
        &self,
        row: &StripRow,
        selected: bool,
        x: u16,
        y: u16,
        width: usize,
        buf: &mut Buffer,
    ) {
        // The selected row takes the neutral `selection` ground plus the `>`
        // pointer, the same treatment the navigator's rows get. A selection in
        // an unfocused block keeps the ground but drops to secondary text, so
        // it never reads as keyboard ownership (§8.5).
        let (row_style, pointer) = match (selected, self.focused) {
            (true, true) => (
                theme::focused_selection_style(),
                Span::styled("> ", theme::accent_style().add_modifier(Modifier::BOLD)),
            ),
            (true, false) => (
                theme::focused_selection_style().patch(theme::dim()),
                Span::styled("> ", theme::dim()),
            ),
            (false, _) => (theme::text_secondary(), Span::raw("  ")),
        };
        if selected {
            theme::fill(Rect::new(x, y, width as u16, 1), buf, row_style);
        }

        let mut spans: Vec<Span<'static>> = Vec::new();
        spans.push(pointer);
        spans.push(Span::styled(
            row.marker(),
            if selected {
                row_style
            } else {
                state_style(row.state).add_modifier(Modifier::BOLD)
            },
        ));
        spans.push(Span::styled(" ", row_style));
        spans.push(Span::styled(
            row.glyph(),
            if selected {
                row_style
            } else {
                theme::text_secondary()
            },
        ));
        spans.push(Span::styled(" ", row_style));

        let used: usize = spans.iter().map(Span::width).sum();
        let elapsed = format!(" {} ", row.elapsed);
        let elapsed_width = elapsed.chars().count();
        let label_budget = width
            .saturating_sub(used)
            .saturating_sub(elapsed_width.saturating_sub(1));
        spans.push(Span::styled(truncate(&row.label, label_budget), row_style));

        let used: usize = spans.iter().map(Span::width).sum();
        if used + elapsed_width <= width {
            spans.push(Span::styled(
                " ".repeat(width - used - elapsed_width),
                row_style,
            ));
            spans.push(Span::styled(
                elapsed,
                if selected { row_style } else { theme::dim() },
            ));
        }
        buf.set_line(x, y, &spans.into(), width as u16);

        // A second line under the row: the request a blocked row is waiting on,
        // or what an active subagent is doing. A request is something the
        // operator has to answer, so it is coloured as a warning; reported
        // activity is not a problem and stays secondary, so the two never look
        // alike.
        if let Some(detail) = row.detail.as_deref() {
            if y + 1 < buf.area.height {
                let indent = GUTTER + MARKER + 1;
                let text = format!("{}{}", " ".repeat(indent), detail);
                let style = if row.state == StripState::Blocked {
                    theme::warn()
                } else {
                    theme::text_secondary()
                };
                let line = ratatui::text::Line::from(Span::styled(truncate(&text, width), style));
                buf.set_line(x, y + 1, &line, width as u16);
            }
        }
    }
}

/// A marker's hue. Colour never travels alone (§5.4): the marker glyph carries
/// the state and this only reinforces it.
fn state_style(state: StripState) -> Style {
    match state {
        StripState::Blocked => theme::warn(),
        StripState::Failed => theme::danger(),
        StripState::Active => theme::activity(),
        StripState::Queued => theme::dim(),
        StripState::Done => theme::ok(),
        StripState::Cancelled => theme::dim(),
    }
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    if width <= 1 {
        return "…".chars().take(width).collect();
    }
    let kept: String = text.chars().take(width - 1).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn row(state: StripState, label: &str, elapsed: &str) -> StripRow {
        StripRow {
            state,
            subagent: true,
            label: label.into(),
            elapsed: elapsed.into(),
            detail: None,
        }
    }

    fn draw(
        strip: &BackgroundStrip,
        selected: Option<usize>,
        focused: bool,
        w: u16,
        h: u16,
    ) -> String {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(
                    BackgroundStripWidget {
                        strip,
                        selected,
                        focused,
                    },
                    frame.area(),
                );
            })
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol().to_string())
            .collect()
    }

    fn strip_of(rows: Vec<StripRow>, hidden: usize) -> BackgroundStrip {
        BackgroundStrip {
            total: rows.len() + hidden,
            rows,
            hidden,
        }
    }

    #[test]
    fn rows_render_marker_label_and_right_aligned_elapsed() {
        let strip = strip_of(
            vec![
                row(StripState::Active, "explore · find auth code", "12s"),
                row(StripState::Failed, "cargo clippy", "2m"),
            ],
            0,
        );

        let text = draw(&strip, None, false, 44, 3);

        assert!(text.contains("Background"), "missing header: {text}");
        assert!(text.contains("[>]"), "missing active marker: {text}");
        assert!(text.contains("[!]"), "missing failed marker: {text}");
        assert!(text.contains("explore · find auth code"), "{text}");
        // The elapsed column is right-aligned, so it ends the row.
        assert!(text.contains("12s"), "{text}");
        assert!(
            !text.contains("+"),
            "nothing is hidden, so no overflow: {text}"
        );
    }

    #[test]
    fn hidden_rows_are_counted_on_the_header() {
        let strip = strip_of(
            vec![
                row(StripState::Blocked, "explore", "9s"),
                row(StripState::Active, "verify", "31s"),
            ],
            5,
        );

        let text = draw(&strip, None, false, 44, 3);

        assert!(text.contains("+5 more"), "missing overflow count: {text}");
    }

    /// The blocked row is the one that has to say what it is waiting for.
    #[test]
    fn a_blocked_row_draws_its_request_underneath() {
        let mut blocked = row(StripState::Blocked, "explore · audit auth deps", "9s");
        blocked.detail = Some("bash · rm -rf target/debug".into());
        let strip = strip_of(vec![blocked], 0);

        let text = draw(&strip, None, false, 44, 3);

        assert!(text.contains("[|]"), "{text}");
        assert!(
            text.contains("bash · rm -rf target/debug"),
            "the pending request must be visible: {text}"
        );
    }

    /// A row with a second line is two lines tall. Advancing one line per row
    /// drew the next row straight over the line underneath — which is why this
    /// only ever looked right on a one-row strip.
    #[test]
    fn a_second_line_is_not_overwritten_by_the_next_row() {
        let mut blocked = row(StripState::Blocked, "explore", "9s");
        blocked.detail = Some("bash · echo risky".into());
        let verify = row(StripState::Active, "verify", "31s");
        let strip = strip_of(vec![blocked, verify], 0);

        // Header + two rows + one second line.
        let text = draw(&strip, None, false, 44, 4);

        assert!(
            text.contains("bash · echo risky"),
            "the second line survived: {text}"
        );
        assert!(
            text.contains("verify"),
            "and the row after it is still drawn: {text}"
        );
        // `draw` flattens the buffer into one string in row order, so the
        // second line has to appear before the row that follows it.
        let detail_at = text.find("bash · echo risky").expect("detail line");
        let verify_at = text.find("verify").expect("next row");
        assert!(
            detail_at < verify_at,
            "the second line belongs to the row above, not below: {text}"
        );
    }

    /// The selection is what the strip was built to reveal; before this the
    /// operator moved an invisible cursor.
    #[test]
    fn the_selected_row_takes_the_pointer_and_the_ground() {
        let strip = strip_of(
            vec![
                row(StripState::Active, "explore", "12s"),
                row(StripState::Active, "verify", "31s"),
            ],
            0,
        );

        let text = draw(&strip, Some(1), true, 44, 3);

        // The pointer plus the marker, so this distinguishes the selected row
        // from its sibling rather than counting `>` glyphs that the `[>]`
        // markers themselves contain.
        assert!(
            text.contains("> [>] ◆ verify"),
            "the selected row needs the pointer: {text}"
        );
        assert!(
            text.contains("  [>] ◆ explore"),
            "an unselected row must not take the pointer: {text}"
        );
    }

    /// Rows past the region are dropped rather than drawn over the composer.
    #[test]
    fn rows_never_draw_past_the_region() {
        let strip = strip_of(
            vec![
                row(StripState::Active, "one", "1s"),
                row(StripState::Active, "two", "2s"),
                row(StripState::Active, "three", "3s"),
            ],
            0,
        );

        let text = draw(&strip, None, false, 44, 2);

        assert!(text.contains("one"), "{text}");
        assert!(!text.contains("two"), "a clipped row must not draw: {text}");
    }
}
