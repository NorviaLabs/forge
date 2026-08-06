//! Mouse text selection and the right-click context menu.
//!
//! v1 wire-up: the **Editor** (SourceViewer) pane. Selection is stored in
//! terminal-screen coordinates (`Cell { row, col }`), which is the natural
//! space crossterm delivers and the renderer paints. Each pane that wants copy
//! exposes a helper that maps a screen selection rect back to pane text; the
//! Editor one lives here. Panes later (conversation/diff/terminal) will provide
//! their own mapping against the same `MouseSelection` state, so the selection
//! engine itself is pane-agnostic.

use ratatui::layout::Rect;

/// A terminal cell in crossterm's 0-based screen coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Cell {
    pub row: u16,
    pub col: u16,
}

/// A normalized selection span, in reading order (top-to-bottom,
/// left-to-right within a row) rather than an independently-normalized
/// rectangle. `start_col`/`end_col` are paired with `row_start`/`row_end`
/// specifically — the column of whichever endpoint (anchor or current) came
/// first in reading order, and the column of whichever came last. This
/// matters whenever a drag isn't purely down-right or up-left: e.g.
/// dragging from (row 0, col 5) down to (row 2, col 3) is a down-left drag,
/// and naively min/maxing rows and columns independently would swap which
/// column belongs to the first vs. last row, corrupting the selection
/// shape. Using reading order keeps `start_col` as row 0's boundary and
/// `end_col` as row 2's, matching what every other terminal app selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SelectionRect {
    pub row_start: u16,
    pub row_end: u16,
    pub start_col: u16,
    pub end_col: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyPane {
    Conversation,
    Editor,
    Diff,
    Terminal,
}

/// State machine for a drag-selection. Screen-anchored; the text is finalised
/// on mouse-up and reused by the context-menu Copy action.
#[derive(Debug, Clone, Default)]
pub(crate) struct MouseSelection {
    anchor: Option<Cell>,
    current: Option<Cell>,
    pub pane: Option<CopyPane>,
    /// Whether a selection is being made / is currently displayed.
    pub active: bool,
    /// True only while the mouse button is actually held, from `start_in`
    /// until `finish`/`clear` — distinct from `active`, which stays `true`
    /// after `finish` so the highlight persists for copying. Gates whether
    /// further pointer-move events should keep extending the selection:
    /// without this, a finished selection was "sticky" — any stray
    /// Moved/Drag event arriving after mouse-up (some terminals send these
    /// even with no button held) would still call `update` and keep
    /// changing the selection, since `active` alone doesn't distinguish
    /// "still dragging" from "drag finished, just showing the result."
    dragging: bool,
    /// Copied text, populated after mouse-up.
    pub text: String,
}

impl MouseSelection {
    pub(crate) fn start_in(&mut self, pane: CopyPane, cell: Cell) {
        self.anchor = Some(cell);
        self.current = Some(cell);
        self.pane = Some(pane);
        self.active = true;
        self.dragging = true;
        self.text.clear();
    }

    pub(crate) fn update(&mut self, cell: Cell) {
        self.current = Some(cell);
    }

    /// Finalise a drag with the text extracted from the pane. The
    /// selection stays `active` (highlighted, copyable) but is no longer
    /// `dragging` — further pointer movement won't change it.
    pub(crate) fn finish(&mut self, text: String) {
        self.dragging = false;
        self.text = text;
    }

    pub(crate) fn clear(&mut self) {
        self.anchor = None;
        self.current = None;
        self.pane = None;
        self.active = false;
        self.dragging = false;
        self.text.clear();
    }

    /// Whether the mouse button is currently held for this selection —
    /// gates whether pointer-move events should update it. See `dragging`.
    pub(crate) fn is_dragging(&self) -> bool {
        self.dragging && self.anchor.is_some()
    }

    pub(crate) fn is_active(&self) -> bool {
        self.active && self.anchor.is_some() && self.current.is_some()
    }

    /// Normalized selection span, if a selection is in progress. Orders the
    /// anchor/current pair by reading order (row, then column) rather than
    /// min/maxing rows and columns independently — see `SelectionRect`'s
    /// doc comment for why that distinction matters.
    pub(crate) fn rect(&self) -> Option<SelectionRect> {
        let (Some(a), Some(c)) = (self.anchor, self.current) else {
            return None;
        };
        let (start, end) = if (a.row, a.col) <= (c.row, c.col) {
            (a, c)
        } else {
            (c, a)
        };
        Some(SelectionRect {
            row_start: start.row,
            row_end: end.row,
            start_col: start.col,
            end_col: end.col,
        })
    }
}

