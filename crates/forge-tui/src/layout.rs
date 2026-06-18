//! Full-screen layout splits (TUI-01 / tui-shell.md).

use ratatui::layout::{Constraint, Direction, Layout, Rect};

/// Minimum terminal size for a usable TUI.
pub const MIN_WIDTH: u16 = 80;
pub const MIN_HEIGHT: u16 = 18;
pub const SIDEBAR_WIDTH: u16 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutRegions {
    pub status: Rect,
    pub chat: Rect,
    pub sidebar: Option<Rect>,
    pub input: Rect,
    pub footer: Rect,
}

/// Split terminal into status / main / input / footer; main splits chat | sidebar when wide enough.
pub fn split_areas(area: Rect) -> LayoutRegions {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // status
            Constraint::Min(3),    // main
            Constraint::Length(3), // input
            Constraint::Length(1), // footer
        ])
        .split(area);

    let status = rows[0];
    let main = rows[1];
    let input = rows[2];
    let footer = rows[3];

    let (chat, sidebar) = if area.width >= MIN_WIDTH {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(40), Constraint::Length(SIDEBAR_WIDTH)])
            .split(main);
        (cols[0], Some(cols[1]))
    } else {
        (main, None)
    };

    LayoutRegions {
        status,
        chat,
        sidebar,
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
    fn wide_layout_has_sidebar() {
        let area = Rect::new(0, 0, 120, 40);
        let r = split_areas(area);
        assert!(r.sidebar.is_some());
        assert_eq!(r.status.height, 1);
        assert_eq!(r.footer.height, 1);
        assert_eq!(r.input.height, 3);
        let sb = r.sidebar.unwrap();
        assert_eq!(sb.width, SIDEBAR_WIDTH);
        assert_eq!(r.chat.width + sb.width, area.width);
        assert_eq!(r.status.y, 0);
        assert_eq!(r.footer.y + r.footer.height, area.height);
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
