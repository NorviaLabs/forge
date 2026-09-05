//! 2026 design-system cell metrics (see
//! `artifacts/astra-tui-2026/03-forge-design-system.md`).
//!
//! Presentation layer only — never config. All values are terminal cells.

/// Outer frame gutter, each side.
pub const FRAME_INSET_X: u16 = 1;
/// Internal pane padding, each side.
pub const PANE_PAD_X: u16 = 1;
/// Pane title height.
pub const PANE_TITLE_H: u16 = 1;
/// Shared vertical separator width.
pub const PANE_SEPARATOR_W: u16 = 1;
/// Max transcript prose width; code/diff/terminal exempt.
pub const TRANSCRIPT_MAX_W: u16 = 88;
/// Blank rows between completed turns.
pub const TURN_GAP_H: u16 = 1;
/// Rows after a user request.
pub const REQUEST_GAP_H: u16 = 1;
/// Rows between related tools (0); 1 across narrative/approval boundaries.
pub const ACTIVITY_GAP_H: u16 = 0;
/// Plan items have no gap; continuation aligns after 4-col state prefix.
pub const PLAN_ITEM_GAP_H: u16 = 0;
/// Completed-item metadata indent (expanded detail only).
pub const PLAN_META_INDENT: u16 = 4;
/// Composer horizontal padding; same text origin as chat.
pub const COMPOSER_PAD_X: u16 = 1;
/// Composer top rule height; no side/bottom border.
pub const COMPOSER_RULE_H: u16 = 1;
/// Composer vertical padding rows.
pub const COMPOSER_PAD_Y: u16 = 0;
/// Max composer input rows.
pub const MAX_COMPOSER_INPUT_H: u16 = 10;
/// Footer height; no separate separator row.
pub const FOOTER_H: u16 = 1;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_match_design_system() {
        assert_eq!(FRAME_INSET_X, 1);
        assert_eq!(PANE_PAD_X, 1);
        assert_eq!(PANE_TITLE_H, 1);
        assert_eq!(PANE_SEPARATOR_W, 1);
        assert_eq!(TRANSCRIPT_MAX_W, 88);
        assert_eq!(TURN_GAP_H, 1);
        assert_eq!(MAX_COMPOSER_INPUT_H, 10);
        assert_eq!(FOOTER_H, 1);
        assert_eq!(MODAL_PAD_X, 2);
        assert_eq!(TREE_INDENT_W, 2);
        assert_eq!(FILES_VISIBLE_FRAME_W, 116);
    }
}
