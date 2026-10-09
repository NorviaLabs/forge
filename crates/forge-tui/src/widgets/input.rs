//! Input bar — multi-line paste / Shift+Enter newline.

use crate::theme;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Paragraph, Widget, Wrap};
use std::ops::Range;

#[derive(Debug, Clone, Default)]
pub struct InputModel {
    pub text: String,
    pub cursor: usize,
    pub dimmed: bool,
    pub hint: String,
    /// When true, text uses history_active background (Phase 7 browse).
    pub history_browse: bool,
    /// No live LLM provider — chrome warns; chat send is gated in the app.
    pub not_connected: bool,
    /// Approval pending — the composer is not the answer input. Renders a
    /// distinct waiting border and suppresses the empty-state hint.
    pub waiting: bool,
    /// Full payloads represented by compact, atomic placeholders in `text`.
    pending_pastes: Vec<PendingPaste>,
}

const LARGE_PASTE_CHAR_THRESHOLD: usize = 1000;
const MAX_VISIBLE_ROWS: usize = crate::design::MAX_COMPOSER_INPUT_H as usize;
// A private probe style locates the caret without inserting a character into
// the draft. The real draw replaces this style before backend output.
const CURSOR_MARK: Color = Color::Indexed(255);

#[derive(Debug, Clone)]
struct PendingPaste {
    placeholder: String,
    content: String,
    range: Range<usize>,
}

impl InputModel {
    pub fn insert(&mut self, c: char) {
        let i = self.insertion_cursor();
        self.shift_ranges_for_insert(i, c.len_utf8());
        self.text.insert(i, c);
        self.cursor = i + c.len_utf8();
    }

    /// Insert a newline at the cursor (Shift+Enter / paste).
    pub fn insert_newline(&mut self) {
        self.insert('\n');
    }

    /// Insert clipboard text, compacting payloads over 1,000 characters into an
    /// atomic Codex-style placeholder while retaining the full submission text.
    pub fn insert_paste(&mut self, pasted: &str) {
        let pasted = normalize_pasted_text(pasted);
        if pasted.is_empty() {
            return;
        }

        let char_count = pasted.chars().count();
        if char_count > LARGE_PASTE_CHAR_THRESHOLD {
            let placeholder = self.next_large_paste_placeholder(char_count);
            let start = self.insertion_cursor();
            self.shift_ranges_for_insert(start, placeholder.len());
            self.text.insert_str(start, &placeholder);
            self.cursor = start + placeholder.len();
            self.pending_pastes.push(PendingPaste {
                range: start..self.cursor,
                placeholder,
                content: pasted,
            });
        } else {
            self.insert_str(&pasted);
        }
    }

    /// Deliberate handoff appends to the retained draft, regardless of caret.
    pub fn append_paste(&mut self, pasted: &str) {
        if pasted.trim().is_empty() {
            return;
        }
        self.cursor = self.text.len();
        if !self.text.is_empty() && !self.text.ends_with('\n') {
            self.insert_newline();
        }
        self.insert_paste(pasted);
    }

    /// An explicit result handoff remains editable and visible in the draft.
    pub fn append_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.cursor = self.text.len();
        if !self.text.is_empty() && !self.text.ends_with('\n') {
            self.insert_newline();
        }
        self.insert_str(&normalize_pasted_text(text));
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        if let Some(index) = self
            .pending_pastes
            .iter()
            .position(|paste| self.cursor > paste.range.start && self.cursor <= paste.range.end)
        {
            let paste = self.pending_pastes.remove(index);
            let removed = paste.range.end - paste.range.start;
            self.text.replace_range(paste.range.clone(), "");
            self.cursor = paste.range.start;
            self.shift_ranges_after_remove(paste.range.end, removed);
            return;
        }

