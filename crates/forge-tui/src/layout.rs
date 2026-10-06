//! Full-screen layout splits (TUI-01 / tui-shell.md + Phase 10 feedback strip).

use ratatui::layout::{Constraint, Direction, Layout, Rect};

/// Minimum terminal size for a usable TUI.
pub const MIN_WIDTH: u16 = 80;
pub const MIN_HEIGHT: u16 = 18;
/// 2026 geometry: 1-column frame gutters replace the legacy 95% inset.
/// See `crate::design::FRAME_INSET_X` and `FILES_VISIBLE_FRAME_W`.
use crate::design::{
    AIRY_MIN_ROWS, CHROME_GAP_Y, COMPOSER_GAP_Y, FILES_VISIBLE_FRAME_W, FRAME_INSET_X, PANE_GAP_X,
    PANE_GAP_Y,
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
    /// Phase 10 / TUI-08 — 0-height when empty. A full-width chrome row
    /// directly above the footer: the status line belongs to the shell, not
    /// inside the conversation column, where it used to push the transcript
    /// up and down by a row every time a message appeared and expired.
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

/// Frame-column gate for Files visibility (2026 contract: 116).
/// Higher than `SIDEBAR_MIN_CONTENT_WIDTH` so the explorer is the first
/// thing to go as the terminal narrows. Previously a 110 content-column
/// threshold applied after the 95% inset (effective 116 frame columns);
/// the inset is gone but the frame gate is preserved byte-for-byte.
const FILES_WIDTH_THRESHOLD: u16 = FILES_VISIBLE_FRAME_W;
/// The transcript's floor. It is the only `Min` in the sidebar's stack, so
/// this is what every other strip there is measured against.
const TRANSCRIPT_MIN_ROWS: u16 = 3;
/// Minimum chat/files width the sidebar must leave behind. The sidebar
/// itself doesn't hide on narrow-width precedence like `files` does — the
/// composer lives inside it — so this is only a defensive floor against
/// negative-width arithmetic on pathologically narrow terminals.
const SIDEBAR_MIN_CONTENT_WIDTH: u16 = 44;

fn content_width(area: Rect) -> u16 {
    content_width_for(area.width)
}

fn content_width_for(frame_width: u16) -> u16 {
    frame_width.saturating_sub(FRAME_INSET_X * 2)
}

/// Whether the file explorer can be rendered at this terminal width.
///
/// Frame-column gate (116). Shares [`FILES_WIDTH_THRESHOLD`] with
/// `split_areas_ex` so a caller asking "will this show?" can never disagree
/// with what the layout actually does.
pub fn files_fit(frame_width: u16) -> bool {
    frame_width >= FILES_WIDTH_THRESHOLD
}

/// Narrowest terminal that can show the explorer, for user-facing messages.
pub fn files_min_frame_width() -> u16 {
    FILES_WIDTH_THRESHOLD
}

fn sidebar_width(content_width: u16) -> u16 {
    if content_width >= 160 {
        (content_width / 2).clamp(64, 88)
    } else {
        (content_width / 4).clamp(32, 44)
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
    let default_sidebar_width = sidebar_width(content_area.width);
    let sidebar_max = if show_files && area.width >= FILES_WIDTH_THRESHOLD {
        content_area
            .width
            .saturating_sub(28 + PANE_GAP_X + 44 + PANE_GAP_X)
    } else {
        content_area
            .width
            .saturating_sub(SIDEBAR_MIN_CONTENT_WIDTH + PANE_GAP_X)
    };
    let sidebar_width = preferences
        .conversation_width_ratio
        .map(|ratio| (f64::from(content_area.width) * ratio).round() as u16)
        .unwrap_or(default_sidebar_width)
        .clamp(32, sidebar_max.max(32));
    let show_sidebar =
        show_sidebar && content_area.width >= sidebar_width + SIDEBAR_MIN_CONTENT_WIDTH;
    let gap_bottom = if footer_h > 0 { CHROME_GAP_Y } else { 0 };
    let status_h = if area.height >= AIRY_MIN_ROWS { 3 } else { 1 };
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
        .saturating_sub(if expand_conversation {
            TRANSCRIPT_MIN_ROWS + input_h + qh + COMPOSER_GAP_Y
        } else {
            3
        });
    let panel_h = if requested_panel_h > 0 {
        requested_panel_h
            .clamp(3, available_panel_h.max(3))
            .min(available_panel_h)
    } else {
        0
    };

    // Top-level vertical stack: status / approve-all warning / task strip /
    // gutter / main / gutter / status line / footer. `queue`, `background`
    // and `input` do not live here — they're scoped to the sidebar's own
    // width, split below.
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

    // main row: [left column (files+chat+bottom_panel), gutter, sidebar]
    let (left_area, sidebar) = if show_sidebar && !expand_conversation {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Min(44),
                Constraint::Length(PANE_GAP_X),
                Constraint::Length(sidebar_width),
            ])
            .split(main);
        (columns[0], Some(columns[2]))
    } else {
        (main, None)
    };

    // left column: [files+chat, gutter, bottom_panel] — bottom panel spans
    // this column's full width, never the sidebar's.
    let left_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(if panel_h > 0 {
            [
                Constraint::Min(3),
                Constraint::Length(PANE_GAP_Y),
                Constraint::Length(panel_h),
            ]
        } else {
            [
                Constraint::Min(3),
                Constraint::Length(0),
                Constraint::Length(0),
            ]
        })
        .split(left_area);
    let top = left_rows[0];
    let bottom_panel = left_rows[2];

    let show_files =
        show_files && area.width >= FILES_WIDTH_THRESHOLD && top.width >= 28 + PANE_GAP_X + 44;
    let default_file_width = (content_area.width / 4).clamp(28, 37);
    let file_width = preferences
        .files_width_ratio
        .map(|ratio| (f64::from(content_area.width) * ratio).round() as u16)
        .unwrap_or(default_file_width)
        .clamp(28, top.width.saturating_sub(44 + PANE_GAP_X).max(28));
    let (files, chat) = if show_files {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(file_width),
                Constraint::Length(PANE_GAP_X),
                Constraint::Min(44),
            ])
            .split(top);
        (Some(columns[0]), columns[2])
    } else {
        (None, top)
    };

    let (files, chat, sidebar) = if expand_conversation && show_sidebar {
        let (files, conversation) = if show_files {
            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Length(file_width),
                    Constraint::Length(PANE_GAP_X),
                    Constraint::Min(44),
                ])
                .split(top);
            (Some(columns[0]), columns[2])
        } else {
            (None, top)
        };
        (
            files,
            Rect::new(conversation.x, conversation.y, 0, 0),
            Some(conversation),
        )
    } else {
        (files, chat, sidebar)
    };

    // sidebar: [transcript, queue, background, gutter, input]. The status line
    // used to be a row in here; it now sits in the shell band above the footer,
    // so a status message no longer takes a row from the conversation.
    let (sidebar, queue, background, input) = if let Some(sb) = sidebar {
        // The background strip yields before the transcript does. It is the
        // only strip here whose height the caller asks for rather than derives,
        // so it is the one that has to be clamped against the transcript's
        // `Min(3)` floor — otherwise the constraint solver would honour the
        // strip first and shrink the conversation instead.
        let bg_h = bg_h.min(
            sb.height
                .saturating_sub(qh + COMPOSER_GAP_Y + input_h)
                .saturating_sub(TRANSCRIPT_MIN_ROWS),
        );
        let sidebar_rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(TRANSCRIPT_MIN_ROWS),
                Constraint::Length(qh),
                Constraint::Length(bg_h),
                Constraint::Length(COMPOSER_GAP_Y), // gutter above the composer
                Constraint::Length(input_h),
            ])
            .split(sb);
        (
            Some(sidebar_rows[0]),
            sidebar_rows[1],
            sidebar_rows[2],
            sidebar_rows[4],
        )
    } else {
        let zero = Rect::new(main.x, main.y, 0, 0);
        (None, zero, zero, zero)
    };

    LayoutRegions {
        status,
        approve_all_warning,
        task_strip,
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
#[allow(dead_code)]
pub fn estimate_composer_content_width(area: Rect) -> usize {
    estimate_composer_region_width(area, false, false)
}

/// Estimate the width of the region containing the composer before vertical
/// layout runs. This mirrors the horizontal split used by the renderer.
pub fn estimate_composer_region_width(
    area: Rect,
    show_files: bool,
    expanded_conversation: bool,
) -> usize {
    let width = content_width(area);
    if expanded_conversation {
        let file_width = (width / 4).clamp(28, 37);
        if show_files && area.width >= FILES_WIDTH_THRESHOLD && width >= file_width + 44 {
            width
                .saturating_sub(file_width)
                .saturating_sub(PANE_GAP_X)
                .max(1) as usize
        } else {
            width.max(1) as usize
        }
    } else {
        sidebar_width(width).max(1) as usize
    }
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
