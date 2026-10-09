use crate::theme;
use forge_types::TaskLifecycle;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::Span;
use ratatui::widgets::Widget;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStripState {
    Idle,
    Running,
    Waiting,
    Completed,
    Failed,
    Interrupted,
    Unavailable,
}

impl From<TaskLifecycle> for TaskStripState {
    fn from(value: TaskLifecycle) -> Self {
        match value {
            TaskLifecycle::Ready => Self::Idle,
            TaskLifecycle::Working => Self::Running,
            TaskLifecycle::Waiting => Self::Waiting,
            TaskLifecycle::Completed => Self::Completed,
            TaskLifecycle::Failed => Self::Failed,
            TaskLifecycle::Cancelled => Self::Idle,
            TaskLifecycle::Interrupted => Self::Interrupted,
            _ => Self::Unavailable,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskStripItem {
    pub slot: Option<u8>,
    pub label: String,
    pub branch: String,
    pub state: TaskStripState,
    pub secondary: Option<String>,
    pub selected: bool,
    pub focused: bool,
    pub attention: bool,
}

pub struct TaskStrip<'a> {
    pub items: &'a [TaskStripItem],
    pub overflow: usize,
    pub focused: bool,
}

impl TaskStrip<'_> {
    /// Shared painting and hit-test geometry. Scroll the window just enough to
    /// retain the cursor (or the open session when the strip is unfocused).
    pub fn chip_rects(&self, area: Rect) -> Vec<(usize, Rect)> {
        if area.height == 0 || area.width < 4 || self.items.is_empty() {
            return Vec::new();
        }
        let anchor = self
            .items
            .iter()
            .position(|item| {
                if self.focused {
                    item.focused
                } else {
                    item.selected
                }
            })
            .unwrap_or(0);
        let total_width = self
            .items
            .iter()
            .map(|item| (Span::raw(&item.label).width() + 4).min(32))
            .sum::<usize>()
            + self.items.len().saturating_sub(1);
        let reserve = if self.overflow > 0 || total_width > usize::from(area.width) {
            Span::raw(format!(" +{} more", self.items.len() + self.overflow - 1)).width() as u16
        } else {
            0
        };
        let budget = area.width.saturating_sub(reserve).max(4).min(area.width);
        let width = |index: usize| {
            (Span::raw(&self.items[index].label).width() + 4)
                .min(32)
                .min(budget as usize) as u16
        };
        let mut start = 0;
        while start < anchor
            && (start..=anchor)
                .map(|i| usize::from(width(i)) + usize::from(i > start))
                .sum::<usize>()
                > usize::from(budget)
        {
            start += 1;
        }
        let mut x = area.x;
        let mut rects = Vec::new();
        for index in start..self.items.len() {
            let w = width(index);
            if x + w > area.x + budget {
                break;
            }
            rects.push((index, Rect::new(x, area.y, w, area.height)));
            x += w + 1;
        }
        rects
    }
}

impl Widget for TaskStrip<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        theme::fill(area, buf, theme::panel());
        let center_y = area.y + area.height.saturating_sub(1) / 2;
        let rects = self.chip_rects(area);
        for (index, rect) in &rects {
            let item = &self.items[*index];
            let owner = self.focused && item.focused;
            // The chip is one solid tile: a single ground across every row, with
            // no per-row rule. The open session gets a filled accent ground so
            // it reads as the selected tab; only the keyboard cursor takes the
            // neutral selection ground and the `>` marker.
            let ground = if owner {
                theme::focused_selection_style()
            } else if item.selected {
                theme::text().bg(theme::accent_soft_bg())
            } else {
                theme::metadata_style()
            };
            theme::fill(*rect, buf, ground);
            // An open session reads as an active tab: a rounded accent outline
            // with a bright base rule, so the tile reads as a tab rather than a
            // plain block. It stays one component — no rule crosses a row.
            if item.selected && rect.height >= 3 {
                let border = theme::accent_style();
                let width = rect.width as usize;
                let top = format!("╭{}╮", "─".repeat(width.saturating_sub(2)));
                buf.set_string(rect.x, rect.y, &top, border);
                buf.set_string(rect.x, center_y, "│", border);
                buf.set_string(rect.right() - 1, center_y, "│", border);
                buf.set_string(rect.x, rect.bottom() - 1, "─".repeat(width), border);
            }
            let label_style = if item.selected {
                ground.add_modifier(Modifier::BOLD)
            } else {
                ground
            };
            let label = truncate(&item.label, rect.width.saturating_sub(4) as usize);
            let label_width = Span::raw(&label).width() as u16;
            // Centre the label across the chip; a focused chip nudges right only
            // when centring would not leave the two-cell `>` marker slot.
            let mut label_x = rect.x + rect.width.saturating_sub(label_width) / 2;
            if owner && label_x < rect.x + 2 {
                label_x = rect.x + 2;
            }
            if owner {
                buf.set_string(
                    label_x.saturating_sub(2).max(rect.x),
                    center_y,
                    ">",
                    ground.patch(theme::accent_style()),
                );
            }
            buf.set_string(label_x, center_y, label, label_style);
        }
        let hidden = self.items.len() + self.overflow - rects.len();
        if hidden > 0 && area.height > 0 {
            let x = rects.last().map_or(area.x, |(_, rect)| rect.right() + 1);
            if x < area.right() {
                buf.set_stringn(
                    x,
                    center_y,
                    format!("+{hidden} more"),
                    usize::from(area.right() - x),
                    theme::metadata_style(),
                );
            }
        }
    }
}