        let prev = self.text[..self.cursor]
            .chars()
            .next_back()
            .map(|c| c.len_utf8())
            .unwrap_or(1);
        let start = self.cursor - prev;
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
        self.shift_ranges_after_remove(start + prev, prev);
    }

    pub fn move_left(&mut self) {
        if self.cursor == 0 {
            return;
        }
        if let Some(paste) = self
            .pending_pastes
            .iter()
            .find(|paste| self.cursor > paste.range.start && self.cursor <= paste.range.end)
        {
            self.cursor = paste.range.start;
            return;
        }
        let prev = self.text[..self.cursor]
            .chars()
            .next_back()
            .map(|c| c.len_utf8())
            .unwrap_or(1);
        self.cursor -= prev;
    }

    pub fn move_right(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        if let Some(paste) = self
            .pending_pastes
            .iter()
            .find(|paste| self.cursor >= paste.range.start && self.cursor < paste.range.end)
        {
            self.cursor = paste.range.end;
            return;
        }
        let next = self.text[self.cursor..]
            .chars()
            .next()
            .map(|c| c.len_utf8())
            .unwrap_or(1);
        self.cursor += next;
    }

    /// Move the cursor up one visual (wrapped) row, preserving column
    /// position where possible. Returns `false` when already on the first
    /// visual row (nothing to do — the caller should fall through to
    /// history recall instead).
    pub fn move_cursor_up(&mut self, width: usize) -> bool {
        self.move_cursor_vertically(width, true)
    }

    /// Move the cursor down one visual (wrapped) row. Returns `false` when
    /// already on the last visual row.
    pub fn move_cursor_down(&mut self, width: usize) -> bool {
        self.move_cursor_vertically(width, false)
    }

    fn move_cursor_vertically(&mut self, width: usize, up: bool) -> bool {
        let width = (width as u16).max(1);
        let (start_row, start_col, total_rows) = visual_row_col(self, width, self.cursor);
        if up {
            if start_row == 0 {
                return false;
            }
        } else if start_row + 1 >= total_rows {
            return false;
        }
        let target_row = if up { start_row - 1 } else { start_row + 1 };
        let original_cursor = self.cursor;

        // Step toward target_row using the existing (already-correct)
        // horizontal movement, which already handles pending-paste atomic
        // jumps — never lands the cursor inside a paste placeholder.
        loop {
            let before = self.cursor;
            if up {
                if self.cursor == 0 {
                    break;
                }
                self.move_left();
            } else {
                if self.cursor >= self.text.len() {
                    break;
                }
                self.move_right();
            }
            if self.cursor == before {
                break;
            }
            let (row, _, _) = visual_row_col(self, width, self.cursor);
            if row == target_row {
                break;
            }
            // Adjacent-row target should never be overshot, but guard
            // against an infinite loop if it somehow is.
            if (up && row < target_row) || (!up && row > target_row) {
                break;
            }
        }

        // Approximate the original column within target_row. Walking left
        // (up) naturally lands on the row's *last* column first (approached
        // from the row below), so it needs to walk back left toward
        // start_col; walking right (down) lands on the row's *first* column
        // first, so it advances right toward start_col.
        loop {
            let (row, col, _) = visual_row_col(self, width, self.cursor);
            let done = if up {
                row != target_row || col <= start_col
            } else {
                row != target_row || col >= start_col
            };
            if done {
                break;
            }
            let before = self.cursor;
            if up {
                self.move_left();
            } else {
                self.move_right();
            }
            if self.cursor == before {
                break;
            }
            let (row2, _, _) = visual_row_col(self, width, self.cursor);
            if row2 != target_row {
                self.cursor = before;
                break;
            }
        }

        self.cursor != original_cursor
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.history_browse = false;
        self.pending_pastes.clear();
    }

    pub fn take(&mut self) -> String {
        let mut t = std::mem::take(&mut self.text);
        self.pending_pastes
            .sort_by_key(|paste| std::cmp::Reverse(paste.range.start));
        for paste in self.pending_pastes.drain(..) {
            t.replace_range(paste.range, &paste.content);
        }
        self.cursor = 0;
        self.history_browse = false;
        t
    }

    /// Replace buffer (e.g. from history recall); cursor moves to end.
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.text.len();
        self.pending_pastes.clear();
    }

    /// Number of visual lines for layout (capped).
    pub fn visual_lines(&self) -> u16 {
        let n = self.text.lines().count().max(1) as u16;
        n.min(MAX_VISIBLE_ROWS as u16)
    }

    /// Wrapped visual row count for a known composer width.
    pub fn visual_lines_for_width(&self, content_width: usize) -> u16 {
        Paragraph::new(composer_text(self, true))
            .wrap(Wrap { trim: false })
            .line_count(content_width.max(1) as u16)
            .clamp(1, MAX_VISIBLE_ROWS) as u16
    }

    /// Copyable buffer text — excludes decorative gutter presentation.
    pub fn copy_text(&self) -> &str {
        &self.text
    }

    fn insert_str(&mut self, text: &str) {
        let i = self.insertion_cursor();
        self.shift_ranges_for_insert(i, text.len());
        self.text.insert_str(i, text);
        self.cursor = i + text.len();
    }

    fn insertion_cursor(&self) -> usize {
        let cursor = self.cursor.min(self.text.len());
        self.pending_pastes
            .iter()
            .find(|paste| cursor > paste.range.start && cursor < paste.range.end)
            .map_or(cursor, |paste| paste.range.end)
    }

    fn shift_ranges_for_insert(&mut self, at: usize, inserted: usize) {
        for paste in &mut self.pending_pastes {
            if paste.range.start >= at {
                paste.range.start += inserted;
                paste.range.end += inserted;
            }
        }
    }

    fn shift_ranges_after_remove(&mut self, removed_end: usize, removed: usize) {
        for paste in &mut self.pending_pastes {
            if paste.range.start >= removed_end {
                paste.range.start -= removed;
                paste.range.end -= removed;
            }
        }
    }

    fn next_large_paste_placeholder(&self, char_count: usize) -> String {
        let base = format!("[Pasted Content {char_count} chars]");
        let prefix = format!("{base} #");
        let mut max_suffix = 0usize;
        for paste in &self.pending_pastes {
            if paste.placeholder == base {
                max_suffix = max_suffix.max(1);
            } else if let Some(suffix) = paste.placeholder.strip_prefix(&prefix) {
                if let Ok(value) = suffix.parse::<usize>() {
                    max_suffix = max_suffix.max(value);
                }
            }
        }
        if max_suffix == 0 {
            base
        } else {
            format!("{base} #{}", max_suffix + 1)
        }
    }
}

