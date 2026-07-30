//! Wrapped visual-row layout for the active composer.

use crate::user_message_gutter::{gutter_prefix_width, GUTTER_GAP};

/// One wrapped visual row of composer content (byte range into the buffer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComposerVisualRow {
    pub start: usize,
    pub end: usize,
}

impl ComposerVisualRow {
    pub fn fragment<'a>(&self, text: &'a str) -> &'a str {
        &text[self.start..self.end.min(text.len())]
    }

    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }
}

/// Cached wrapped layout for the active composer.
#[derive(Debug, Default)]
pub struct ComposerLayoutCache {
    revision: u64,
    width: usize,
    rows: Vec<ComposerVisualRow>,
}

impl ComposerLayoutCache {
    pub fn rows(&mut self, revision: u64, text: &str, width: usize) -> &[ComposerVisualRow] {
        if self.revision == revision && self.width == width {
            return &self.rows;
        }
        self.rows = build_visual_rows(text, width);
        self.revision = revision;
        self.width = width;
        &self.rows
    }
}

/// Build wrapped visual rows with source byte offsets.
pub fn build_visual_rows(text: &str, content_width: usize) -> Vec<ComposerVisualRow> {
    if text.is_empty() {
        return vec![ComposerVisualRow { start: 0, end: 0 }];
    }

    let mut rows = Vec::new();
    let mut line_start = 0usize;
    for (line_idx, line) in text.split('\n').enumerate() {
        if line_idx > 0 {
            line_start += 1;
        }
        rows.extend(visual_rows_for_line(line, line_start, content_width));
        line_start += line.len();
    }
    rows
}

fn visual_rows_for_line(
    line: &str,
    line_start: usize,
    content_width: usize,
) -> Vec<ComposerVisualRow> {
    wrap_line_ranges(line, content_width)
        .into_iter()
        .map(|(start, end)| ComposerVisualRow {
            start: line_start + start,
            end: line_start + end,
        })
        .collect()
}

/// Word-wrap `line` into byte ranges without allocating per-fragment strings.
fn wrap_line_ranges(line: &str, width: usize) -> Vec<(usize, usize)> {
    if line.is_empty() {
        return vec![(0, 0)];
    }
    if line.len() <= width {
        return vec![(0, line.len())];
    }

    let mut out = Vec::new();
    let mut row_start = 0usize;
    let mut row_len = 0usize;
    let mut search_from = 0usize;

    for word in line.split_whitespace() {
        let word_pos = line[search_from..]
            .find(word)
            .map(|p| search_from + p)
            .unwrap_or(search_from);
        let word_len = word.len();

        if row_len == 0 {
            row_start = word_pos;
            row_len = word_len;
        } else if row_len + 1 + word_len <= width {
            row_len += 1 + word_len;
        } else {
            out.push((row_start, row_start + row_len));
            row_start = word_pos;
            row_len = word_len;
        }
        search_from = word_pos + word_len;
    }

    if row_len > 0 {
        out.push((row_start, row_start + row_len));
    }
    if out.is_empty() {
        out.push((0, line.len()));
    }
    out
}

/// Locate the cursor within wrapped visual rows.
pub fn locate_cursor(text: &str, cursor: usize, content_width: usize) -> (usize, usize) {
    let cursor = cursor.min(text.len());
    let rows = build_visual_rows(text, content_width);
    locate_cursor_in_rows(&rows, cursor)
}

/// Scroll offset so `cursor_row` stays visible in `visible_rows`.
pub fn scroll_offset(cursor_row: usize, total_rows: usize, visible_rows: usize) -> usize {
    if total_rows <= visible_rows || visible_rows == 0 {
        return 0;
    }
    if cursor_row + 1 <= visible_rows {
        0
    } else {
        (cursor_row + 1).saturating_sub(visible_rows)
    }
}

/// Locate the cursor within pre-built wrapped visual rows.
pub fn locate_cursor_in_rows(rows: &[ComposerVisualRow], cursor: usize) -> (usize, usize) {
    if rows.is_empty() {
        return (0, 0);
    }
    for (row_idx, row) in rows.iter().enumerate() {
        if cursor > row.start && cursor <= row.end {
            return (row_idx, cursor - row.start);
        }
        if cursor == row.start {
            return (row_idx, 0);
        }
    }
    let last = rows.len() - 1;
    (last, rows[last].len())
}

/// Map a mouse click within a visual row to a buffer index.
pub fn click_to_cursor(
    text: &str,
    visual_row: usize,
    display_col: usize,
    glyph: &str,
    content_width: usize,
) -> usize {
    let prefix_width = gutter_prefix_width(glyph);
    let content_col = display_col.saturating_sub(prefix_width);
    let rows = build_visual_rows(text, content_width);
    let Some(row) = rows.get(visual_row) else {
        return text.len();
    };
    let col = content_col.min(row.len());
    row.start + col
}

/// Layout rows and cursor position in one pass.
pub fn layout_with_cursor(
    text: &str,
    cursor: usize,
    content_width: usize,
) -> (Vec<ComposerVisualRow>, usize, usize) {
    let cursor = cursor.min(text.len());
    let rows = build_visual_rows(text, content_width);
    let (cursor_row, cursor_col) = locate_cursor_in_rows(&rows, cursor);
    (rows, cursor_row, cursor_col)
}

/// Clamp a display click column to the first editable cell on a row.
pub fn clamp_display_column_to_content(display_col: usize, prefix_width: usize) -> usize {
    display_col.saturating_sub(prefix_width)
}

