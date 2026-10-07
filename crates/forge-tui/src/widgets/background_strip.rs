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
    /// Identity of the task the operator has selected with `↑↓`. This is the whole
    /// reason the strip exists: the keys already worked, but with nothing drawn
    /// the selection was invisible.
    pub selected: Option<forge_types::BackgroundTaskId>,
    /// Row under the pointer (pointer motion only; never moves the
    /// selection). Painted as `›` in the row's own reserved gutter cell, so
    /// hover can never shift text.
    pub hover: Option<usize>,
    /// Whether the Sidebar block owns the keyboard. A selection inside an
    /// inactive block stays visible but muted and must not imply ownership.
    pub focused: bool,
}

/// Rows are one line each, plus a second for a row that carries a detail line.
/// Paint and hit-testing both step by this, so a row the pointer addresses is
/// always the row the operator sees.
fn row_height(row: &StripRow) -> u16 {
    1 + u16::from(row.detail.is_some())
}

/// Index of the strip row under a pointer cell, if any. Row 0 is the strip
/// header and is never hittable. Shared with the renderer through
/// [`row_height`] so the two can never disagree.
pub fn row_index_at(rows: &[StripRow], area: Rect, col: u16, row: u16) -> Option<usize> {
    if area.width == 0 || area.height == 0 {
        return None;
    }
    if col < area.x || col >= area.right() || row <= area.y || row >= area.bottom() {
        return None;
    }
    let mut y = area.y + 1;
    for (index, entry) in rows.iter().enumerate() {
        let height = row_height(entry);
        if row < y + height {
            return Some(index);
        }
        y += height;
    }
    None
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
            let selected = self.selected == Some(row.id);
            let hovered = self.hover == Some(index);
            self.render_row(
                row,
                selected,
                hovered,
                Rect::new(
                    area.x,
                    y,
                    width as u16,
                    row_height(row).min(area.bottom() - y),
                ),
                buf,
            );
            // A row with a second line is two lines tall; advancing by one
            // would draw the next row over that line.
            lines_above += row_height(row);
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
            Span::styled("  Background", theme::metadata_style()),
            Span::styled(" · ", theme::metadata_style()),
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
        hovered: bool,
        area: Rect,
        buf: &mut Buffer,
    ) {
        let Rect { x, y, width, .. } = area;
        let width = width as usize;
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
            // Hover is a pointer affordance on a row that can be acted on: a
            // raised ground plus the non-colour `›` marker, in the row's own
            // reserved gutter, so it never shifts text (§8.6).
            (false, _) if hovered => (
                theme::text().patch(theme::surface_hover()),
                Span::styled("› ", theme::active_panel_border()),
            ),
            (false, _) => (theme::text_secondary(), Span::raw("  ")),
        };
        if selected {
            theme::fill(Rect::new(x, y, width as u16, 1), buf, row_style);
        } else if hovered {
            theme::fill(
                Rect::new(x, y, width as u16, 1),
                buf,
                theme::surface_hover(),
            );
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
        spans.push(Span::styled(
            crate::decision::preview(&row.label, label_budget),
            row_style,
        ));

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
            if area.height > 1 {
                let indent = GUTTER + MARKER + 1;
                let text = format!("{}{}", " ".repeat(indent), detail);
                let style = if row.state == StripState::Blocked {
                    theme::warn()
                } else {
                    theme::text_secondary()
                };
                let line = ratatui::text::Line::from(Span::styled(
                    crate::decision::preview(&text, width),
                    style,
                ));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn row(state: StripState, label: &str, elapsed: &str) -> StripRow {
        StripRow {
            id: forge_types::BackgroundTaskId(1),
            state,
            subagent: true,
            label: label.into(),
            elapsed: elapsed.into(),
            detail: None,
        }
    }

    fn strip_of(rows: Vec<StripRow>, hidden: usize) -> BackgroundStrip {
        BackgroundStrip {
            total: rows.len() + hidden,
            rows,
            hidden,
            start: 0,
        }
    }

    /// Hit-testing must agree with painting cell for cell, including for a
    /// row whose detail line makes it two lines tall. Row 0 is the header and
    /// is not hittable.
    #[test]
    fn row_index_at_addresses_the_painted_rows() {
        let mut tall = row(StripState::Blocked, "explore", "9s");
        tall.detail = Some("bash cargo clippy".into());
        let strip = strip_of(
            vec![
                row(StripState::Active, "one", "1s"),
                tall,
                row(StripState::Active, "three", "3s"),
            ],
            0,
        );
        let area = Rect::new(0, 10, 44, 6);

        // Header, then row 0 on the line under it.
        assert_eq!(row_index_at(&strip.rows, area, 4, 10), None);
        assert_eq!(row_index_at(&strip.rows, area, 4, 11), Some(0));
        // The blocked row owns two lines, so the next row starts below both.
        assert_eq!(row_index_at(&strip.rows, area, 4, 12), Some(1));
        assert_eq!(row_index_at(&strip.rows, area, 4, 13), Some(1));
        assert_eq!(row_index_at(&strip.rows, area, 4, 14), Some(2));
        // Past the last row, and outside the region's columns.
        assert_eq!(row_index_at(&strip.rows, area, 4, 15), None);
        assert_eq!(row_index_at(&strip.rows, area, 44, 12), None);
    }
}
