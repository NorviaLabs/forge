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
}

impl StartGroup {
    pub fn new(body: Rect, input_height: u16, frame_height: u16) -> Self {
        let width = body.width.min(crate::design::HOME_MAX_WIDTH);
        let starters = if frame_height < 24 { 1 } else { STARTERS.len() };
        let height = (3 + input_height + 2 + starters as u16 + 2).min(body.height);
        let area = Rect::new(
            body.x + body.width.saturating_sub(width) / 2,
            body.y + body.height.saturating_sub(height) / 2,
            width,
            height,
        );
        Self {
            input: Rect::new(area.x, area.y + 3, area.width, input_height),
            area,
            starters,
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
        Paragraph::new(if connected {
            "Describe a task, or choose a starting point."
        } else {
            "Connect a provider with /connect to start working."
        })
        .style(if connected {
            theme::text_secondary()
        } else {
            theme::warn()
        })
        .render(Rect::new(x, self.area.y + 1, width, 1), buf);

        let mut y = self.input.bottom() + 1;
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
        if y + 1 < self.area.bottom() {
            Paragraph::new(if focused {
                "↑↓ choose · Enter use · Tab prompt"
            } else {
                "/ commands · Ctrl+P files · Tab navigate"
            })
            .style(theme::muted())
            .render(Rect::new(x, y + 1, width, 1), buf);
        }
        rows
    }
}
