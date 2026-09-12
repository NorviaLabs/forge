use crate::theme;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

const MAX_DISPLAYED_MESSAGES: usize = 3;

pub struct QueuedMessages<'a> {
    pub messages: &'a [String],
    pub selected: Option<usize>,
    /// Hovered row from pointer motion. Hover never changes the selection and
    /// never outranks it.
    pub hover: Option<usize>,
}

/// First message index in the visible window (window follows the selection).
fn window_start(len: usize, selected: Option<usize>) -> usize {
    let visible = len.min(MAX_DISPLAYED_MESSAGES);
    selected
        .unwrap_or(0)
        .saturating_sub(visible.saturating_sub(1))
        .min(len.saturating_sub(visible))
}

/// Index of the queued-message row under a pointer cell, if any. Shared by
/// pointer routing and the renderer's hover pass so hit-testing and paint can
/// never disagree. Row 0 is the strip title and is never hittable.
pub fn message_index_at(
    len: usize,
    selected: Option<usize>,
    area: Rect,
    col: u16,
    row: u16,
) -> Option<usize> {
    if len == 0 || area.width == 0 || area.height == 0 {
        return None;
    }
    if col < area.x || col >= area.right() || row <= area.y || row >= area.bottom() {
        return None;
    }
    let start = window_start(len, selected);
    let visible = len.min(MAX_DISPLAYED_MESSAGES);
    let index = start + (row - area.y - 1) as usize;
    (index < start + visible).then_some(index)
}