/// Screen column for the first editable character on a row.
pub fn content_start_column(prefix_width: usize) -> usize {
    prefix_width
}

/// Strip decorative gutter prefix from a rendered composer row.
pub fn strip_rendered_prefix<'a>(line: &'a str, glyph: &str) -> &'a str {
    crate::user_message_gutter::strip_rendered_line_prefix(line, glyph)
}

/// Number of wrapped visual rows for height estimation.
pub fn visual_row_count(text: &str, content_width: usize) -> usize {
    build_visual_rows(text, content_width).len().max(1)
}

/// Copy text is the buffer itself; placeholders and gutters are never included.
pub fn copy_buffer(text: &str) -> &str {
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_message_gutter::gutter_glyph;
    use forge_config::Theme;

    fn glyph() -> &'static str {
        gutter_glyph(Theme::Dark, false)
    }

    fn prefix_width() -> usize {
        gutter_prefix_width(glyph())
    }

    #[test]
    fn empty_buffer_has_one_visual_row() {
        let rows = build_visual_rows("", 40);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].fragment(""), "");
    }

    #[test]
    fn single_line_maps_cursor() {
        let text = "Summarize this codebase";
        let (row, col) = locate_cursor(text, 0, 80);
        assert_eq!(row, 0);
        assert_eq!(col, 0);
        let (row, col) = locate_cursor(text, text.len(), 80);
        assert_eq!(row, 0);
        assert_eq!(col, text.len());
    }

    #[test]
    fn wrapped_input_has_gutter_row_per_visual_line() {
        let text = "word ".repeat(20);
        let rows = build_visual_rows(text.trim(), 20);
        assert!(rows.len() >= 3);
        for row in &rows {
            assert!(row.end >= row.start);
        }
    }

    #[test]
    fn explicit_newlines_produce_rows() {
        let text = "one\ntwo\nthree";
        let rows = build_visual_rows(text, 80);
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn blank_line_retains_row() {
        let text = "First.\n\nSecond.";
        let rows = build_visual_rows(text, 80);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1].fragment(text), "");
    }

    #[test]
    fn cursor_at_start_maps_after_prefix() {
        assert_eq!(content_start_column(prefix_width()), prefix_width());
        let (row, col) = locate_cursor("hello", 0, 40);
        assert_eq!((row, col), (0, 0));
    }

    #[test]
    fn click_on_gutter_clamps_to_row_start() {
        let text = "hello world";
        let idx = click_to_cursor(text, 0, 0, glyph(), 40);
        assert_eq!(idx, 0);
        let idx = click_to_cursor(text, 0, prefix_width() - 1, glyph(), 40);
        assert_eq!(idx, 0);
    }

    #[test]
    fn click_on_first_character() {
        let text = "hello";
        let idx = click_to_cursor(text, 0, prefix_width(), glyph(), 40);
        assert_eq!(idx, 0);
    }

    #[test]
    fn copy_returns_buffer_only() {
        let text = "Explain how session recovery works";
        assert_eq!(copy_buffer(text), text);
        assert!(!copy_buffer(text).contains(glyph()));
    }

    fn scroll_keeps_cursor_row_visible_with_tight_viewport() {
        assert_eq!(scroll_offset(4, 8, 4), 1);
        assert_eq!(scroll_offset(7, 8, 4), 4);
    }

    #[test]
    fn locate_cursor_in_rows_reuses_layout() {
        let text = "one\ntwo\nthree\nfour\nfive";
        let rows = build_visual_rows(text, 40);
        assert_eq!(locate_cursor_in_rows(&rows, text.len()), (4, 4));
    }

    #[test]
    fn continuation_rows_keep_offsets() {
        let text = "word ".repeat(30);
        let rows = build_visual_rows(text.trim(), 15);
        assert!(rows.len() > 2);
        let tail = &rows[1..];
        assert!(tail
            .iter()
            .all(|row| row.start < row.end || row.fragment(text.trim()).is_empty()));
    }

    #[test]
    fn pasted_code_preserves_indentation_in_fragments() {
        let text = "Review:\n\nfn main() {\n    println!(\"hi\");\n}";
        let rows = build_visual_rows(text, 80);
        assert!(rows
            .iter()
            .any(|row| row.fragment(text).contains("println!")));
    }

    #[test]
    fn large_paste_placeholder_layout_is_stable() {
        let text = "[Pasted Content 1001 chars]";
        let rows = build_visual_rows(text, 40);
        assert_eq!(rows.len(), 1);
        assert_eq!(locate_cursor(text, text.len(), 40), (0, text.len()));
    }

    #[test]
    fn unicode_cursor_alignment() {
        let text = "emoji 🚀 日本語";
        let rows = build_visual_rows(text, 12);
        assert!(rows.len() >= 2);
        let (row, col) = locate_cursor(text, 0, 12);
        assert_eq!((row, col), (0, 0));
    }

    #[test]
    fn resize_reflow_changes_row_count() {
        let text = "word ".repeat(20).trim().to_string();
        let wide = build_visual_rows(&text, 80);
        let narrow = build_visual_rows(&text, 20);
        assert!(narrow.len() > wide.len());
    }

    #[test]
    fn layout_cache_reuses_rows_until_revision_changes() {
        let text = "hello world";
        let mut cache = ComposerLayoutCache::default();
        let first = cache.rows(1, text, 40) as *const [ComposerVisualRow];
        let second = cache.rows(1, text, 40) as *const [ComposerVisualRow];
        assert_eq!(first, second);
        let third = cache.rows(2, text, 40) as *const [ComposerVisualRow];
        assert_ne!(first, third);
    }
}
