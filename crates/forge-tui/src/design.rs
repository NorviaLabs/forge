//! 2026 design-system cell metrics (see
//! `artifacts/astra-tui-2026/03-forge-design-system.md`).
//!
//! Presentation layer only — never config. All values are terminal cells.
//!
//! # The spacing system
//!
//! Three nested steps, applied by role rather than per screen, so every pane,
//! chrome row and overlay shares one origin:
//!
//! 1. [`FRAME_INSET_X`] — the canvas gutter between the terminal edge and the
//!    work surface. Identical in every mode.
//! 2. [`PANE_PAD_X`] / [`PANE_GAP_X`] — a pane's own interior padding past its
//!    border column, and the gutter between adjacent panes.
//! 3. [`TEXT_INSET`](crate::widgets::input::TEXT_INSET) — the shared text origin
//!    inside a bordered pane's padding. Composer, feedback, queue and tree rows
//!    resolve to it; the borderless transcript uses [`PANE_PAD_X`] directly.
//!
//! # The border system
//!
//! Three levels, defined as theme accessors rather than literals:
//!
//! - **L1 — pane frame.** `theme::panel_border()`. Bordered panes, focused or not.
//!   Carries the layout's structure and never changes hue with focus.
//! - **L2 — inset field.** `theme::composer_surface()` for the borderless composer
//!   and explorer search field, so a nested field never reads as a louder box.
//! - **L3 — local accent.** `theme::active_panel_border()`. A focused search
//!   field's border, a pane's `>` title marker, a keyboard-bearing modal's title.
//!   Workspace tabs carry selection with a filled surface; inputs use a caret.
//!
//! A pane never takes L3: a bright rectangle around a whole panel is louder
//! than the content inside it, and it makes focus the layout's only readable
//! information.

/// Outer frame gutter, each side.
pub const FRAME_INSET_X: u16 = 1;
/// Internal pane padding, each side, in addition to the border column.
pub const PANE_PAD_X: u16 = 1;
/// Two-column gutter keeps resize grips clear of adjacent pane borders.
pub const PANE_GAP_X: u16 = 2;
/// Blank row between vertically stacked panes (left column ↔ bottom panel).
pub const PANE_GAP_Y: u16 = 1;
pub const CHROME_GAP_Y: u16 = 0;
/// Blank rows between the transcript and the composer.
pub const COMPOSER_GAP_Y: u16 = 1;
/// Leading inset for nested list rows (the explorer's tree).
///
/// One extra indent step past [`PANE_PAD_X`], so the disclosure column clears
/// the inset field above it (the search box keeps [`PANE_PAD_X`]) and the tree
/// reads as nested *inside* the pane rather than flush against its edge. The
/// per-level indent stays [`TREE_INDENT_W`].
pub const LIST_INSET_X: u16 = 2;
/// Blank rows between the explorer's search field and the first tree row.
pub const TREE_TOP_GAP_Y: u16 = 1;
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
/// The composer is a plain input surface with no reserved chrome row.
pub const COMPOSER_BORDER_H: u16 = 0;
/// Workspace view switcher: a tab-label row sitting on a baseline rule, so the
/// Agent/Files/Git tabs read as tabs and the active one opens into the pane.
pub const WORKSPACE_HEADER_H: u16 = 2;
/// Thickness of the horizontal session strip (session-name chips). Three rows
/// so chip labels sit on the centre row, centred both axes.
pub const SESSION_STRIP_H: u16 = 3;
/// Thickness of the navigator tab row (`Sessions` / `+` / `Files` / `Git`).
/// A terminal cell's height is fixed by the font, so a single text row cannot
/// be made taller without spilling into extra rows (which then can't centre the
/// label). Held at one row; labels sit centred across it.
pub const NAVIGATOR_TAB_H: u16 = 1;
/// Maximum visible draft rows; longer drafts retain their text and scroll.
pub const MAX_COMPOSER_INPUT_H: u16 = 4;
pub const COMPACT_COMPOSER_INPUT_H: u16 = 3;
pub const COMPACT_FRAME_H: u16 = 28;
pub const HOME_MAX_WIDTH: u16 = 84;
pub const NAVIGATOR_MIN_WIDTH: u16 = 32;
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
/// Frame-width eligibility for persistent columns; pane floors also apply.
pub const FILES_VISIBLE_FRAME_W: u16 = 116;
/// Minimum frame size.
pub const MIN_FRAME_W: u16 = 80;
pub const MIN_FRAME_H: u16 = 18;
/// Conversation pane rows below which the transcript falls back to compact
/// rhythm. Airy spacing (blank rows around headings and code) is
/// the default at comfortable heights; short terminals keep today's density so
/// the 80×18 workflow never loses content rows to padding.
pub const AIRY_MIN_ROWS: u16 = 24;
