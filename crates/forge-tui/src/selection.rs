//! Mouse text selection and the right-click context menu.
//!
//! Endpoints are inclusive terminal cells. Source, preview and terminal panes
//! capture rendered cell spans; transcript drags retain the complete row model
//! and its painted viewport. Changed mappings invalidate screen selections.

use ratatui::buffer::CellWidth;
use ratatui::layout::Rect;

/// Text and cell widths captured after the pane's renderer has laid out text.
/// Wide-cell continuations are not characters; OSC 8 wrappers are not text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RenderedText {
    pub area: Rect,
    pub rows: Vec<Vec<(u16, u16, ratatui::buffer::Cell)>>,
    pub revision: u64,
}

impl RenderedText {
    pub fn capture(buf: &ratatui::buffer::Buffer, area: Rect) -> Self {
        let rows = (area.y..area.bottom())
            .map(|row| {
                let mut cells = Vec::with_capacity(area.width as usize);
                let mut col = area.x;
                while col < area.right() {
                    let cell = &buf[(col, row)];
                    let width = cell.cell_width().max(1);
                    let symbol = cell.symbol();
                    let text = if let Some(link) = symbol.strip_prefix("\x1b]8;;") {
                        link.split_once("\x1b\\")
                            .and_then(|(_, label)| label.split_once("\x1b]8;;"))
                            .map(|(label, _)| label)
                            .unwrap_or("")
                    } else {
                        symbol
                    };
                    // Ratatui stores short symbols inline: no heap string per cell.
                    let mut captured = ratatui::buffer::Cell::default();
                    captured.set_symbol(text);
                    cells.push((col, width, captured));
                    col = col.saturating_add(width);
                }
                cells
            })
            .collect();
        Self {
            area,
            rows,
            revision: 0,
        }
    }

    pub fn selection_text(&self, sel: &MouseSelection, strip_prefix: bool) -> String {
        let Some(rect) = sel.rect() else {
            return String::new();
        };
        let mut output = Vec::new();
        for row in
            rect.row_start.max(self.area.y)..=rect.row_end.min(self.area.bottom().saturating_sub(1))
        {
            let Some(cells) = self.rows.get((row - self.area.y) as usize) else {
                continue;
            };
            let left = if row == rect.row_start {
                rect.start_col
            } else {
                self.area.x
            };
            let right = if row == rect.row_end {
                rect.end_col
            } else {
                self.area.right().saturating_sub(1)
            };
            let prefix = if strip_prefix
                && cells
                    .first()
                    .is_some_and(|(_, _, text)| text.symbol() == "│")
            {
                self.area.x
                    + if cells
                        .get(1)
                        .is_some_and(|(_, _, text)| text.symbol() == " ")
                    {
                        2
                    } else {
                        1
                    }
            } else {
                self.area.x
            };
            let text = cells
                .iter()
                .filter(|(col, width, _)| {
                    *col >= prefix && *col <= right && col.saturating_add(*width) > left
                })
                .map(|(_, _, text)| text.symbol())
                .collect::<String>();
            output.push(text.trim_end_matches(' ').to_owned());
        }
        output.join("\n")
    }

    /// Invalidate screen-anchored selections before painting changed cells.
    pub fn validate_selection(&self, old: &Self, pane: CopyPane, sel: &mut MouseSelection) {
        if sel.pane == Some(pane) && sel.is_active() && self != old {
            sel.clear();
        }
    }
}

pub(crate) fn mapping_revision(value: impl std::hash::Hash) -> u64 {
    use std::hash::Hasher;
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hash);
    hash.finish()
}

