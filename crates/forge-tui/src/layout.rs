//! Full-screen layout splits (TUI-01 / tui-shell.md + Phase 10 feedback strip).

use ratatui::layout::{Constraint, Direction, Layout, Rect};

/// Minimum terminal size for a usable TUI.
pub const MIN_WIDTH: u16 = 80;
pub const MIN_HEIGHT: u16 = 18;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutRegions {
    pub chat: Rect,
    pub sidebar: Option<Rect>,
    /// Phase 10 / TUI-08 — 0-height when empty.
    pub feedback: Rect,
    /// Outbound message queue (click a row to cancel). 0-height when empty.
    pub queue: Rect,
    pub input: Rect,
    pub footer: Rect,
}

/// Split terminal; `feedback_h` is 0 or 1 (feedback strip).
pub fn split_areas(area: Rect) -> LayoutRegions {
    split_areas_ex(area, 0)
}

pub fn split_areas_ex(area: Rect, feedback_h: u16) -> LayoutRegions {
    split_areas_full(area, feedback_h, 3, true, 0)
}

/// Full layout control: input height, optional sidebar, queue strip height.
pub fn split_areas_full(
    area: Rect,
    feedback_h: u16,
    input_h: u16,
    show_sidebar: bool,
    queue_h: u16,
) -> LayoutRegions {
    let fb = feedback_h.min(2);
    let input_h = input_h.clamp(3, 8);
    let qh = queue_h.min(8);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),          // main
            Constraint::Length(fb),      // feedback
            Constraint::Length(qh),      // message queue
            Constraint::Length(input_h), // input (multi-line)
            Constraint::Length(1),       // footer
        ])
        .split(area);

    let main = rows[0];
    let feedback = rows[1];
    let queue = rows[2];
    let input = rows[3];
    let footer = rows[4];

    let _ = show_sidebar;

    LayoutRegions {
        chat: main,
        sidebar: None,
        feedback,
        queue,
        input,
        footer,
    }
}

pub fn is_too_small(area: Rect) -> bool {
    area.width < 40 || area.height < MIN_HEIGHT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_layout_uses_full_width_for_chat() {
        let area = Rect::new(0, 0, 120, 40);
        let r = split_areas(area);
        assert!(r.sidebar.is_none());
        assert_eq!(r.chat.width, area.width);
        assert_eq!(r.footer.height, 1);
        assert_eq!(r.input.height, 3);
        assert_eq!(r.feedback.height, 0);
        assert_eq!(r.queue.height, 0);
        assert_eq!(r.footer.y + r.footer.height, area.height);
    }

    #[test]
    fn queue_strip_height_reserved() {
        let area = Rect::new(0, 0, 120, 40);
        let r = split_areas_full(area, 0, 3, true, 3);
        assert_eq!(r.queue.height, 3);
    }

    #[test]
    fn feedback_row_reserved_when_requested() {
        let area = Rect::new(0, 0, 100, 30);
        let r = split_areas_ex(area, 1);
        assert_eq!(r.feedback.height, 1);
        assert!(r.feedback.y + r.feedback.height <= r.queue.y || r.queue.height == 0);
        assert!(r.feedback.y + r.feedback.height <= r.input.y);
    }

    #[test]
    fn narrow_layout_hides_sidebar() {
        let area = Rect::new(0, 0, 60, 24);
        let r = split_areas(area);
        assert!(r.sidebar.is_none());
        assert_eq!(r.chat.width, 60);
    }

    #[test]
    fn min_size_guard() {
        assert!(is_too_small(Rect::new(0, 0, 30, 10)));
        assert!(!is_too_small(Rect::new(0, 0, 100, 30)));
    }
}
