//! Geometry shared by modal overlays and theme previews.

use ratatui::layout::{Constraint, Direction, Layout, Rect};

pub fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup[1])[1]
}

pub fn centered_content_rect(area: Rect, max_width: u16, content: u16, max_height: u16) -> Rect {
    centered_capped_rect(area, max_width, content.min(max_height).max(3))
}

pub(crate) fn centered_capped_rect(area: Rect, max_width: u16, max_height: u16) -> Rect {
    let width = area.width.saturating_sub(4).min(max_width).max(1);
    let height = area.height.saturating_sub(4).min(max_height).max(1);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

pub fn theme_preview_card(area: Rect) -> Rect {
    if area.width < 24 || area.height < 8 {
        return Rect::new(area.x, area.y, 0, 0);
    }
    let reserved = 28.min(area.width.saturating_sub(24));
    let width = area
        .width
        .saturating_sub(reserved)
        .clamp(24, 58)
        .min(area.width);
    let gap = 1;
    let height = area.height.saturating_sub(gap).min(16);
    Rect {
        x: area
            .x
            .saturating_add(area.width.saturating_sub(width + gap)),
        y: area
            .y
            .saturating_add(area.height.saturating_sub(height + gap)),
        width,
        height,
    }
}

pub fn theme_preview_rect(area: Rect) -> Rect {
    if area.width < 24 || area.height < 8 {
        return Rect::new(area.x, area.y, 0, 0);
    }
    let dock = theme_dock_rect(area);
    let gap = 1;
    let available = dock.y.saturating_sub(area.y).saturating_sub(gap);
    let height = available.clamp(8, 16);
    let max_w = area.width.saturating_sub(2);
    let width = (area.width.saturating_mul(58) / 100).max(24).min(max_w);
    let x = area
        .x
        .saturating_add(area.width.saturating_sub(width).saturating_sub(1));
    let y = dock
        .y
        .saturating_sub(height)
        .saturating_sub(gap)
        .max(area.y);
    Rect {
        x,
        y,
        width,
        height: height.min(dock.y.saturating_sub(y)).min(area.height),
    }
}

pub(crate) fn theme_dock_rect(area: Rect) -> Rect {
    let height = crate::layout::THEME_DOCK_H
        .min(area.height.saturating_sub(1))
        .max(3);
    Rect::new(
        area.x,
        area.y + area.height.saturating_sub(height),
        area.width,
        height,
    )
}