fn display_slice(line: &str, start: usize, end: usize) -> String {
    let mut col = 0;
    ratatui::text::Span::raw(line)
        .styled_graphemes(ratatui::style::Style::default())
        .filter(|text| {
            let left = col;
            col += ratatui::text::Span::raw(text.symbol).width();
            left < end && col > start
        })
        .map(|text| text.symbol)
        .collect()
}

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

    /// Keep selected text anchored to its logical rows while its pane scrolls.
    pub(crate) fn shift_rows(&mut self, delta: i32) {
        let shift = |cell: &mut Option<Cell>| {
            if let Some(cell) = cell {
                cell.row = if delta >= 0 {
                    cell.row.saturating_add(delta as u16)
                } else {
                    cell.row.saturating_sub(delta.unsigned_abs() as u16)
                };
            }
        };
        shift(&mut self.anchor);
        shift(&mut self.current);
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

    /// Keep painting and pointer hit-testing on the same on-screen rectangle.
    pub(crate) fn fit(&mut self, frame: Rect) {
        self.width = self.width.min(frame.width);
        self.x = self
            .x
            .clamp(frame.x, frame.right().saturating_sub(self.width));
        self.y = self.y.clamp(
            frame.y,
            frame
                .bottom()
                .saturating_sub(self.items.len() as u16)
                .max(frame.y),
        );
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
        let (line, prefix_width) = if strip_prefix {
            strip_conversation_prefix(raw)
        } else {
            (raw.clone(), 0)
        };
        let start = rect
            .start_col
            .saturating_sub(area.x)
            .saturating_sub(prefix_width) as usize;
        let end = rect
            .end_col
            .saturating_sub(area.x)
            .saturating_sub(prefix_width) as usize;
        let selected = display_slice(
            &line,
            if row == rect.row_start { start } else { 0 },
            if row == rect.row_end {
                end.saturating_add(1)
            } else {
                usize::MAX
            },
        );
        output.push(selected);
    }
    output.join("\n")
}

/// Extract from a complete logical row set whose viewport begins at `top`.
/// Endpoints may sit outside the pane after drag-to-edge autoscrolling.
pub(crate) fn rows_selection_text(
    rows: &[String],
    top: usize,
    area: Rect,
    sel: &MouseSelection,
    strip_prefix: bool,
) -> String {
    let Some(rect) = sel.rect() else {
        return String::new();
    };
    let mut selected_rows = Vec::new();
    for screen_row in rect.row_start..=rect.row_end {
        let index = top as i64 + screen_row as i64 - area.y as i64;
        if index >= 0 {
            if let Some(row) = rows.get(index as usize) {
                selected_rows.push(row.clone());
            }
        }
    }
    if selected_rows.is_empty() {
        return String::new();
    }
    let translated = MouseSelection {
        anchor: Some(Cell {
            row: area.y,
            col: rect.start_col,
        }),
        current: Some(Cell {
            row: area.y.saturating_add(selected_rows.len() as u16 - 1),
            col: rect.end_col,
        }),
        pane: sel.pane,
        active: true,
        dragging: false,
        text: String::new(),
    };
    visible_rows_selection_text(
        &selected_rows,
        Rect {
            height: selected_rows.len() as u16,
            ..area
        },
        &translated,
        strip_prefix,
    )
}

