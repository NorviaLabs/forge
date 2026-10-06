//! Full-screen layout splits (TUI-01 / tui-shell.md + Phase 10 feedback strip).

use ratatui::layout::{Constraint, Direction, Layout, Rect};

/// Minimum terminal size for a usable TUI.
pub const MIN_WIDTH: u16 = 80;
pub const MIN_HEIGHT: u16 = 18;
/// 2026 geometry: 1-column frame gutters replace the legacy 95% inset.
/// See `crate::design::FRAME_INSET_X` and `FILES_VISIBLE_FRAME_W`.
use crate::design::{
    CHROME_GAP_Y, COMPOSER_GAP_Y, FILES_VISIBLE_FRAME_W, FRAME_INSET_X, PANE_GAP_X, PANE_GAP_Y,
};
use forge_config::PaneLayoutPreferences;
/// Composer text rows (visual lines), capped for normal chat. The band adds
/// top and bottom border rows on top of this.
pub const MAX_COMPOSER_INPUT_H: u16 = 10;
/// Bottom theme picker dock: fits built-in themes without scrolling; scrolls for more.
pub const THEME_DOCK_H: u16 = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutRegions {
    pub status: Rect,
    /// Full-width approve-all warning strip, directly under `status`. 0-height
    /// when approve-all is off.
    pub approve_all_warning: Rect,
    /// Persistent task/session strip. Zero-height for legacy layout callers
    /// that have not opted into repository task mode.
    pub task_strip: Rect,
    /// Conversation/resource switcher above the work surface.
    pub workspace_tabs: Rect,
    /// Resource inspector: File, Diff or GitHub issues.
    pub chat: Rect,
    pub files: Option<Rect>,
    /// Primary conversation surface. On narrow terminals the inspector and
    /// navigator can temporarily occupy this surface without closing it.
    pub sidebar: Option<Rect>,
    /// Terminal under the conversation and inspector, beside the navigator.
    pub bottom_panel: Rect,
    /// Phase 10 / TUI-08 — 0-height when empty. A full-width chrome row
    /// directly above the footer: the status line belongs to the shell, not
    /// inside the conversation column, where it used to push the transcript
    /// up and down by a row every time a message appeared and expired.
    pub feedback: Rect,
    /// Outbound message queue across the work surface. 0-height when empty.
    pub queue: Rect,
    /// Background-task strip across the work surface, above the composer.
    pub background: Rect,
    /// Composer spans the work surface, including in inspection/navigation.
    pub input: Rect,
    pub footer: Rect,
}

/// Frame-column gate for persistent columns. Narrow navigation uses the
/// entire work surface temporarily instead of becoming unreachable.
const FILES_WIDTH_THRESHOLD: u16 = FILES_VISIBLE_FRAME_W;
/// Reserve useful content above the composer as optional strips grow.
const TRANSCRIPT_MIN_ROWS: u16 = 3;
/// Floors for simultaneous conversation and resource inspection.
const RESOURCE_MIN_WIDTH: u16 = 44;
const CONVERSATION_MIN_WIDTH: u16 = 60;

fn content_width(area: Rect) -> u16 {
    content_width_for(area.width)
}

fn content_width_for(frame_width: u16) -> u16 {
    frame_width.saturating_sub(FRAME_INSET_X * 2)
}

/// Whether the frame is eligible for persistent side-by-side columns.
/// Individual pane budgets can still require a temporary navigation view.
pub fn files_fit(frame_width: u16) -> bool {
    frame_width >= FILES_WIDTH_THRESHOLD
}

/// Keep geometry and composer measurement on the same horizontal budgets.
/// A requested navigator becomes the work surface when a persistent column
/// would squeeze either the conversation or the inspector below its floor.
fn workspace_columns(
    area: Rect,
    frame_width: u16,
    show_files: bool,
    resource_open: bool,
    navigator_active: bool,
    preferences: PaneLayoutPreferences,
) -> (Option<Rect>, Rect, bool) {
    let work_min = if resource_open {
        CONVERSATION_MIN_WIDTH + PANE_GAP_X + RESOURCE_MIN_WIDTH
    } else {
        CONVERSATION_MIN_WIDTH
    };
    let persistent =
        show_files && files_fit(frame_width) && area.width >= 28 + PANE_GAP_X + work_min;
    if persistent {
        let file_width = preferences
            .files_width_ratio
            .map(|ratio| (f64::from(area.width) * ratio).round() as u16)
            .unwrap_or((area.width / 5).clamp(28, 32))
            .clamp(28, area.width.saturating_sub(work_min + PANE_GAP_X));
        let columns = Layout::horizontal([
            Constraint::Length(file_width),
            Constraint::Length(PANE_GAP_X),
            Constraint::Min(work_min),
        ])
        .split(area);
        (Some(columns[0]), columns[2], false)
    } else if show_files && navigator_active {
        (Some(area), area, true)
    } else {
        (None, area, false)
    }
}