fn normalize_pasted_text(pasted: &str) -> String {
    let mut normalized = String::with_capacity(pasted.len());
    let mut chars = pasted.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                normalized.push('\n');
            }
            '\n' | '\t' => normalized.push(c),
            _ if !c.is_control() => normalized.push(c),
            _ => {}
        }
    }
    normalized
}

pub struct InputBar<'a> {
    pub model: &'a InputModel,
    /// Optional file-attachment label shown above the prompt line.
    pub attachment: Option<&'a str>,
    pub dimmed: bool,
    pub focused: bool,
}

fn composer_text(model: &InputModel, show_cursor: bool) -> Text<'_> {
    composer_text_at(model, show_cursor, model.cursor)
}

fn composer_text_at(model: &InputModel, show_cursor: bool, offset: usize) -> Text<'_> {
    if model.text.is_empty() && model.waiting {
        return Text::default();
    }
    let source = if model.text.is_empty() {
        &model.hint
    } else {
        &model.text
    };
    let offset = if model.text.is_empty() {
        0
    } else {
        offset.min(source.len())
    };
    let mut text = Text::raw(source.as_str());
    if !show_cursor {
        return text;
    }
    let prefix = &source[..offset];
    let row = if offset == source.len() {
        // Text has already parsed these lines. Recounting every byte makes
        // normal typing and result insertion scan a long draft twice.
        text.lines
            .len()
            .saturating_sub(usize::from(!source.ends_with('\n')))
    } else {
        prefix.matches('\n').count()
    };
    while text.lines.len() <= row {
        text.lines.push(Line::default());
    }
    if offset == source.len() {
        // Keep the parsed source spans and append only the EOF caret.
        text.lines[row]
            .spans
            .push(Span::styled(" ", Style::default().bg(CURSOR_MARK)));
        return text;
    }
    let start = prefix.rfind('\n').map_or(0, |index| index + 1);
    let line = source[start..].split('\n').next().unwrap_or_default();
    let column = offset - start;
    let mut caret_start = line.len();
    let mut length = 0;
    let mut bytes = 0;
    if column < line.len() {
        for grapheme in Span::raw(line).styled_graphemes(Style::default()) {
            if column < bytes + grapheme.symbol.len() {
                caret_start = bytes;
                length = grapheme.symbol.len();
                break;
            }
            bytes += grapheme.symbol.len();
        }
    }
    let (before, after) = line.split_at(caret_start);
    let glyph = if length == 0 { " " } else { &after[..length] };
    text.lines[row] = Line::from(vec![
        Span::raw(before),
        Span::styled(glyph, Style::default().bg(CURSOR_MARK)),
        Span::raw(&after[length..]),
    ]);
    text
}

fn cursor_scroll(model: &InputModel, content_width: u16, visible_rows: u16) -> u16 {
    // Typing and result handoff usually leave the caret at the end. Counting
    // those lines avoids materializing a full draft-sized probe buffer.
    if model.cursor >= model.text.len() {
        return Paragraph::new(composer_text(model, true))
            .wrap(Wrap { trim: false })
            .line_count(content_width.max(1))
            .saturating_sub(visible_rows as usize) as u16;
    }
    let (row, ..) = visual_row_col(model, content_width.max(1), model.cursor);
    row.saturating_add(1).saturating_sub(visible_rows)
}