/// A right-click context-menu action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContextMenuItem {
    Copy,
    ClearSelection,
}

/// A lightweight popover listing copy actions near the right-click point.
#[derive(Debug, Clone)]
pub(crate) struct ContextMenu {
    pub x: u16,
    pub y: u16,
    pub selected: usize,
    pub items: Vec<ContextMenuItem>,
    pub width: u16,
}

impl ContextMenu {
    pub(crate) fn new(x: u16, y: u16) -> Self {
        Self {
            x,
            y,
            selected: 0,
            items: vec![ContextMenuItem::Copy, ContextMenuItem::ClearSelection],
            width: 24,
        }
    }

    /// The popover rectangle (one row per item).
    pub(crate) fn rect(&self) -> Rect {
        Rect {
            x: self.x,
            y: self.y,
            width: self.width,
            height: self.items.len() as u16,
        }
    }

    /// Which item index sits under a screen cell, if any.
    pub(crate) fn index_at(&self, col: u16, row: u16) -> Option<usize> {
        let r = self.rect();
        if col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height {
            Some((row - r.y) as usize)
        } else {
            None
        }
    }
}

/// The Editor pane's inner content area, matching `SourceViewerWidget`'s render
/// geometry: surrounded by a bordered block (1 cell each side) with a one-row
/// header at the top.
pub(crate) fn editor_body(area: Rect) -> Rect {
    Rect {
        x: area.x.saturating_add(1),
        y: area.y.saturating_add(2),
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(3),
    }
}

/// Map a selection rect in screen coordinates to the Editor pane's text,
/// excluding the line-number gutter (content begins after `gutter` columns).
pub(crate) fn editor_selection_text(
    lines: &[String],
    top_line: usize,
    h_scroll: usize,
    area: Rect,
    sel: &MouseSelection,
) -> String {
    let rect = match sel.rect() {
        Some(r) => r,
        None => return String::new(),
    };
    let body = editor_body(area);
    if body.height == 0 || body.width == 0 {
        return String::new();
    }

    let total = lines.len().max(1);
    let number_width = total.to_string().len().max(3);
    let gutter = (number_width + 3) as u16;
    let content_x = body.x.saturating_add(gutter);
    let content_width = body.width.saturating_sub(gutter);

    // Screen column -> buffer char index (accounts for h_scroll + clipping).
    let char_for_col = |col: u16| -> usize {
        if col < content_x || content_width == 0 {
            return h_scroll;
        }
        let rel = (col - content_x) as usize;
        h_scroll + rel.min(content_width.saturating_sub(1) as usize)
    };

    // SourceViewer's line model is character-oriented while Rust string
    // slicing is byte-oriented. Keep screen columns in character space, then
    // convert safely at the final slice boundary so Unicode cannot panic or
    // split a code point.
    let slice_chars = |line: &str, start: usize, end: Option<usize>| -> String {
        let mut chars = line.chars();
        let skipped = chars.by_ref().skip(start);
        match end {
            Some(end) => skipped.take(end.saturating_sub(start)).collect(),
            None => skipped.collect(),
        }
    };

    let mut out: Vec<String> = Vec::new();
    for row in rect.row_start..=rect.row_end {
        if row < body.y || row >= body.y + body.height {
            continue;
        }
        let index = top_line + (row - body.y) as usize;
        let line = lines.get(index).map(String::as_str).unwrap_or("");
        let first = row == rect.row_start;
        let last = row == rect.row_end;

        if first && last {
            let a = char_for_col(rect.start_col).min(line.chars().count());
            let b = char_for_col(rect.end_col).max(a).min(line.chars().count());
            out.push(slice_chars(line, a, Some(b)));
        } else if first {
            let a = char_for_col(rect.start_col).min(line.chars().count());
            out.push(slice_chars(line, a, None));
        } else if last {
            let b = char_for_col(rect.end_col).min(line.chars().count());
            out.push(slice_chars(line, 0, Some(b)));
        } else {
            // Interior rows copy the full logical line (the gutter was never
            // part of `content`, so line numbers never leak).
            out.push(line.to_string());
        }
    }
    out.join("\n")
}

