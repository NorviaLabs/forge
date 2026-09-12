//! 2026 design-system cell metrics (see
//! `artifacts/astra-tui-2026/03-forge-design-system.md`).
//!
//! Presentation layer only — never config. All values are terminal cells.

/// Outer frame gutter, each side.
pub const FRAME_INSET_X: u16 = 1;
/// Internal pane padding, each side, in addition to the border column.
pub const PANE_PAD_X: u16 = 1;
/// Blank column between adjacent columns (Files | Workspace | Sidebar).
pub const PANE_GAP_X: u16 = 2;
/// Blank row between vertically stacked panes (left column ↔ bottom panel).
pub const PANE_GAP_Y: u16 = 1;
/// Blank row between the content band and the chrome rows (StatusBar /
/// Footer), so the chrome reads as separate from the work surface.
pub const CHROME_GAP_Y: u16 = 1;
/// Blank row between the transcript and the composer.
pub const COMPOSER_GAP_Y: u16 = 1;
/// Pane title height.
pub const PANE_TITLE_H: u16 = 1;
/// Shared vertical separator width.
pub const PANE_SEPARATOR_W: u16 = 1;
/// Blank rows between related tools (0); 1 across narrative/approval
/// boundaries. The conversation's airy `gap` flag is the rhythm owner, so this
/// metric is kept for callers that need the compact value.
pub const ACTIVITY_GAP_H: u16 = 0;
/// Plan items have no gap; continuation aligns after 4-col state prefix.
pub const PLAN_ITEM_GAP_H: u16 = 0;
/// Completed-item metadata indent (expanded detail only).
pub const PLAN_META_INDENT: u16 = 4;
/// Composer horizontal padding; same text origin as chat.
pub const COMPOSER_PAD_X: u16 = 2;
/// Composer border rows, above and below the input.
pub const COMPOSER_BORDER_H: u16 = 2;
/// Max composer input rows.
pub const MAX_COMPOSER_INPUT_H: u16 = 10;
/// Footer height; no separate separator row.
pub const FOOTER_H: u16 = 2;
/// Modal inner horizontal padding (inside 1-col border).
pub const MODAL_PAD_X: u16 = 2;
/// Modal inner vertical padding (omitted at height < 24).
pub const MODAL_PAD_Y: u16 = 1;
/// Modal section gap.
pub const MODAL_SECTION_GAP_H: u16 = 1;
/// Tree indent per nesting level.
pub const TREE_INDENT_W: u16 = 2;
/// Theme bottom dock max height.
pub const THEME_DOCK_H: u16 = 12;
/// Files visibility gate: frame columns (preserves effective 116-col contract
/// when removing the 95% inset).
pub const FILES_VISIBLE_FRAME_W: u16 = 116;
/// Minimum frame size.
pub const MIN_FRAME_W: u16 = 80;
pub const MIN_FRAME_H: u16 = 18;
/// Conversation pane rows below which the transcript falls back to compact
/// rhythm. Airy spacing (blank rows around headings and code) is
/// the default at comfortable heights; short terminals keep today's density so
/// the 80×18 workflow never loses content rows to padding.
pub const AIRY_MIN_ROWS: u16 = 24;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_match_design_system() {
        assert_eq!(FRAME_INSET_X, 1);
        assert_eq!(PANE_PAD_X, 1);
        assert_eq!(PANE_GAP_X, 2);
        assert_eq!(PANE_GAP_Y, 1);
        assert_eq!(CHROME_GAP_Y, 1);
        assert_eq!(COMPOSER_GAP_Y, 1);
        assert_eq!(PANE_TITLE_H, 1);
        assert_eq!(PANE_SEPARATOR_W, 1);
        assert_eq!(MAX_COMPOSER_INPUT_H, 10);
        assert_eq!(FOOTER_H, 2);
        assert_eq!(MODAL_PAD_X, 2);
        assert_eq!(TREE_INDENT_W, 2);
        assert_eq!(FILES_VISIBLE_FRAME_W, 116);
        assert_eq!(AIRY_MIN_ROWS, 24);
    }
}