impl Widget for QueuedMessages<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 || self.messages.is_empty() {
            return;
        }

        let visible = self.messages.len().min(MAX_DISPLAYED_MESSAGES);
        let start = window_start(self.messages.len(), self.selected);
        let title = Line::from(vec![
            Span::styled("Queued", theme::metadata_style()),
            Span::styled(" · ↑ edit last", theme::dim()),
        ]);
        buf.set_line(area.x, area.y, &title, area.width);

        for (offset, message) in self.messages.iter().skip(start).take(visible).enumerate() {
            let index = start + offset;
            let selected = self.selected == Some(index);
            let hovered = self.hover == Some(index) && !selected;
            let normalized = message.split_whitespace().collect::<Vec<_>>().join(" ");
            let (prefix, prefix_style) = if hovered {
                (
                    format!("› {}. ", index + 1),
                    theme::accent_style().add_modifier(Modifier::BOLD),
                )
            } else {
                (format!("  {}. ", index + 1), theme::dim())
            };
            let available = area.width.saturating_sub(prefix.chars().count() as u16) as usize;
            let preview = truncate(&normalized, available);
            let style = if selected {
                theme::focused_selection_style()
            } else if hovered {
                // Ground plus a weight step: hover is a pointer affordance,
                // never a selection.
                theme::metadata_style()
                    .patch(theme::surface_hover())
                    .add_modifier(Modifier::BOLD)
            } else {
                theme::metadata_style()
            };
            let mut spans = vec![
                Span::styled(prefix, prefix_style),
                Span::styled(preview.clone(), style),
            ];
            if hovered {
                let used = spans.iter().map(Span::width).sum::<usize>();
                if used < area.width as usize {
                    spans.push(Span::styled(" ".repeat(area.width as usize - used), style));
                }
            }
            let line = Line::from(spans);
            let row = area.y.saturating_add(1 + offset as u16);
            if row < area.bottom() {
                buf.set_line(area.x, row, &line, area.width);
            }
        }

        if self.messages.len() > MAX_DISPLAYED_MESSAGES {
            let row = area.y.saturating_add(1 + visible as u16);
            if row < area.bottom() {
                let overflow = format!(
                    "  … ({}–{} of {} · {} hidden)",
                    start + 1,
                    start + visible,
                    self.messages.len(),
                    self.messages.len() - visible
                );
                buf.set_line(
                    area.x,
                    row,
                    &Line::from(Span::styled(overflow, theme::dim())),
                    area.width,
                );
            }
        }
    }
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    if width <= 1 {
        return "…".chars().take(width).collect();
    }
    text.chars()
        .take(width - 1)
        .collect::<String>()
        .trim_end()
        .to_string()
        + "…"
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn render(
        messages: &[String],
        selected: Option<usize>,
        hover: Option<usize>,
        width: u16,
        height: u16,
    ) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(
                    QueuedMessages {
                        messages,
                        selected,
                        hover,
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

    #[test]
    fn renders_bounded_normalized_queue_with_overflow() {
        let messages = vec![
            "first   message".into(),
            "second\tmessage".into(),
            "third\nmessage".into(),
            "fourth message".into(),
        ];
        let output = render(&messages, Some(1), None, 40, 5);
        assert!(output.contains("Queued · ↑ edit last"));
        assert!(output.contains("first message"));
        assert!(output.contains("second message"));
        assert!(output.contains("third message"));
        assert!(output.contains("1–3 of 4 · 1 hidden"));
        assert!(!output.contains("fourth message"));
    }

    #[test]
    fn truncates_previews_to_the_available_width() {
        let output = render(&["a very long queued message".into()], None, None, 18, 2);
        assert!(output.contains("a very long…"));
    }

    #[test]
    fn selected_message_is_kept_inside_the_visible_window() {
        let messages = (0..5).map(|i| format!("message {i}")).collect::<Vec<_>>();
        let output = render(&messages, Some(4), None, 40, 5);
        assert!(output.contains("message 4"));
        assert!(output.contains("3–5 of 5"));
        assert!(!output.contains("message 0"));
    }

    #[test]
    fn hovered_row_takes_the_marker_and_ground_but_not_the_selection() {
        let messages = vec!["alpha".into(), "beta".into()];
        let mut terminal = Terminal::new(TestBackend::new(40, 4)).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(
                    QueuedMessages {
                        messages: &messages,
                        selected: None,
                        hover: Some(1),
                    },
                    frame.area(),
                );
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        let cell = |x, y| &buf[(x, y)];
        // Row 0 is the title, row 1 the first message, row 2 the hovered one.
        assert!(cell(0, 2).symbol() == "›", "hovered row lost its marker");
        assert_eq!(
            cell(5, 2).style().bg,
            theme::surface_hover().bg,
            "hovered row lost its ground"
        );
        assert_ne!(
            cell(5, 1).style().bg,
            theme::surface_hover().bg,
            "unhovered row gained ground"
        );
        assert_eq!(
            cell(0, 1).style().fg,
            theme::dim().fg,
            "unhovered prefix style changed"
        );
    }

    #[test]
    fn selection_outranks_hover_on_the_same_row() {
        let messages = vec!["alpha".into()];
        let mut terminal = Terminal::new(TestBackend::new(40, 3)).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(
                    QueuedMessages {
                        messages: &messages,
                        selected: Some(0),
                        hover: Some(0),
                    },
                    frame.area(),
                );
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        assert_eq!(
            buf[(5, 1)].style().bg,
            theme::focused_selection_style().bg,
            "selection ground must survive hover"
        );
        assert!(
            buf[(0, 1)].symbol() != "›",
            "hover marker impersonated selection"
        );
    }

    #[test]
    fn hit_testing_matches_the_rendered_window() {
        let area = Rect::new(4, 10, 30, 5);
        // Window follows the selection: 2..5.
        assert_eq!(message_index_at(5, Some(4), area, 5, 10), None, "title row");
        assert_eq!(message_index_at(5, Some(4), area, 5, 11), Some(2));
        assert_eq!(message_index_at(5, Some(4), area, 5, 13), Some(4));
        assert_eq!(
            message_index_at(5, Some(4), area, 5, 14),
            None,
            "overflow row"
        );
        assert_eq!(message_index_at(5, Some(4), area, 3, 11), None, "outside");
    }
}