fn truncate(text: &str, width: usize) -> String {
    if Span::raw(text).width() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut result = String::new();
    for ch in text.chars() {
        if Span::raw(&result).width() + Span::raw(ch.to_string()).width() > width - 1 {
            break;
        }
        result.push(ch);
    }
    result.push('…');
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn item(label: &str, selected: bool, focused: bool) -> TaskStripItem {
        TaskStripItem {
            slot: None,
            label: label.into(),
            branch: String::new(),
            state: TaskStripState::Idle,
            secondary: None,
            selected,
            focused,
            attention: false,
        }
    }
    #[test]
    fn selected_chip_draws_a_tab_outline() {
        let items = [item("forge", true, false)];
        let area = Rect::new(0, 0, 20, 3);
        let strip = TaskStrip {
            items: &items,
            overflow: 0,
            focused: false,
        };
        let rect = strip.chip_rects(area)[0].1;
        let mut buf = Buffer::empty(area);
        strip.render(area, &mut buf);
        let center_y = rect.y + rect.height / 2;
        assert_eq!(buf[(rect.x, rect.y)].symbol(), "╭");
        assert_eq!(buf[(rect.right() - 1, rect.y)].symbol(), "╮");
        assert_eq!(buf[(rect.x, center_y)].symbol(), "│");
        assert_eq!(buf[(rect.right() - 1, center_y)].symbol(), "│");
        assert_eq!(buf[(rect.x, rect.bottom() - 1)].symbol(), "─");
        assert_eq!(buf[(rect.right() - 1, rect.bottom() - 1)].symbol(), "─");
        assert_eq!(buf[(rect.x, center_y)].bg, theme::accent_soft_bg());
    }

    #[test]
    fn chips_have_fixed_gutters_and_separate_open_and_cursor_styles() {
        let items = [item("first", true, false), item("second", false, true)];
        let area = Rect::new(2, 1, 60, 1);
        let strip = TaskStrip {
            items: &items,
            overflow: 0,
            focused: true,
        };
        let rects = strip.chip_rects(area);
        assert_eq!(rects[1].1.x, rects[0].1.right() + 1);
        let mut buf = Buffer::empty(area);
        strip.render(area, &mut buf);
        let label_x = |rect: Rect, label: &str| {
            rect.x + rect.width.saturating_sub(Span::raw(label).width() as u16) / 2
        };
        // The focused chip carries the `>` marker in the slot left of the label.
        let focused = rects[1].1;
        let focused_label = label_x(focused, "second");
        assert_eq!(buf[(focused.x, 1)].symbol(), ">");
        assert_eq!(buf[(focused_label, 1)].symbol(), "s");
        // The open-but-unfocused chip is a solid accent tile with a bold label,
        // and no per-row rule anywhere on the strip.
        let selected = rects[0].1;
        let selected_label = label_x(selected, "first");
        assert_eq!(buf[(selected_label, 1)].bg, theme::accent_soft_bg());
        assert!(buf[(selected_label, 1)].modifier.contains(Modifier::BOLD));
        assert_ne!(buf[(selected.x, 1)].symbol(), ">");
        for (_, rect) in &rects {
            for x in rect.x..rect.right() {
                assert!(!buf[(x, rect.y)].modifier.contains(Modifier::UNDERLINED));
            }
        }
    }
    #[test]
    fn overflow_keeps_the_cursor_visible_and_uses_cell_width() {
        let items = [
            item("a very long session name", true, false),
            item("second", false, false),
            item("界界界界界界界界", false, true),
        ];
        let area = Rect::new(0, 0, 22, 1);
        let strip = TaskStrip {
            items: &items,
            overflow: 0,
            focused: true,
        };
        let rects = strip.chip_rects(area);
        assert!(rects.iter().any(|(i, _)| *i == 2));
        assert!(rects.iter().all(|(_, r)| r.right() <= area.right()));
        let mut buf = Buffer::empty(area);
        strip.render(area, &mut buf);
        let text: String = buf.content.iter().map(|cell| cell.symbol()).collect();
        assert!(text.contains("+2 more"), "{text}");
        assert_eq!(truncate("界界界", 4), "界…");
    }
}