/// Split terminal. `feedback_h` is retained for source compatibility; no
/// feedback strip is reserved.
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
    show_sidebar: bool,
    background_h: u16,
    warning_h: u16,
) -> LayoutRegions {
    split_areas_with_preferences(
        area,
        feedback_h,
        input_h,
        show_files,
        queue_h,
        bottom_panel_h,
        footer_h,
        show_sidebar,
        background_h,
        warning_h,
        false,
        false,
        false,
        false,
        PaneLayoutPreferences::default(),
    )
}

#[allow(clippy::too_many_arguments)]
#[allow(dead_code)]
pub fn split_areas_with_expanded_conversation(
    area: Rect,
    feedback_h: u16,
    input_h: u16,
    show_files: bool,
    queue_h: u16,
    bottom_panel_h: u16,
    footer_h: u16,
    show_sidebar: bool,
    background_h: u16,
    warning_h: u16,
) -> LayoutRegions {
    split_areas_with_preferences(
        area,
        feedback_h,
        input_h,
        show_files,
        queue_h,
        bottom_panel_h,
        footer_h,
        show_sidebar,
        background_h,
        warning_h,
        true,
        false,
        false,
        false,
        PaneLayoutPreferences::default(),
    )
}

#[allow(clippy::too_many_arguments)]
pub fn split_areas_with_preferences(
    area: Rect,
    _feedback_h: u16,
    input_h: u16,
    show_files: bool,
    queue_h: u16,
    bottom_panel_h: u16,
    footer_h: u16,
    show_sidebar: bool,
    background_h: u16,
    warning_h: u16,
    expand_conversation: bool,
    show_task_strip: bool,
    resource_selected: bool,
    navigator_active: bool,
    preferences: PaneLayoutPreferences,
) -> LayoutRegions {
    let content_width = content_width(area);
    let content_area = Rect {
        x: area.x.saturating_add(FRAME_INSET_X),
        y: area.y,
        width: content_width,
        height: area.height,
    };
    let fb = 0;
    let input_h = input_h.clamp(3, THEME_DOCK_H);
    let qh = queue_h.min(8);
    let bg_h = background_h.min(8);
    let footer_h = footer_h.min(2);
    let resource_open = !expand_conversation;
    let gap_bottom = if footer_h > 0 { CHROME_GAP_Y } else { 0 };
    let status_h = 1;
    let fixed_h = status_h + footer_h + fb + CHROME_GAP_Y + gap_bottom;
    let requested_panel_h = if bottom_panel_h > 0 {
        preferences
            .bottom_panel_height_ratio
            .map(|ratio| (f64::from(content_area.height) * ratio).round() as u16)
            .unwrap_or(bottom_panel_h)
            .min(32)
    } else {
        0
    };
    let available_panel_h = content_area
        .height
        .saturating_sub(fixed_h)
        .saturating_sub(warning_h.min(1) + u16::from(show_task_strip))
        .saturating_sub(PANE_GAP_Y)
        .saturating_sub(1 + TRANSCRIPT_MIN_ROWS + input_h + qh + COMPOSER_GAP_Y);
    let panel_h = if requested_panel_h > 0 {
        requested_panel_h
            .clamp(3, available_panel_h.max(3))
            .min(available_panel_h)
    } else {
        0
    };

    // Shell chrome surrounds the work surface. Queue, background and input
    // share the work surface width, split below the conversation/inspector.
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(status_h),               // status
            Constraint::Length(warning_h.min(1)),       // approve-all warning
            Constraint::Length(show_task_strip as u16), // task strip
            Constraint::Length(CHROME_GAP_Y),           // gutter under chrome
            Constraint::Min(3),                         // main
            Constraint::Length(gap_bottom),             // gutter above the shell band
            Constraint::Length(fb),                     // status line
            Constraint::Length(footer_h),               // contextual hint
        ])
        .split(content_area);
    let status = rows[0];
    let approve_all_warning = rows[1];
    let task_strip = rows[2];
    let main = rows[4];
    let feedback = rows[6];
    let footer = rows[7];

    let (mut files, work, navigator_overlay) = workspace_columns(
        main,
        area.width,
        show_files,
        resource_open,
        navigator_active,
        preferences,
    );
    let tabs_h = u16::from(!navigator_overlay);
    let input_h = input_h.min(
        work.height
            .saturating_sub(
                tabs_h + qh + panel_h + u16::from(panel_h > 0) * PANE_GAP_Y + TRANSCRIPT_MIN_ROWS,
            )
            .max(3),
    );
    let bg_h = bg_h.min(
        work.height
            .saturating_sub(tabs_h + qh + input_h + panel_h + PANE_GAP_Y + TRANSCRIPT_MIN_ROWS),
    );
    let work_rows = Layout::vertical([
        Constraint::Length(tabs_h),
        Constraint::Min(TRANSCRIPT_MIN_ROWS),
        Constraint::Length(if panel_h > 0 { PANE_GAP_Y } else { 0 }),
        Constraint::Length(panel_h),
        Constraint::Length(qh),
        Constraint::Length(bg_h),
        Constraint::Length(COMPOSER_GAP_Y),
        Constraint::Length(input_h),
    ])
    .split(work);
    let workspace_tabs = work_rows[0];
    let body = work_rows[1];
    let bottom_panel = work_rows[3];
    let queue = work_rows[4];
    let background = work_rows[5];
    let input = work_rows[7];
    let zero = Rect::new(body.x, body.y, 0, 0);
    let (sidebar, chat) = if navigator_overlay {
        files = Some(body);
        (None, zero)
    } else if resource_open
        && files_fit(area.width)
        && body.width >= CONVERSATION_MIN_WIDTH + PANE_GAP_X + RESOURCE_MIN_WIDTH
        && show_sidebar
    {
        let conversation_width = preferences
            .conversation_width_ratio
            .map(|ratio| (f64::from(content_area.width) * ratio).round() as u16)
            .unwrap_or(body.width * 3 / 5)
            .clamp(
                CONVERSATION_MIN_WIDTH,
                body.width - PANE_GAP_X - RESOURCE_MIN_WIDTH,
            );
        let columns = Layout::horizontal([
            Constraint::Length(conversation_width),
            Constraint::Length(PANE_GAP_X),
            Constraint::Min(RESOURCE_MIN_WIDTH),
        ])
        .split(body);
        (Some(columns[0]), columns[2])
    } else if resource_open && resource_selected {
        (None, body)
    } else {
        (show_sidebar.then_some(body), zero)
    };

    LayoutRegions {
        status,
        approve_all_warning,
        task_strip,
        workspace_tabs,
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

/// Estimate the composer width before the layout split runs.
#[allow(dead_code)]
pub fn estimate_composer_content_width(area: Rect) -> usize {
    estimate_composer_region_width(area, false, true, false, PaneLayoutPreferences::default())
}

/// Estimate the width of the region containing the composer before vertical
/// layout runs. This mirrors the horizontal split used by the renderer.
pub fn estimate_composer_region_width(
    area: Rect,
    show_files: bool,
    expanded_conversation: bool,
    navigator_active: bool,
    preferences: PaneLayoutPreferences,
) -> usize {
    let (_, work, _) = workspace_columns(
        Rect::new(area.x, area.y, content_width(area), area.height),
        area.width,
        show_files,
        !expanded_conversation,
        navigator_active,
        preferences,
    );
    work.width.max(1) as usize
}

/// Whether the frame is below the size the layout is built for.
///
/// This used to guard on a hardcoded 40 columns while every splitter below it
/// assumes [`MIN_WIDTH`]. Between the two, `split_areas` handed back regions of
/// zero height: at 60x20 Forge drew a header, a rule and a footer, and nothing
/// to type into. Honouring the layout's own stated minimum turns that into a
/// message that says what to do.
pub fn is_too_small(area: Rect) -> bool {
    area.width < MIN_WIDTH || area.height < MIN_HEIGHT
}