/// `(row, col, total_rows)` of `offset` in `model.text`'s full, unscrolled
/// wrapped layout at `width` — independent of the current viewport/scroll
/// (unlike [`cursor_scroll`], which is about keeping a position visible in
/// a *limited* window). Used to drive [`InputModel::move_cursor_up`]/
/// [`InputModel::move_cursor_down`]: uses the same caret probe style used
/// for display at `offset` and renders through the real `Paragraph` +
/// `Wrap`, so results are pixel-identical to what's on screen rather than
/// reimplementing word-wrap in string space.
fn visual_row_col(model: &InputModel, width: u16, offset: usize) -> (u16, u16, u16) {
    let width = width.max(1);
    let probe_text = composer_text_at(model, true, offset);
    let total_rows = Paragraph::new(probe_text.clone())
        .wrap(Wrap { trim: false })
        .line_count(width)
        .max(1) as u16;
    let scratch_area = Rect::new(0, 0, width, total_rows);
    let mut scratch = Buffer::empty(scratch_area);
    Paragraph::new(probe_text)
        .wrap(Wrap { trim: false })
        .render(scratch_area, &mut scratch);
    for y in 0..total_rows {
        for x in 0..width {
            if scratch[(x, y)].bg == CURSOR_MARK {
                return (y, x, total_rows);
            }
        }
    }
    (0, 0, total_rows)
}

/// Shared two-column text origin for the composer, transcript and dock.
pub(crate) const TEXT_INSET: u16 = crate::design::COMPOSER_PAD_X;

/// Composer geometry derived from `model`/`area`/`attachment` — no styling.
/// Shared by [`InputBar::render`] and [`composer_cursor_position`] so the two
/// never drift apart.
struct ComposerGeometry {
    text_area: Rect,
}

/// The composer has no separate chrome row.
pub(crate) const COMPOSER_RULE_H: u16 = 0;

fn composer_geometry(
    model: &InputModel,
    area: Rect,
    attachment: Option<&str>,
) -> Option<ComposerGeometry> {
    // Share the transcript's text origin; every row belongs to the draft.
    let text_w = area.width.saturating_sub(TEXT_INSET * 2);
    let text_h = area.height.saturating_sub(crate::design::COMPOSER_BORDER_H);
    if text_w == 0 || text_h == 0 {
        return None;
    }

    let attach_h = u16::from(attachment.is_some() && text_h > 1);
    let y = area
        .y
        .saturating_add(COMPOSER_RULE_H)
        .saturating_add(attach_h);
    let remain = text_h.saturating_sub(attach_h);
    let text_area_h = remain.max(1);
    let input_area = Rect::new(area.x, y, area.width, text_area_h);

    let raw_text_area = Rect::new(
        input_area.x.saturating_add(TEXT_INSET),
        input_area.y,
        input_area.width.saturating_sub(TEXT_INSET * 2),
        input_area.height,
    );

    // Vertically center content that's shorter than the box (the common
    // case: an idle/short-draft composer sitting in a box tall enough for
    // growth). Once content fills or exceeds the available rows, use the
    // full height instead so cursor navigation still has room to scroll.
    let total_lines = Paragraph::new(composer_text(model, true))
        .wrap(Wrap { trim: false })
        .line_count(raw_text_area.width.max(1));
    let visible_content_rows = (total_lines as u16).min(raw_text_area.height).max(1);
    let top_pad = raw_text_area.height.saturating_sub(visible_content_rows) / 2;
    let text_area = Rect::new(
        raw_text_area.x,
        raw_text_area.y.saturating_add(top_pad),
        raw_text_area.width,
        raw_text_area.height.saturating_sub(top_pad),
    );

    Some(ComposerGeometry { text_area })
}

