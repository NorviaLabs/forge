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

/// A normalized (top-left -> bottom-right) screen rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SelectionRect {
    pub row_start: u16,
    pub row_end: u16,
    pub col_start: u16,
    pub col_end: u16,
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
    /// Copied text, populated after mouse-up.
    pub text: String,
}

impl MouseSelection {
    pub(crate) fn start_in(&mut self, pane: CopyPane, cell: Cell) {
        self.anchor = Some(cell);
        self.current = Some(cell);
        self.pane = Some(pane);
        self.active = true;
        self.text.clear();
    }

    pub(crate) fn update(&mut self, cell: Cell) {
        self.current = Some(cell);
    }

    /// Finalise a drag with the text extracted from the pane.
    pub(crate) fn finish(&mut self, text: String) {
        self.text = text;
    }

    pub(crate) fn clear(&mut self) {
        self.anchor = None;
        self.current = None;
        self.pane = None;
        self.active = false;
        self.text.clear();
    }

    pub(crate) fn is_active(&self) -> bool {
        self.active && self.anchor.is_some() && self.current.is_some()
    }

    /// Normalized selection rectangle, if a selection is in progress.
    pub(crate) fn rect(&self) -> Option<SelectionRect> {
        let (Some(a), Some(c)) = (self.anchor, self.current) else {
            return None;
        };
        Some(SelectionRect {
            row_start: a.row.min(c.row),
            row_end: a.row.max(c.row),
            col_start: a.col.min(c.col),
            col_end: a.col.max(c.col),
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
            let a = char_for_col(rect.col_start).min(line.chars().count());
            let b = char_for_col(rect.col_end).max(a).min(line.chars().count());
            out.push(slice_chars(line, a, Some(b)));
        } else if first {
            let a = char_for_col(rect.col_start).min(line.chars().count());
            out.push(slice_chars(line, a, None));
        } else if last {
            let b = char_for_col(rect.col_end).min(line.chars().count());
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
        let start = rect.col_start.saturating_sub(area.x) as usize;
        let end = rect.col_end.saturating_sub(area.x) as usize;
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
    fn rect_normalizes_any_drag_direction() {
        let s = sel(Cell { row: 5, col: 30 }, Cell { row: 2, col: 4 });
        let r = s.rect().unwrap();
        assert_eq!(r.row_start, 2);
        assert_eq!(r.row_end, 5);
        assert_eq!(r.col_start, 4);
        assert_eq!(r.col_end, 30);
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
