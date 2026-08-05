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
    /// Center pane: File/Diff/Run content, or an empty-state placeholder.
    pub chat: Rect,
    pub files: Option<Rect>,
    /// Persistent conversation sidebar. Unlike `files`, this doesn't hide at
    /// narrow widths — the composer lives inside it, so it always gets a
    /// column (see `SIDEBAR_MIN_CONTENT_WIDTH`).
    pub sidebar: Option<Rect>,
    /// Contextual bottom panel, docked under `files`+`chat` only — never
    /// under `sidebar`. 0-height when closed or space is tight.
    pub bottom_panel: Rect,
    /// Phase 10 / TUI-08 — 0-height when empty. Scoped to `sidebar`'s width.
    pub feedback: Rect,
    /// Outbound message queue. 0-height when empty.
    /// Scoped to `sidebar`'s width.
    pub queue: Rect,
    /// Background-task strip, docked above the composer. Scoped to
    /// `sidebar`'s width. 0-height when the sidebar itself is hidden.
    pub background: Rect,
    /// Composer. Scoped to `sidebar`'s width, docked at its bottom.
    pub input: Rect,
    pub footer: Rect,
}

/// Width threshold below which the file explorer hides. Higher than
/// `SIDEBAR_MIN_CONTENT_WIDTH` so the explorer is the first thing to go as
/// the terminal narrows.
const FILES_WIDTH_THRESHOLD: u16 = 110;
/// Minimum chat/files width the sidebar must leave behind. The sidebar
/// itself doesn't hide on narrow-width precedence like `files` does — the
/// composer lives inside it — so this is only a defensive floor against
/// negative-width arithmetic on pathologically narrow terminals.
const SIDEBAR_MIN_CONTENT_WIDTH: u16 = 40;

fn content_width(area: Rect) -> u16 {
    (u32::from(area.width) * CONTENT_WIDTH_PERCENT / 100) as u16
}

fn sidebar_width(content_width: u16) -> u16 {
    if content_width >= 160 {
        (content_width / 2).clamp(64, 88)
    } else {
        (content_width / 4).clamp(32, 44)
    }
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
    split_areas_with_chrome(
        area,
        feedback_h,
        input_h,
        false,
        queue_h,
        bottom_panel_h,
        0,
        true,
        0,
    )
}

#[allow(dead_code, clippy::too_many_arguments)]
pub fn split_areas_with_side_panels(
    area: Rect,
    feedback_h: u16,
    input_h: u16,
    show_files: bool,
    queue_h: u16,
    bottom_panel_h: u16,
    show_sidebar: bool,
    background_h: u16,
) -> LayoutRegions {
    split_areas_with_chrome(
        area,
        feedback_h,
        input_h,
        show_files,
        queue_h,
        bottom_panel_h,
        0,
        show_sidebar,
        background_h,
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
    show_sidebar: bool,
    background_h: u16,
) -> LayoutRegions {
    let content_width = content_width(area);
    let content_area = Rect {
        x: area.x + area.width.saturating_sub(content_width) / 2,
        y: area.y,
        width: content_width,
        height: area.height,
    };
    let fb = feedback_h.min(2);
    let input_h = input_h.clamp(3, THEME_DOCK_H);
    let qh = queue_h.min(8);
    let bg_h = background_h.min(8);
    let footer_h = footer_h.min(1);
    let sidebar_width = sidebar_width(content_area.width);
    let show_sidebar =
        show_sidebar && content_area.width >= sidebar_width + SIDEBAR_MIN_CONTENT_WIDTH;
    let fixed_h = 1 + footer_h;
    let requested_panel_h = bottom_panel_h.min(32);
    let available_panel_h = content_area
        .height
        .saturating_sub(fixed_h)
        .saturating_sub(3);
    let panel_h = requested_panel_h.min(available_panel_h);

    // Top-level vertical stack: status / main / footer. `feedback`, `queue`
    // and `input` no longer live here — they're scoped to the sidebar's own
    // width, split out below.
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),        // status
            Constraint::Min(3),           // main
            Constraint::Length(footer_h), // contextual hint
        ])
        .split(content_area);
    let status = rows[0];
    let main = rows[1];
    let footer = rows[2];

    // main row: [left column (files+chat+bottom_panel), sidebar]
    let (left_area, sidebar) = if show_sidebar {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(40), Constraint::Length(sidebar_width)])
            .split(main);
        (columns[0], Some(columns[1]))
    } else {
        (main, None)
    };

    // left column: [files+chat, bottom_panel] — bottom panel spans this
    // column's full width, never the sidebar's.
    let left_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(panel_h)])
        .split(left_area);
    let top = left_rows[0];
    let bottom_panel = left_rows[1];

    let show_files =
        show_files && content_area.width >= FILES_WIDTH_THRESHOLD && top.width >= 24 + 40;
    let file_width = (content_area.width / 5).clamp(24, 32);
    let (files, chat) = if show_files {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(file_width), Constraint::Min(40)])
            .split(top);
        (Some(columns[0]), columns[1])
    } else {
        (None, top)
    };

    // sidebar: [transcript, feedback, queue, background, input] — always
    // shows a composer when the sidebar itself is shown.
    let (sidebar, feedback, queue, background, input) = if let Some(sb) = sidebar {
        let sidebar_rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),
                Constraint::Length(fb),
                Constraint::Length(qh),
                Constraint::Length(bg_h),
                Constraint::Length(input_h),
            ])
            .split(sb);
        (
            Some(sidebar_rows[0]),
            sidebar_rows[1],
            sidebar_rows[2],
            sidebar_rows[3],
            sidebar_rows[4],
        )
    } else {
        let zero = Rect::new(main.x, main.y, 0, 0);
        (None, zero, zero, zero, zero)
    };

    LayoutRegions {
        status,
        chat,
        files,
        sidebar,
        bottom_panel,
        feedback,
        queue,
        background,
        input,
        footer,
    }
}