/// Absolute `(x, y)` of the composer's cursor cell within `area`, for driving
/// the real terminal cursor (`Frame::set_cursor_position`). Renders the same
/// composer text used for display into a scratch buffer and scans for the
/// caret probe style — mirrors the display render exactly rather than
/// reimplementing wrap math independently.
pub fn composer_cursor_position(
    model: &InputModel,
    area: Rect,
    attachment: Option<&str>,
) -> Option<(u16, u16)> {
    let geometry = composer_geometry(model, area, attachment)?;
    let text_area = geometry.text_area;
    if text_area.width == 0 || text_area.height == 0 {
        return None;
    }

    let scroll = cursor_scroll(model, text_area.width, text_area.height);
    let mut scratch = Buffer::empty(text_area);
    Paragraph::new(composer_text(model, true))
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0))
        .render(text_area, &mut scratch);

    for y in text_area.top()..text_area.bottom() {
        for x in text_area.left()..text_area.right() {
            if scratch[(x, y)].bg == CURSOR_MARK {
                return Some((x, y));
            }
        }
    }
    None
}

/// Composer's actual rendered text-area width — for key handling (which runs
/// before the next render) to pass into
/// [`InputModel::move_cursor_up`]/[`InputModel::move_cursor_down`]. Reuses
/// [`composer_geometry`], the single source of truth `InputBar::render`
/// itself uses, rather than re-deriving the border/inset/attachment
/// arithmetic separately and risking drift.
pub fn composer_text_area_width(
    model: &InputModel,
    area: Rect,
    attachment: Option<&str>,
) -> Option<u16> {
    let geometry = composer_geometry(model, area, attachment)?;
    Some(geometry.text_area.width)
}