/// Extract a selection from rows already clipped to a pane's visible area.
/// Rendered rows are used intentionally: this preserves markdown wrapping and
/// the exact scroll position the user selected. `strip_prefix` removes
/// presentation-only rails from the Conversation pane.
pub(crate) fn visible_rows_selection_text(
    rows: &[String],
    area: Rect,
    sel: &MouseSelection,
    strip_prefix: bool,
) -> String {
    let Some(rect) = sel.rect() else {
        return String::new();
    };
    let mut output = Vec::new();
    for row in rect.row_start..=rect.row_end {
        if row < area.y || row >= area.bottom() {
            continue;
        }
        let Some(raw) = rows.get((row - area.y) as usize) else {
            continue;
        };
        let line = if strip_prefix {
            strip_conversation_prefix(raw)
        } else {
            raw.clone()
        };
        let chars: Vec<char> = line.chars().collect();
        let start = rect.start_col.saturating_sub(area.x) as usize;
        let end = rect.end_col.saturating_sub(area.x) as usize;
        let selected = if row == rect.row_start && row == rect.row_end {
            chars
                .get(start.min(chars.len())..=end.min(chars.len().saturating_sub(1)))
                .unwrap_or(&[])
                .iter()
                .collect()
        } else if row == rect.row_start {
            chars
                .get(start.min(chars.len())..)
                .unwrap_or(&[])
                .iter()
                .collect()
        } else if row == rect.row_end {
            chars
                .get(..=end.min(chars.len().saturating_sub(1)))
                .unwrap_or(&[])
                .iter()
                .collect()
        } else {
            line
        };
        output.push(selected);
    }
    output.join("\n")
}

fn strip_conversation_prefix(line: &str) -> String {
    line.strip_prefix("│ ")
        .or_else(|| line.strip_prefix("│"))
        .unwrap_or(line)
        .to_string()
}