/// Estimate the sidebar composer width before the layout split runs.
pub fn estimate_composer_content_width(area: Rect) -> usize {
    sidebar_width(content_width(area)).max(1) as usize
}

pub fn is_too_small(area: Rect) -> bool {
    area.width < 40 || area.height < MIN_HEIGHT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_composer_content_width_matches_sidebar_width() {
        let area = Rect::new(0, 0, 140, 40);
        assert_eq!(estimate_composer_content_width(area), 33);
        assert_eq!(
            estimate_composer_content_width(Rect::new(0, 0, 200, 40)),
            88
        );
    }

    #[test]
    fn wide_layout_uses_95_percent_width_for_chat() {
        let area = Rect::new(0, 0, 120, 40);
        let r = split_areas(area);
        assert_eq!(r.status, Rect::new(3, 0, 114, 1));
        // Chat gets the width left of the sidebar; taller than before since
        // the composer no longer eats vertical space from this column — it
        // lives in the sidebar's own split instead.
        assert_eq!(r.chat, Rect::new(3, 1, 82, 39));
        assert_eq!(r.sidebar, Some(Rect::new(85, 1, 32, 36)));
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
        assert_eq!(r.bottom_panel.height, 20);
        assert_eq!(r.input.height, 3);
        assert_eq!(r.footer.height, 0);
    }

    #[test]
    fn bottom_panel_caps_at_32_rows() {
        let area = Rect::new(0, 0, 120, 50);
        let r = split_areas_with_bottom_panel(area, 0, 3, 0, 40);
        assert_eq!(r.bottom_panel.height, 32);
        assert_eq!(r.chat.height, 17);
    }

    #[test]
    fn bottom_panel_hides_when_height_is_tight() {
        let area = Rect::new(0, 0, 80, MIN_HEIGHT);
        let r = split_areas_with_bottom_panel(area, 0, 3, 0, 32);
        assert_eq!(r.bottom_panel.height, 14);
        assert_eq!(r.input.height, 3);
    }

    #[test]
    fn very_wide_layout_centers_the_95_percent_content_column() {
        let area = Rect::new(0, 0, 200, 40);
        let r = split_areas(area);
        assert_eq!(r.status, Rect::new(5, 0, 190, 1));
        assert_eq!(r.chat, Rect::new(5, 1, 102, 39));
        // The composer is scoped to the sidebar's width now, not the full
        // content column.
        assert_eq!(r.input.x, 107);
        assert_eq!(r.input.width, 88);
        assert_eq!(r.footer.x, 5);
        assert_eq!(r.footer.width, 190);
        assert_eq!(r.footer.height, 0);
    }

    #[test]
    fn contextual_hint_row_is_explicit() {
        let area = Rect::new(0, 0, 120, 40);
        let r = split_areas_with_chrome(area, 0, 3, false, 0, 0, 1, true, 0);
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
        let area = Rect::new(0, 0, 120, 30);
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
        let r = split_areas_with_side_panels(area, 0, 3, true, 0, 0, true, 0);
        assert_eq!(r.files, Some(Rect::new(3, 1, 24, 39)));
        assert_eq!(r.chat, Rect::new(27, 1, 58, 39));
        assert_eq!(r.sidebar, Some(Rect::new(85, 1, 32, 36)));
    }

    #[test]
    fn files_panel_hides_on_narrow_terminals() {
        let narrow =
            split_areas_with_side_panels(Rect::new(0, 0, 100, 30), 0, 3, true, 0, 0, true, 0);
        assert!(narrow.files.is_none());
        assert_eq!(narrow.chat.width, 63);
    }

    #[test]
    fn sidebar_never_hides_due_to_narrow_terminal_precedence_only_a_defensive_floor() {
        // Below the defensive floor (sidebar_width + SIDEBAR_MIN_CONTENT_WIDTH),
        // the sidebar collapses to avoid negative-width arithmetic — but this
        // is a safety guard, not the same auto-hide precedence `files` has.
        let narrow =
            split_areas_with_side_panels(Rect::new(0, 0, 60, 24), 0, 3, false, 0, 0, true, 0);
        assert!(narrow.sidebar.is_none());

        let comfortable =
            split_areas_with_side_panels(Rect::new(0, 0, 120, 40), 0, 3, false, 0, 0, true, 0);
        assert!(comfortable.sidebar.is_some());
    }

    #[test]
    fn min_size_guard() {
        assert!(is_too_small(Rect::new(0, 0, 30, 10)));
        assert!(!is_too_small(Rect::new(0, 0, 100, 30)));
    }
}