impl Widget for InputBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        // Placeholder text follows the universal empty-input-field
        // convention (italic/dim = "type here") rather than the emphasized
        // style real typed content gets — a bold placeholder reads as
        // content, not an invitation to type.
        let is_placeholder = self.model.text.is_empty() && !self.model.waiting;
        let base = if self.dimmed {
            theme::dim()
        } else if self.model.history_browse {
            theme::history_active()
        } else if is_placeholder {
            theme::composer_placeholder()
        } else {
            theme::composer_text()
        };
        let text_focused = self.focused;
        let surface = if self.dimmed {
            theme::surface_hover()
        } else {
            theme::composer_surface()
        };
        Block::default().style(surface).render(area, buf);
        let text_zone = Rect::new(
            area.x,
            area.y.saturating_add(COMPOSER_RULE_H),
            area.width,
            area.height.saturating_sub(crate::design::COMPOSER_BORDER_H),
        );

        let Some(geometry) = composer_geometry(self.model, area, self.attachment) else {
            return;
        };
        let text_area = geometry.text_area;

        if self.attachment.is_some() && text_zone.height > 1 {
            let att_text = self.attachment.unwrap_or("");
            let att_line = Line::from(vec![
                Span::styled("» ", theme::info()),
                Span::styled(att_text, theme::info()),
                Span::styled("  [Ctrl+A or /cf to remove]", theme::dim()),
            ]);
            Paragraph::new(att_line).render(
                Rect::new(
                    text_zone.x.saturating_add(TEXT_INSET),
                    text_zone.y,
                    text_zone.width.saturating_sub(TEXT_INSET * 2),
                    1,
                ),
                buf,
            );
        }

        let scroll = if text_focused {
            cursor_scroll(self.model, text_area.width, text_area.height)
        } else {
            0
        };
        Paragraph::new(composer_text(self.model, text_focused))
            .style(base.add_modifier(if self.model.dimmed {
                Modifier::DIM
            } else {
                Modifier::empty()
            }))
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0))
            .render(text_area, buf);
        if text_focused {
            // Paint the same solid block caret used by every other input.
            for y in text_area.top()..text_area.bottom() {
                for x in text_area.left()..text_area.right() {
                    if buf[(x, y)].bg == CURSOR_MARK {
                        buf[(x, y)].set_style(theme::caret());
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_and_short_draft_have_balanced_vertical_padding() {
        for text in ["", "A short task"] {
            let model = InputModel {
                text: text.into(),
                hint: "Describe a task…".into(),
                ..Default::default()
            };
            let buf = draw_input_bar(&model, 40, 4, true, None);
            assert_eq!(
                composer_cursor_position(&model, *buf.area(), None),
                Some((TEXT_INSET, 1))
            );
            for y in [0, 3] {
                for x in 0..40 {
                    assert_eq!(buf[(x, y)].symbol(), " ");
                    assert_eq!(Some(buf[(x, y)].bg), theme::composer_surface().bg);
                }
            }
            assert_eq!(
                buf[(TEXT_INSET, 1)].symbol(),
                if text.is_empty() { "D" } else { "A" }
            );
        }
    }

    #[test]
    fn composer_has_no_prompt_label_or_separator_rule() {
        for (focused, waiting, not_connected, dimmed) in [
            (false, false, false, false),
            (true, false, false, false),
            (true, true, false, true),
            (false, false, true, false),
        ] {
            let model = InputModel {
                waiting,
                dimmed,
                not_connected,
                ..Default::default()
            };
            let buf = draw_input_bar(&model, 40, 3, focused, None);
            let surface = if dimmed {
                theme::surface_hover()
            } else {
                theme::composer_surface()
            };
            let row: String = (0..40).map(|x| buf[(x, 0)].symbol()).collect();
            assert!(!row.contains("Prompt") && !row.contains("Follow-up"));
            assert!(!row.contains('─') && !row.contains("---"));
            for x in 0..40 {
                assert_eq!(Some(buf[(x, 2)].bg), surface.bg);
            }
        }
    }

    #[test]
    fn composer_shows_only_the_draft_without_shortcut_hints() {
        for width in [40, 80, 120] {
            for (waiting, not_connected) in [(false, false), (true, false), (false, true)] {
                let model = InputModel {
                    text: "Keep my draft".into(),
                    cursor: 5,
                    waiting,
                    not_connected,
                    ..Default::default()
                };
                let buf = draw_input_bar(&model, width, 3, true, None);
                let visible: String = buf.content.iter().map(|cell| cell.symbol()).collect();
                assert!(visible.contains("Keep my draft"));
                for hint in [
                    "Enter send",
                    "Enter queue",
                    "Shift+Enter",
                    "Answer above",
                    "/connect to send",
                ] {
                    assert!(!visible.contains(hint), "{visible}");
                }
                assert_eq!(model.text, "Keep my draft");
                assert_eq!(model.cursor, 5);
            }
        }
    }

    #[test]
    fn focused_caret_keeps_draft_cells_and_literal_block_characters() {
        let area = Rect::new(0, 0, 80, 6);
        for (text, cursor) in [
            ("Retained parent draft.", 7),
            ("Retained λ/東京.rs █ marker", "Retained λ/".len()),
            ("Literal █ stays in the draft", "Literal █ stays ".len()),
            ("Combining e\u{301} stays intact", "Combining e".len()),
            ("Last paragraph\n\n", "Last paragraph\n\n".len()),
        ] {
            let model = InputModel {
                text: text.into(),
                cursor,
                ..Default::default()
            };
            let render = |focused| {
                let mut buffer = Buffer::empty(area);
                InputBar {
                    model: &model,
                    attachment: None,
                    dimmed: false,
                    focused,
                }
                .render(area, &mut buffer);
                buffer
                    .content
                    .iter()
                    .skip(area.width as usize)
                    .map(|cell| cell.symbol())
                    .collect::<String>()
            };
            assert_eq!(
                render(true),
                render(false),
                "caret changed the displayed draft: {text}"
            );
            assert!(composer_cursor_position(&model, area, None).is_some());
            assert_eq!(model.text, text);
            assert_eq!(model.cursor, cursor);
        }
    }

    #[test]
    fn append_handoff_keeps_draft_and_expands_pending_pastes() {
        let payload = "source ".repeat(200);
        let mut input = InputModel::default();
        input.insert_paste(&payload);
        input.cursor = 0;
        input.append_paste("Inspect this result before sending.");
        assert_eq!(
            input.take(),
            format!("{payload}\nInspect this result before sending.")
        );
    }

    #[test]
    fn empty_handoff_does_not_move_or_replace_the_draft() {
        let mut input = InputModel::default();
        input.insert_paste("existing draft");
        input.cursor = 3;
        input.append_paste("  \n");
        assert_eq!(input.text, "existing draft");
        assert_eq!(input.cursor, 3);
    }

    #[test]
    fn append_result_keeps_pending_pastes_and_leaves_long_evidence_editable() {
        let payload = "source ".repeat(200);
        let result = "evidence ✓ ".repeat(200);
        let mut input = InputModel::default();
        input.insert_paste(&payload);
        input.cursor = 0;
        input.append_text(&result);
        assert!(input.text.ends_with(&result));
        assert_eq!(input.cursor, input.text.len());
        assert_eq!(input.take(), format!("{payload}\n{result}"));
    }

    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    /// The composer has no leading marker at all (see `TEXT_INSET`) — this
    /// only guards against a stray `|` leaking into stored/copied text from
    /// paste/backspace/take logic, not an actual rendered glyph.
    fn glyph() -> &'static str {
        "|"
    }

    fn draw_input_bar(
        model: &InputModel,
        width: u16,
        height: u16,
        focused: bool,
        attachment: Option<&str>,
    ) -> ratatui::buffer::Buffer {
        let backend = TestBackend::new(width, height);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            f.render_widget(
                InputBar {
                    model,
                    attachment,
                    dimmed: model.dimmed,
                    focused,
                },
                f.area(),
            );
        })
        .unwrap();
        term.backend().buffer().clone()
    }

    fn render_lines(model: &InputModel, width: u16, height: u16, focused: bool) -> Vec<String> {
        let buf = draw_input_bar(model, width, height, focused, None);
        // Text zone excludes both borders and the shared horizontal inset.
        let inner = Rect::new(
            TEXT_INSET,
            COMPOSER_RULE_H,
            buf.area().width.saturating_sub(TEXT_INSET * 2),
            buf.area()
                .height
                .saturating_sub(crate::design::COMPOSER_BORDER_H),
        );
        (0..inner.height)
            .map(|y| {
                (0..inner.width)
                    .map(|x| buf[(inner.x + x, inner.y + y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .filter(|row| !row.is_empty())
            .collect()
    }

    #[test]
    fn insert_and_backspace() {
        let mut m = InputModel::default();
        m.insert('a');
        m.insert('b');
        assert_eq!(m.text, "ab");
        m.backspace();
        assert_eq!(m.text, "a");
        assert_eq!(m.cursor, 1);
    }

    #[test]
    fn newline_insert() {
        let mut m = InputModel::default();
        m.insert('a');
        m.insert_newline();
        m.insert('b');
        assert_eq!(m.text, "a\nb");
        assert_eq!(m.visual_lines(), 2);
    }

    #[test]
    fn cursor_moves() {
        let mut m = InputModel {
            text: "hi".into(),
            cursor: 2,
            ..Default::default()
        };
        m.move_left();
        assert_eq!(m.cursor, 1);
        m.insert('X');
        assert_eq!(m.text, "hXi");
    }

    #[test]
    fn move_cursor_vertically_no_op_on_single_line() {
        let mut m = InputModel {
            text: "hello".into(),
            cursor: 3,
            ..Default::default()
        };
        assert!(!m.move_cursor_up(40));
        assert_eq!(m.cursor, 3);
        assert!(!m.move_cursor_down(40));
        assert_eq!(m.cursor, 3);
    }

    #[test]
    fn move_cursor_vertically_across_explicit_newlines_preserves_column() {
        // "one\ntwo\nthree" — cursor starts at offset 5 ("tw|o", col 1).
        let mut m = InputModel {
            text: "one\ntwo\nthree".into(),
            cursor: 5,
            ..Default::default()
        };
        assert!(m.move_cursor_up(40));
        assert_eq!(m.cursor, 1, "should land on col 1 of \"one\"");
        assert!(m.move_cursor_down(40));
        assert_eq!(m.cursor, 5, "should return to col 1 of \"two\"");
        assert!(m.move_cursor_down(40));
        assert_eq!(m.cursor, 9, "should advance to col 1 of \"three\"");
        assert!(!m.move_cursor_down(40), "already on the last line");
        assert_eq!(m.cursor, 9);
    }

    #[test]
    fn move_cursor_up_returns_false_on_first_line() {
        let mut m = InputModel {
            text: "one\ntwo".into(),
            cursor: 1,
            ..Default::default()
        };
        assert!(!m.move_cursor_up(40));
        assert_eq!(m.cursor, 1);
    }

    #[test]
    fn move_cursor_down_clamps_column_to_a_shorter_target_line() {
        let mut m = InputModel {
            text: "hello\nhi".into(),
            cursor: 5,
            ..Default::default()
        };
        assert!(m.move_cursor_down(40));
        assert_eq!(
            m.cursor, 8,
            "should clamp to the end of the shorter \"hi\" line"
        );
    }

    #[test]
    fn move_cursor_vertically_wraps_within_a_single_logical_line() {
        let mut m = InputModel::default();
        m.set_text("word ".repeat(30).trim().to_string());
        let width = 20u16;
        let (start_row, _, total_rows) = visual_row_col(&m, width, m.cursor);
        assert!(total_rows > 1, "expected the text to wrap to multiple rows");
        assert_eq!(
            start_row,
            total_rows - 1,
            "set_text moves cursor to the end"
        );
        assert!(m.move_cursor_up(width as usize));
        let (row_after, _, _) = visual_row_col(&m, width, m.cursor);
        assert!(
            row_after < start_row,
            "expected the cursor to move to an earlier wrapped row"
        );
    }

    #[test]
    fn move_cursor_vertically_never_lands_inside_a_pending_paste() {
        let mut m = InputModel::default();
        m.insert_str("before\n");
        m.insert_paste(&"x".repeat(LARGE_PASTE_CHAR_THRESHOLD + 1));
        m.insert_str("\nafter");
        let paste_range = m.pending_pastes[0].range.clone();
        let width = 10u16;
        for _ in 0..10 {
            if !m.move_cursor_up(width as usize) {
                break;
            }
            assert!(
                m.cursor <= paste_range.start || m.cursor >= paste_range.end,
                "cursor landed inside the paste placeholder: {}",
                m.cursor
            );
        }
    }

    #[test]
    fn take_clears() {
        let mut m = InputModel {
            text: "cmd".into(),
            cursor: 3,
            ..Default::default()
        };
        assert_eq!(m.take(), "cmd");
        assert!(m.text.is_empty());
    }

    #[test]
    fn large_paste_uses_placeholder_and_expands_on_take() {
        let pasted = "λ".repeat(LARGE_PASTE_CHAR_THRESHOLD + 1);
        let mut m = InputModel::default();
        m.insert_paste(&pasted);
        assert_eq!(m.text, "[Pasted Content 1001 chars]");
        assert_eq!(m.visual_lines(), 1);
        assert_eq!(m.take(), pasted);
        assert!(m.text.is_empty());
        assert!(!pasted.contains(glyph()));
    }

    #[test]
    fn large_paste_placeholder_is_atomic_for_cursor_and_backspace() {
        let mut m = InputModel::default();
        m.insert_paste(&"x".repeat(LARGE_PASTE_CHAR_THRESHOLD + 1));
        let end = m.cursor;
        m.move_left();
        assert_eq!(m.cursor, 0);
        m.move_right();
        assert_eq!(m.cursor, end);
        m.backspace();
        assert!(m.text.is_empty());
        assert!(m.pending_pastes.is_empty());
    }

    #[test]
    fn duplicate_length_pastes_get_unique_placeholders() {
        let pasted = "x".repeat(LARGE_PASTE_CHAR_THRESHOLD + 1);
        let base = "[Pasted Content 1001 chars]";
        let mut m = InputModel::default();
        m.insert_paste(&pasted);
        m.insert_paste(&pasted);
        assert_eq!(m.text, format!("{base}{base} #2"));
        assert_eq!(m.take(), format!("{pasted}{pasted}"));
    }

    #[test]
    fn surrounding_edits_preserve_large_paste_expansion() {
        let pasted = "x".repeat(LARGE_PASTE_CHAR_THRESHOLD + 1);
        let mut m = InputModel::default();
        m.insert_paste(&pasted);
        m.insert('!');
        m.move_left();
        m.move_left();
        m.insert('>');
        assert_eq!(m.take(), format!(">{pasted}!"));
    }

    #[test]
    fn small_paste_is_bulk_inserted_and_normalized() {
        let mut m = InputModel::default();
        m.set_text("before after");
        m.cursor = "before".len();
        m.insert_paste("\r\n\tmiddle\u{0000}");
        assert_eq!(m.text, "before\n\tmiddle after");
        assert_eq!(m.take(), "before\n\tmiddle after");
    }

    #[test]
    fn copy_entire_buffer_excludes_gutter() {
        let text = "Explain how session recovery works";
        let m = InputModel {
            text: text.into(),
            ..Default::default()
        };
        assert_eq!(m.copy_text(), text);
    }

    #[test]
    fn copy_text_never_includes_the_rendered_gutter() {
        let model = InputModel {
            text: "alpha beta gamma".into(),
            ..Default::default()
        };
        assert!(!model.copy_text().contains(glyph()));
    }

    #[test]
    fn history_recall_preserves_buffer() {
        let text = "line1\nline2";
        let mut m = InputModel::default();
        m.set_text(text);
        let raw_rows = render_lines(&m, 60, 8, false);
        let rows: Vec<&str> = raw_rows.iter().map(|row| row.trim_start()).collect();
        assert_eq!(rows, vec!["line1", "line2"]);
        assert_eq!(m.copy_text(), text);
    }

    #[test]
    fn submission_take_excludes_gutter() {
        let mut m = InputModel {
            text: "multi\nline".into(),
            ..Default::default()
        };
        let submitted = m.take();
        assert_eq!(submitted, "multi\nline");
        assert!(!submitted.contains(glyph()));
    }
}