/// Is a screen cell inside a rectangle?
pub(crate) fn cell_inside(area: Rect, col: u16, row: u16) -> bool {
    col >= area.x
        && col < area.x.saturating_add(area.width)
        && row >= area.y
        && row < area.y.saturating_add(area.height)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(a: Cell, c: Cell) -> MouseSelection {
        let mut s = MouseSelection::default();
        s.start_in(CopyPane::Editor, a);
        s.update(c);
        s
    }

    #[test]
    fn dragging_stops_on_finish_but_active_persists_for_the_highlight() {
        let mut s = MouseSelection::default();
        assert!(!s.is_dragging());
        assert!(!s.is_active());

        s.start_in(CopyPane::Conversation, Cell { row: 0, col: 0 });
        assert!(
            s.is_dragging(),
            "should be dragging while the button is held"
        );
        assert!(s.is_active());

        s.update(Cell { row: 1, col: 5 });
        assert!(s.is_dragging(), "update alone must not end the drag");

        s.finish("hello".into());
        assert!(
            !s.is_dragging(),
            "finish (mouse-up) must end dragging so further pointer movement is ignored"
        );
        assert!(
            s.is_active(),
            "finish must keep the selection active so the highlight persists for copying"
        );

        s.clear();
        assert!(!s.is_dragging());
        assert!(!s.is_active());
    }

    #[test]
    fn rect_normalizes_any_drag_direction() {
        let s = sel(Cell { row: 5, col: 30 }, Cell { row: 2, col: 4 });
        let r = s.rect().unwrap();
        assert_eq!(r.row_start, 2);
        assert_eq!(r.row_end, 5);
        assert_eq!(r.start_col, 4);
        assert_eq!(r.end_col, 30);
    }

    /// A down-left (or up-right) drag has row and column moving in opposite
    /// directions — independently min/maxing rows and columns would swap
    /// which column belongs to which row, corrupting the shape. Reading
    /// order keeps the anchor's own column (5) paired with its own row (0).
    #[test]
    fn rect_pairs_columns_with_their_own_row_on_a_diagonal_opposite_drag() {
        let s = sel(Cell { row: 0, col: 5 }, Cell { row: 2, col: 3 });
        let r = s.rect().unwrap();
        assert_eq!(r.row_start, 0);
        assert_eq!(r.row_end, 2);
        assert_eq!(r.start_col, 5, "row 0's own column, not the global min");
        assert_eq!(r.end_col, 3, "row 2's own column, not the global max");
    }

    #[test]
    fn editor_extraction_multiline_skips_gutter() {
        let lines = vec!["abc".to_string(), "def".to_string(), "ghi".to_string()];
        // Tall area => body: x=3,y=5,width=8,height=4. total=3 lines, number_width=3,
        // gutter=6, so content begins at col 9. Selection cols 4..7, rows 5..7.
        let area = Rect::new(2, 3, 10, 7);
        let s = sel(Cell { row: 5, col: 4 }, Cell { row: 7, col: 7 });
        // row5=>idx0: start col4 < content_x7 => a=0 => "abc"
        // row6=>idx1: full => "def"
        // row7=>idx2: b=char_for_col(7)=0 => ""
        assert_eq!(editor_selection_text(&lines, 0, 0, area, &s), "abc\ndef\n");
    }

    #[test]
    fn editor_single_line_span() {
        let lines = vec!["hello world".to_string()];
        let area = Rect::new(0, 0, 30, 7);
        // body x=1,y=2,w=28,h=4; gutter=6; content_x=7 (char 0).
        // Selecting from inside the gutter (col 6) clamps start to char 0;
        // col 12 => char 5. Range "hello".
        let s = sel(Cell { row: 2, col: 6 }, Cell { row: 2, col: 12 });
        assert_eq!(editor_selection_text(&lines, 0, 0, area, &s), "hello");
    }

    #[test]
    fn editor_extraction_clamps_to_line_length() {
        let lines = vec!["xy".to_string()];
        let area = Rect::new(0, 0, 30, 7);
        // Start at char 1 (col 8), drag far right; end clamps to line len 2.
        let s = sel(Cell { row: 2, col: 8 }, Cell { row: 2, col: 60 });
        assert_eq!(editor_selection_text(&lines, 0, 0, area, &s), "y");
    }

    #[test]
    fn editor_extraction_does_not_split_unicode() {
        let lines = vec!["λ🙂z".to_string()];
        let area = Rect::new(0, 0, 30, 7);
        // content starts at col 7; select the first two character cells.
        let s = sel(Cell { row: 2, col: 7 }, Cell { row: 2, col: 9 });
        assert_eq!(editor_selection_text(&lines, 0, 0, area, &s), "λ🙂");
    }

    #[test]
    fn conversation_extraction_removes_rail_and_preserves_rows() {
        let rows = vec!["│ hello".to_string(), "│ world".to_string()];
        let area = Rect::new(4, 10, 20, 2);
        let mut selection = MouseSelection::default();
        selection.start_in(CopyPane::Conversation, Cell { row: 10, col: 4 });
        selection.update(Cell { row: 11, col: 10 });
        assert_eq!(
            visible_rows_selection_text(&rows, area, &selection, true),
            "hello\nworld"
        );
    }

    #[test]
    fn diff_and_terminal_extraction_keep_display_text_without_number_injection() {
        let rows = vec!["@@ -1 +1 @@".to_string(), "+changed".to_string()];
        let area = Rect::new(2, 4, 30, 2);
        let mut selection = MouseSelection::default();
        selection.start_in(CopyPane::Diff, Cell { row: 4, col: 2 });
        selection.update(Cell { row: 5, col: 20 });
        assert_eq!(
            visible_rows_selection_text(&rows, area, &selection, false),
            "@@ -1 +1 @@\n+changed"
        );

        selection.start_in(CopyPane::Terminal, Cell { row: 4, col: 2 });
        selection.update(Cell { row: 5, col: 20 });
        assert_eq!(
            visible_rows_selection_text(&rows, area, &selection, false),
            "@@ -1 +1 @@\n+changed"
        );
    }

    #[test]
    fn context_menu_index_respects_popover_bounds() {
        let m = ContextMenu::new(10, 10);
        assert_eq!(m.index_at(12, 11), Some(1));
        assert_eq!(m.index_at(5, 11), None); // left of popover
        assert_eq!(m.index_at(12, 20), None); // below popover
    }

    #[test]
    fn cell_inside_uses_half_open_interval() {
        let r = Rect::new(1, 1, 4, 4);
        assert!(cell_inside(r, 1, 1));
        assert!(cell_inside(r, 4, 4));
        assert!(!cell_inside(r, 5, 1));
        assert!(!cell_inside(r, 1, 5));
    }
}
