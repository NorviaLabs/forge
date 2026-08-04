//! Full-screen layout splits (TUI-01 / tui-shell.md + Phase 10 feedback strip).

use ratatui::layout::{Constraint, Direction, Layout, Rect};

/// Minimum terminal size for a usable TUI.
pub const MIN_WIDTH: u16 = 80;
pub const MIN_HEIGHT: u16 = 18;
const CONTENT_WIDTH_PERCENT: u32 = 95;
/// Composer input band (visual lines + chrome), capped for normal chat.
pub const MAX_COMPOSER_INPUT_H: u16 = 8;
/// Bottom theme picker dock: fits built-in themes without scrolling; scrolls for more.
pub const THEME_DOCK_H: u16 = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutRegions {
    pub status: Rect,
    pub chat: Rect,
    pub files: Option<Rect>,
    /// Contextual bottom panel. 0-height when closed or space is tight.
    pub bottom_panel: Rect,
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
    split_areas_full(area, feedback_h, 3, 0)
}

/// Full layout control: input height and queue strip height.
pub fn split_areas_full(area: Rect, feedback_h: u16, input_h: u16, queue_h: u16) -> LayoutRegions {
    split_areas_with_bottom_panel(area, feedback_h, input_h, queue_h, 0)
}

/// Full layout control plus optional bottom panel height.
pub fn split_areas_with_bottom_panel(
    area: Rect,
    feedback_h: u16,
    input_h: u16,
    queue_h: u16,
    bottom_panel_h: u16,
) -> LayoutRegions {
    split_areas_with_chrome(area, feedback_h, input_h, false, queue_h, bottom_panel_h, 0)
}

#[allow(dead_code)]
pub fn split_areas_with_side_panels(
    area: Rect,
    feedback_h: u16,
    input_h: u16,
    show_files: bool,
    queue_h: u16,
    bottom_panel_h: u16,
) -> LayoutRegions {
    split_areas_with_chrome(
        area,
        feedback_h,
        input_h,
        show_files,
        queue_h,
        bottom_panel_h,
        0,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn split_areas_with_chrome(
    area: Rect,
    feedback_h: u16,
    input_h: u16,
    show_files: bool,
    queue_h: u16,
    bottom_panel_h: u16,
    footer_h: u16,
) -> LayoutRegions {
    let content_width = (u32::from(area.width) * CONTENT_WIDTH_PERCENT / 100) as u16;
    let content_area = Rect {
        x: area.x + area.width.saturating_sub(content_width) / 2,
        y: area.y,
        width: content_width,
        height: area.height,
    };
    let fb = feedback_h.min(2);
    let input_h = input_h.clamp(3, THEME_DOCK_H);
    let qh = queue_h.min(8);
    let footer_h = footer_h.min(1);
    let fixed_h = 1 + fb + qh + input_h + footer_h;
    let requested_panel_h = bottom_panel_h.min(8);
    let panel_h = if content_area.height >= fixed_h + requested_panel_h + 10 {
        requested_panel_h
    } else {
        0
    };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),        // status
            Constraint::Min(3),           // main
            Constraint::Length(panel_h),  // bottom panel
            Constraint::Length(fb),       // feedback
            Constraint::Length(qh),       // message queue
            Constraint::Length(input_h),  // input (multi-line)
            Constraint::Length(footer_h), // contextual hint
        ])
        .split(content_area);

    let status = rows[0];
    let main = rows[1];
    let bottom_panel = rows[2];
    let feedback = rows[3];
    let queue = rows[4];
    let input = rows[5];
    let footer = rows[6];

    let show_files = show_files && content_area.width >= 110;
    let file_width = (content_area.width / 5).clamp(24, 32);
    let (files, chat) = if show_files {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(file_width), Constraint::Min(40)])
            .split(main);
        (Some(columns[0]), columns[1])
    } else {
        (None, main)
    };

    LayoutRegions {
        status,
        chat,
        files,
        bottom_panel,
        feedback,
        queue,
        input,
        footer,
    }
}

