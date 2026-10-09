//! The first task groups its heading, editable prompt and useful starters.

use crate::theme;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget, Wrap};

pub(crate) const STARTERS: [&str; 3] = [
    "Explain the architecture of this project",
    "Review the changes in this workspace",
    "Find and fix a failing test",
];

pub(crate) struct StartGroup {
    pub area: Rect,
    pub input: Rect,
    pub starters: usize,
    condensed: bool,
}

impl StartGroup {
    pub fn new(body: Rect, input_height: u16, frame_height: u16) -> Self {
        let width = body.width.min(crate::design::HOME_MAX_WIDTH);
        // Fit choices to the remaining body, including the actual draft
        // height. Compact terminals still show all choices when they fit.
        let condensed = body.height < input_height + 8;
        let heading_h = if condensed { 1 } else { 3 };
        let fixed_h = heading_h + input_height + if condensed { 1 } else { 2 };
        let starters = body
            .height
            .saturating_sub(fixed_h)
            .clamp(1, STARTERS.len() as u16) as usize;
        let height = (fixed_h + starters as u16).min(body.height);
        let area = Rect::new(
            body.x + body.width.saturating_sub(width) / 2,
            body.y + body.height.saturating_sub(height) / if frame_height < 24 { 2 } else { 3 },
            width,
            height,
        );
        Self {
            input: Rect::new(area.x, area.y + heading_h, area.width, input_height),
            area,
            starters,
            condensed,
        }
    }

    pub fn render(
        &self,
        buf: &mut Buffer,
        connected: bool,
        selected: usize,
        focused: bool,
        hover: Option<usize>,
    ) -> Vec<(usize, Rect)> {
        let inset = crate::design::COMPOSER_PAD_X;
        let x = self.area.x + inset;
        let width = self.area.width.saturating_sub(inset * 2);
        Paragraph::new("What would you like to work on?")
            .style(theme::text().add_modifier(Modifier::BOLD))
            .render(Rect::new(x, self.area.y, width, 1), buf);
        if !self.condensed {
            Paragraph::new(if connected {
                "Describe a task, or choose a starting point."
            } else {
                "Connect a provider to start working."
            })
            .style(if connected {
                theme::text_secondary()
            } else {
                theme::warn()
            })
            .render(Rect::new(x, self.area.y + 1, width, 1), buf);
        }

        let mut y = self.input.bottom() + u16::from(!self.condensed);
        Paragraph::new(if focused {
            "> Starting points"
        } else {
            "Starting points"
        })
        .style(if focused {
            theme::accent_style()
        } else {
            theme::muted()
        })
        .render(Rect::new(x, y, width, 1), buf);
        y += 1;
        let mut rows = Vec::new();
        for (index, prompt) in STARTERS.iter().take(self.starters).enumerate() {
            if y >= self.area.bottom() {
                break;
            }
            let active = focused && index == selected;
            let area = Rect::new(x, y, width, 1);
            if active {
                theme::fill(area, buf, theme::selected_row());
            } else if hover == Some(index) {
                theme::fill(area, buf, theme::surface_hover());
            }
            let line = Line::from(vec![
                Span::styled(
                    if active { "> " } else { "› " },
                    if active {
                        theme::accent_style()
                    } else {
                        theme::muted()
                    },
                ),
                Span::styled(*prompt, theme::text_secondary()),
            ]);
            Paragraph::new(line)
                .wrap(Wrap { trim: false })
                .render(area, buf);
            rows.push((index, area));
            y += 1;
        }
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn condensed_start_keeps_draft_and_choices_inside_a_short_body() {
        for (height, input_height) in [(8, 4), (9, 5), (13, 2), (30, 4)] {
            let body = Rect::new(1, 5, 78, height);
            let group = StartGroup::new(body, input_height, 18);
            let mut buffer = Buffer::empty(body);
            let rows = group.render(&mut buffer, true, 0, false, None);
            assert_eq!(group.input.height, input_height);
            assert!(body.contains(group.input.as_position()));
            assert!(group.input.bottom() <= body.bottom());
            assert_eq!(rows.len(), group.starters);
            for (_, row) in rows {
                assert!(row.y >= group.input.bottom());
                assert!(row.bottom() <= body.bottom());
            }
        }
    }
}
