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

/// Text area inside the strip: the queue has no border, so it pads directly
/// to the sidebar's shared text origin (same inset as the composer and the
/// conversation), keeping one left edge for the whole column.
fn inner_area(area: Rect) -> Rect {
    let inset = crate::widgets::input::TEXT_INSET.min(area.width / 2);
    Rect {
        x: area.x.saturating_add(inset),
        width: area.width.saturating_sub(inset.saturating_mul(2)),
        ..area
    }
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
    let inner = inner_area(area);
    if col < inner.x || col >= inner.right() || row <= area.y || row >= area.bottom() {
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

        let inner = inner_area(area);
        if inner.width == 0 {
            return;
        }
        let visible = self.messages.len().min(MAX_DISPLAYED_MESSAGES);
        let start = window_start(self.messages.len(), self.selected);
        let title = Line::from(vec![
            Span::styled("Queued", theme::metadata_style()),
            Span::styled(" · ↑ edit last", theme::dim()),
        ]);
        buf.set_line(inner.x, area.y, &title, inner.width);

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
            let available = inner.width.saturating_sub(prefix.chars().count() as u16) as usize;
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
                if used < inner.width as usize {
                    spans.push(Span::styled(" ".repeat(inner.width as usize - used), style));
                }
            }
            let line = Line::from(spans);
            let row = area.y.saturating_add(1 + offset as u16);
            if row < area.bottom() {
                buf.set_line(inner.x, row, &line, inner.width);
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
                    inner.x,
                    row,
                    &Line::from(Span::styled(overflow, theme::dim())),
                    inner.width,
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