fn strip_conversation_prefix(line: &str) -> (String, u16) {
    if let Some(line) = line.strip_prefix("│ ") {
        (line.to_string(), 2)
    } else if let Some(line) = line.strip_prefix("│") {
        (line.to_string(), 1)
    } else {
        (line.to_string(), 0)
    }
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

    #[test]
    fn rendered_cells_copy_unicode_using_display_columns() {
        use ratatui::widgets::Widget;
        let area = Rect::new(3, 2, 20, 1);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        ratatui::text::Line::raw("A界BC🙂D e\u{301}Z").render(area, &mut buf);
        let text = RenderedText::capture(&buf, area);
        let s = sel(Cell { row: 2, col: 6 }, Cell { row: 2, col: 7 });
        assert_eq!(text.selection_text(&s, false), "BC");
        assert_eq!(
            visible_rows_selection_text(&["A界BC🙂D e\u{301}Z".into()], area, &s, false),
            "BC"
        );
        let s = sel(Cell { row: 2, col: 12 }, Cell { row: 2, col: 13 });
        assert_eq!(text.selection_text(&s, false), "e\u{301}Z");
        let s = sel(Cell { row: 2, col: 5 }, Cell { row: 2, col: 5 });
        assert_eq!(
            text.selection_text(&s, false),
            "界",
            "continuation cell includes whole glyph"
        );
    }

    #[test]
    fn rendered_mapping_changes_clear_finished_and_active_selection() {
        use ratatui::widgets::Widget;
        let area = Rect::new(0, 0, 20, 1);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        ratatui::text::Line::raw("TERM-28").render(area, &mut buf);
        let old = RenderedText::capture(&buf, area);
        let mut selection = MouseSelection::default();
        selection.start_in(CopyPane::Terminal, Cell { row: 0, col: 0 });
        selection.update(Cell { row: 0, col: 6 });
        selection.finish(old.selection_text(&selection, false));
        old.validate_selection(&old, CopyPane::Terminal, &mut selection);
        assert!(selection.is_active());
        ratatui::text::Line::raw("TERM-25").render(area, &mut buf);
        let new = RenderedText::capture(&buf, area);
        new.validate_selection(&old, CopyPane::Terminal, &mut selection);
        assert!(!selection.is_active());
        assert!(selection.text.is_empty());
        selection.start_in(CopyPane::Terminal, Cell { row: 0, col: 0 });
        let resized = RenderedText::capture(&buf, Rect::new(0, 0, 10, 1));
        resized.validate_selection(&new, CopyPane::Terminal, &mut selection);
        assert!(!selection.is_dragging());
    }

    #[test]
    fn source_copy_uses_rendered_editor_and_preview_cells() {
        use crate::source_viewer::{SourceViewer, SourceViewerWidget, ViewerStatus};
        use ratatui::widgets::Widget;
        let area = Rect::new(0, 0, 40, 12);
        let mut viewer = SourceViewer::new();
        viewer.status = ViewerStatus::Ok;
        viewer.lines = vec!["ASCII: ABCDEF".into(), "OTHER ROW".into()];
        let mut editor = crate::editor_session::EditorSession::new("ASCII: ABCDEF\nOTHER ROW");
        let mut buf = ratatui::buffer::Buffer::empty(area);
        SourceViewerWidget {
            viewer: &mut viewer,
            focused: false,
            editor: Some(&mut editor),
            editor_command: None,
            editor_message: None,
        }
        .render(area, &mut buf);
        let (row, col) = (0..area.height)
            .find_map(|y| {
                (0..area.width.saturating_sub(5)).find_map(|x| {
                    ((0..6)
                        .map(|dx| buf[(x + dx, y)].symbol())
                        .collect::<String>()
                        == "ABCDEF")
                        .then_some((y, x))
                })
            })
            .expect("fixture must be visible in edtui buffer");
        assert_eq!(
            viewer
                .rendered_text
                .selection_text(&sel(Cell { row, col }, Cell { row, col: col + 5 }), false),
            "ABCDEF"
        );
        assert!(row > 2, "actual editor includes air row");
        editor.place_cursor_at(col, row);
        assert_eq!((editor.cursor_row(), editor.cursor_col()), (0, 7));

        viewer.markdown_preview = true;
        editor = crate::editor_session::EditorSession::new("**PREVIEW**");
        buf.reset();
        SourceViewerWidget {
            viewer: &mut viewer,
            focused: false,
            editor: Some(&mut editor),
            editor_command: None,
            editor_message: None,
        }
        .render(area, &mut buf);
        let body = viewer.rendered_text.area;
        let row = (body.y..body.bottom())
            .find(|y| {
                (body.x..body.right())
                    .map(|x| buf[(x, *y)].symbol())
                    .collect::<String>()
                    .contains("PREVIEW")
            })
            .unwrap();
        assert_eq!(
            viewer
                .rendered_text
                .selection_text(
                    &sel(
                        Cell { row, col: body.x },
                        Cell {
                            row,
                            col: body.right() - 1
                        }
                    ),
                    false
                )
                .trim(),
            "PREVIEW"
        );
    }

    #[test]
    fn rendered_editor_mapping_tracks_wrapped_unicode_tabs_and_scrolled_lines() {
        use crate::source_viewer::{SourceViewer, SourceViewerWidget, ViewerStatus};
        use ratatui::widgets::Widget;
        let area = Rect::new(0, 0, 20, 12);
        let mut viewer = SourceViewer::new();
        viewer.status = ViewerStatus::Ok;
        let source = "01234567890123456A界BC\tD e\u{301}Z";
        let mut editor = crate::editor_session::EditorSession::new(source);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        SourceViewerWidget {
            viewer: &mut viewer,
            focused: false,
            editor: Some(&mut editor),
            editor_command: None,
            editor_message: None,
        }
        .render(area, &mut buf);
        let body = viewer.rendered_text.area;
        let (row, col) = (body.y..body.bottom())
            .find_map(|y| {
                (body.x..body.right().saturating_sub(1)).find_map(|x| {
                    (buf[(x, y)].symbol() == "B" && buf[(x + 1, y)].symbol() == "C")
                        .then_some((y, x))
                })
            })
            .unwrap();
        assert!(row > body.y, "fixture wraps before BC");
        assert_eq!(
            viewer
                .rendered_text
                .selection_text(&sel(Cell { row, col }, Cell { row, col: col + 1 }), false),
            "BC"
        );
        editor.place_cursor_at(col, row);
        assert_eq!((editor.cursor_row(), editor.cursor_col()), (0, 19));
        let (row, col) = (body.y..body.bottom())
            .find_map(|y| {
                (body.x..body.right()).find_map(|x| (buf[(x, y)].symbol() == "D").then_some((y, x)))
            })
            .unwrap();
        editor.place_cursor_at(col, row);
        assert_eq!(
            (editor.cursor_row(), editor.cursor_col()),
            (0, 22),
            "expanded tab consumes one source character"
        );

        editor = crate::editor_session::EditorSession::new(
            &(0..100)
                .map(|i| format!("ROW{i:02}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        editor.set_cursor(70, 0);
        buf.reset();
        // edtui establishes its viewport height on the first frame, then
        // scrolls to an already-positioned cursor on the next frame.
        SourceViewerWidget {
            viewer: &mut viewer,
            focused: false,
            editor: Some(&mut editor),
            editor_command: None,
            editor_message: None,
        }
        .render(area, &mut buf);
        SourceViewerWidget {
            viewer: &mut viewer,
            focused: false,
            editor: Some(&mut editor),
            editor_command: None,
            editor_message: None,
        }
        .render(area, &mut buf);
        let body = viewer.rendered_text.area;
        let (row, col) = (body.y..body.bottom())
            .find_map(|y| {
                (body.x..body.right().saturating_sub(4)).find_map(|x| {
                    ((0..5)
                        .map(|dx| buf[(x + dx, y)].symbol())
                        .collect::<String>()
                        == "ROW70")
                        .then_some((y, x))
                })
            })
            .unwrap_or_else(|| panic!("ROW70 missing: {:?}", viewer.rendered_text.rows));
        assert_eq!(
            viewer
                .rendered_text
                .selection_text(&sel(Cell { row, col }, Cell { row, col: col + 4 }), false),
            "ROW70"
        );
        editor.place_cursor_at(col, row);
        assert_eq!((editor.cursor_row(), editor.cursor_col()), (70, 0));
    }

    #[test]
    fn conversation_columns_are_measured_after_the_rail() {
        let rows = vec!["│ hello world".to_string()];
        let area = Rect::new(4, 2, 20, 1);
        let mut selection = MouseSelection::default();
        selection.start_in(CopyPane::Conversation, Cell { row: 2, col: 12 });
        selection.update(Cell { row: 2, col: 16 });

        assert_eq!(
            visible_rows_selection_text(&rows, area, &selection, true),
            "world"
        );
    }

    #[test]
    fn complete_rows_can_copy_beyond_the_visible_viewport() {
        let rows = (0..10).map(|i| format!("row {i}")).collect::<Vec<_>>();
        let area = Rect::new(0, 5, 20, 3);
        let mut selection = MouseSelection::default();
        selection.start_in(CopyPane::Conversation, Cell { row: 9, col: 4 });
        selection.update(Cell { row: 5, col: 0 });

        assert_eq!(
            rows_selection_text(&rows, 2, area, &selection, false),
            "row 2\nrow 3\nrow 4\nrow 5\nrow 6"
        );
    }

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
    fn terminal_extraction_keeps_display_text_without_number_injection() {
        let rows = vec!["@@ -1 +1 @@".to_string(), "+changed".to_string()];
        let area = Rect::new(2, 4, 30, 2);
        let mut selection = MouseSelection::default();
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
    fn context_menu_fits_all_frame_corners_and_hit_testing() {
        for frame in [Rect::new(0, 0, 80, 18), Rect::new(4, 3, 120, 40)] {
            for (x, y) in [
                (frame.x, frame.y),
                (frame.right(), frame.y),
                (frame.x, frame.bottom()),
                (frame.right(), frame.bottom()),
            ] {
                let mut menu = ContextMenu::new(x, y);
                menu.fit(frame);
                assert!(frame.contains((menu.x, menu.y).into()));
                assert!(menu.rect().right() <= frame.right());
                assert!(menu.rect().bottom() <= frame.bottom());
                assert_eq!(menu.index_at(menu.x, menu.y), Some(0));
                assert_eq!(menu.index_at(menu.x, menu.y + 1), Some(1));
            }
        }
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