/// Estimate composer content width for wrapping before the layout split runs.
#[cfg(test)]
pub fn estimate_composer_content_width(area: Rect, show_files: bool) -> usize {
    let content_width = (u32::from(area.width) * CONTENT_WIDTH_PERCENT / 100) as u16;
    let show_files = show_files && content_width >= 110;
    let mut main_width = content_width;
    if show_files {
        let file_width = (content_width / 5).clamp(24, 32);
        main_width = main_width.saturating_sub(file_width);
    }
    main_width.saturating_sub(2).max(1) as usize
}

pub fn is_too_small(area: Rect) -> bool {
    area.width < 40 || area.height < MIN_HEIGHT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_composer_content_width_accounts_for_files_pane() {
        let area = Rect::new(0, 0, 140, 40);
        let wide = estimate_composer_content_width(area, true);
        let narrow = estimate_composer_content_width(area, false);
        assert!(wide < narrow);
        assert!(wide >= 1);
    }

    #[test]
    fn wide_layout_uses_95_percent_width_for_chat() {
        let area = Rect::new(0, 0, 120, 40);
        let r = split_areas(area);
        assert_eq!(r.status, Rect::new(3, 0, 114, 1));
        assert_eq!(r.chat, Rect::new(3, 1, 114, 36));
        assert_eq!(r.footer.height, 0);
        assert_eq!(r.input.height, 3);
        assert_eq!(r.bottom_panel.height, 0);
        assert_eq!(r.feedback.height, 0);
        assert_eq!(r.queue.height, 0);
    }

    #[test]
    fn bottom_panel_reserves_bounded_space() {
        let area = Rect::new(0, 0, 120, 40);
        let r = split_areas_with_bottom_panel(area, 0, 3, 0, 20);
        assert_eq!(r.bottom_panel.height, 8);
        assert_eq!(r.input.height, 3);
        assert_eq!(r.footer.height, 0);
    }

    #[test]
    fn bottom_panel_hides_when_height_is_tight() {
        let area = Rect::new(0, 0, 80, MIN_HEIGHT);
        let r = split_areas_with_bottom_panel(area, 0, 3, 0, 6);
        assert_eq!(r.bottom_panel.height, 0);
        assert_eq!(r.input.height, 3);
    }

    #[test]
    fn very_wide_layout_centers_the_95_percent_content_column() {
        let area = Rect::new(0, 0, 200, 40);
        let r = split_areas(area);
        assert_eq!(r.status, Rect::new(5, 0, 190, 1));
        assert_eq!(r.chat, Rect::new(5, 1, 190, 36));
        assert_eq!(r.input.x, 5);
        assert_eq!(r.input.width, 190);
        assert_eq!(r.footer.x, 5);
        assert_eq!(r.footer.width, 190);
        assert_eq!(r.footer.height, 0);
    }

    #[test]
    fn contextual_hint_row_is_explicit() {
        let area = Rect::new(0, 0, 120, 40);
        let r = split_areas_with_chrome(area, 0, 3, false, 0, 0, 1);
        assert_eq!(r.footer.height, 1);
        assert_eq!(r.footer.y + r.footer.height, area.height);
    }

    #[test]
    fn queue_strip_height_reserved() {
        let area = Rect::new(0, 0, 120, 40);
        let r = split_areas_full(area, 0, 3, 3);
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
    fn narrow_layout_keeps_full_chat_width() {
        let area = Rect::new(0, 0, 60, 24);
        let r = split_areas(area);
        assert_eq!(r.chat.width, 57);
        assert_eq!(r.status.width, 57);
    }

    #[test]
    fn files_panel_reserves_bounded_left_space() {
        let area = Rect::new(0, 0, 120, 40);
        let r = split_areas_with_side_panels(area, 0, 3, true, 0, 0);
        assert_eq!(r.files, Some(Rect::new(3, 1, 24, 36)));
        assert_eq!(r.chat, Rect::new(27, 1, 90, 36));
    }

    #[test]
    fn files_panel_hides_on_narrow_terminals() {
        let narrow = split_areas_with_side_panels(Rect::new(0, 0, 100, 30), 0, 3, true, 0, 0);
        assert!(narrow.files.is_none());
        assert_eq!(narrow.chat.width, 95);
    }

    #[test]
    fn min_size_guard() {
        assert!(is_too_small(Rect::new(0, 0, 30, 10)));
        assert!(!is_too_small(Rect::new(0, 0, 100, 30)));
    }
}
