//! Input bar — multi-line paste / Shift+Enter newline.

use crate::theme;
use crate::user_message_gutter::{gutter_prefix_width, GutterRole, ACTIVE_GLYPH};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget, Wrap};
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
    /// Full payloads represented by compact, atomic placeholders in `text`.
    pending_pastes: Vec<PendingPaste>,
}

const LARGE_PASTE_CHAR_THRESHOLD: usize = 1000;
const MAX_VISIBLE_ROWS: usize = 6;
const CURSOR_GLYPH: &str = "▏";

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
    pub not_connected: bool,
    pub focused: bool,
}

fn composer_text(model: &InputModel, show_cursor: bool) -> String {
    if model.text.is_empty() {
        return if show_cursor {
            format!("{CURSOR_GLYPH}{}", model.hint)
        } else {
            model.hint.clone()
        };
    }
    if !show_cursor {
        return model.text.clone();
    }

    let cursor = model.cursor.min(model.text.len());
    let (before, after) = model.text.split_at(cursor);
    format!("{before}{CURSOR_GLYPH}{after}")
}

fn cursor_scroll(model: &InputModel, content_width: u16, visible_rows: u16) -> u16 {
    let cursor = model.cursor.min(model.text.len());
    let prefix = format!("{}{}", &model.text[..cursor], CURSOR_GLYPH);
    Paragraph::new(prefix)
        .wrap(Wrap { trim: false })
        .line_count(content_width.max(1))
        .saturating_sub(visible_rows as usize) as u16
}

impl Widget for InputBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let lines_area = if self.attachment.is_some() && area.height > 1 {
            let rows = ratatui::layout::Layout::default()
                .direction(ratatui::layout::Direction::Vertical)
                .constraints([
                    ratatui::layout::Constraint::Length(1),
                    ratatui::layout::Constraint::Min(0),
                ])
                .split(area);
            let att_text = self.attachment.unwrap_or("");
            let att_line = Line::from(vec![
                Span::styled("» ", theme::info()),
                Span::styled(att_text, theme::info()),
                Span::styled("  [Ctrl+A or /cf to remove]", theme::dim()),
            ]);
            Paragraph::new(att_line).render(rows[0], buf);
            rows[1]
        } else {
            area
        };

        let base = if self.dimmed {
            theme::dim()
        } else if self.model.history_browse {
            theme::history_active()
        } else {
            theme::text()
        };
        let theme = crate::theme::active();
        let gutter_style = crate::user_message_gutter::gutter_style_for(&theme, GutterRole::Active);

        let border = if self.focused {
            theme::active_panel_border()
        } else if self.not_connected {
            theme::warn()
        } else {
            theme::inactive_panel_border()
        };
        let block = Block::default()
            .borders(Borders::TOP)
            .border_style(border)
            .style(if self.dimmed {
                theme::surface_hover()
            } else if self.focused || self.model.history_browse {
                theme::composer_focused()
            } else {
                theme::panel()
            });
        let input_area = block.inner(lines_area);
        block.render(lines_area, buf);
        if input_area.width == 0 || input_area.height == 0 {
            return;
        }

        Paragraph::new(Span::styled(ACTIVE_GLYPH, gutter_style))
            .render(Rect::new(input_area.x, input_area.y, 1, 1), buf);
        let prefix_width = gutter_prefix_width(ACTIVE_GLYPH) as u16;
        let text_area = Rect::new(
            input_area.x.saturating_add(prefix_width),
            input_area.y,
            input_area.width.saturating_sub(prefix_width),
            input_area.height,
        );
        let scroll = if self.focused {
            cursor_scroll(self.model, text_area.width, text_area.height)
        } else {
            0
        };
        Paragraph::new(composer_text(self.model, self.focused))
            .style(base.add_modifier(if self.model.dimmed {
                Modifier::DIM
            } else {
                Modifier::empty()
            }))
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0))
            .render(text_area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme;
    use crate::user_message_gutter::{gutter_prefix_width, gutter_style_for, GutterRole};
    use forge_config::THEME_SOLARIZED_DARK;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn glyph() -> &'static str {
        ACTIVE_GLYPH
    }

    fn draw_input_bar(
        model: &InputModel,
        width: u16,
        height: u16,
        focused: bool,
        not_connected: bool,
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
                    not_connected,
                    focused,
                },
                f.area(),
            );
        })
        .unwrap();
        term.backend().buffer().clone()
    }

    fn render_lines(model: &InputModel, width: u16, height: u16, focused: bool) -> Vec<String> {
        let buf = draw_input_bar(model, width, height, focused, model.not_connected, None);
        (1..buf.area().height)
            .map(|y| {
                (0..buf.area().width)
                    .map(|x| buf[(x, y)].symbol())
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
    fn caret_marker_is_visible() {
        let mut m = InputModel::default();
        m.set_text("ab");
        m.cursor = 2;
        let buf = draw_input_bar(&m, 40, 5, true, false, None);
        let area = buf.area();
        let mut found = false;
        for y in 0..area.height {
            for x in 0..area.width {
                let cell = &buf[(x, y)];
                if cell.symbol() == CURSOR_GLYPH {
                    found = true;
                }
            }
        }
        assert!(found, "expected visible cursor marker");
    }

    #[test]
    fn empty_input_starts_with_caret_cell() {
        let m = InputModel::default();
        let buf = draw_input_bar(&m, 40, 5, true, false, None);
        let cell = &buf[(2, 1)];
        assert_eq!(cell.symbol(), CURSOR_GLYPH);
    }

    #[test]
    fn mid_line_cursor_marker_precedes_the_character() {
        let mut m = InputModel::default();
        m.set_text("ab");
        m.cursor = 0;
        let buf = draw_input_bar(&m, 40, 5, true, false, None);
        let area = buf.area();
        let mut found_marker = false;
        for y in 0..area.height {
            for x in 0..area.width {
                let cell = &buf[(x, y)];
                if cell.symbol() == CURSOR_GLYPH {
                    found_marker = true;
                }
            }
        }
        assert!(
            found_marker,
            "expected visible cursor marker before the first character"
        );
    }

    #[test]
    fn renders_mode_label_and_connection_hint() {
        let m = InputModel {
            not_connected: true,
            hint: "type here".into(),
            ..Default::default()
        };
        let buf = draw_input_bar(&m, 48, 5, false, true, None);
        let border = &buf[(0, 0)];
        assert_eq!(
            border.style().fg,
            Some(theme::palette(THEME_SOLARIZED_DARK).warn)
        );
        let rendered: String = (0..buf.area().height)
            .map(|y| {
                (0..buf.area().width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("type here"));
        assert!(rendered.contains(glyph()));
    }

    #[test]
    fn renders_history_and_multiline_mode_indicators() {
        let m = InputModel {
            text: "line1\nline2".into(),
            history_browse: true,
            ..Default::default()
        };
        let buf = draw_input_bar(&m, 48, 5, true, false, Some("file.txt"));
        let mut saw_history_bg = false;
        for y in 0..buf.area().height {
            for x in 0..buf.area().width {
                if buf[(x, y)].style().bg == Some(theme::palette(&theme::active()).selection) {
                    saw_history_bg = true;
                }
            }
        }
        assert!(saw_history_bg);
        let rendered: String = (0..buf.area().height)
            .map(|y| {
                (0..buf.area().width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("file.txt"));
        // Non-repeating prompt marker: exactly one `>`, on the true first
        // row, not one per wrapped/continuation line.
        assert_eq!(rendered.matches(glyph()).count(), 1);
    }

    #[test]
    fn empty_composer_renders_placeholder_gutter() {
        let m = InputModel {
            hint: "Ask Forge anything…".into(),
            ..Default::default()
        };
        let rows = render_lines(&m, 60, 5, true);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].starts_with(glyph()));
        assert!(rows[0].contains("Ask Forge anything…"));
        assert!(m.copy_text().is_empty());
    }

    #[test]
    fn single_line_input_has_continuous_gutter() {
        let m = InputModel {
            text: "Summarize this codebase".into(),
            cursor: 0,
            ..Default::default()
        };
        let rows = render_lines(&m, 60, 5, true);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].starts_with(&format!("{} {CURSOR_GLYPH}Summarize", glyph())));
        assert_eq!(m.copy_text(), "Summarize this codebase");
    }

    #[test]
    fn wrapped_input_has_gutter_only_on_first_row() {
        let m = InputModel {
            text: "word ".repeat(30).trim().to_string(),
            cursor: 0,
            ..Default::default()
        };
        let rows = render_lines(&m, 24, 8, true);
        assert!(rows.len() >= 3);
        assert!(rows[0].starts_with(glyph()));
        // Continuation rows get blank padding of the same width, not a
        // repeated `>` — but text still lines up under the first row.
        let prefix_width = gutter_prefix_width(glyph());
        for row in &rows[1..] {
            assert!(!row.starts_with(glyph()), "continuation row: {row}");
            assert!(row.starts_with(&" ".repeat(prefix_width)), "row: {row}");
        }
    }

    #[test]
    fn cursor_marker_moves_after_a_wrapped_space() {
        let model = InputModel {
            text: "alpha beta gamma".into(),
            cursor: "alpha beta ".len(),
            ..Default::default()
        };
        assert_eq!(composer_text(&model, true), "alpha beta ▏gamma");
    }

    #[test]
    fn explicit_newlines_have_gutter_only_on_first_row() {
        let m = InputModel {
            text: "one\ntwo\nthree".into(),
            ..Default::default()
        };
        let rows = render_lines(&m, 60, 8, true);
        assert_eq!(
            rows.iter().filter(|row| row.starts_with(glyph())).count(),
            1
        );
        assert!(rows[0].starts_with(glyph()));
    }

    #[test]
    fn composer_text_preserves_blank_lines() {
        let model = InputModel {
            text: "First.\n\nSecond.".into(),
            ..Default::default()
        };
        assert_eq!(composer_text(&model, false), "First.\n\nSecond.");
    }

    #[test]
    fn active_gutter_uses_configured_theme_token() {
        let style = gutter_style_for(THEME_SOLARIZED_DARK, GutterRole::Active);
        let dark = theme::palette(THEME_SOLARIZED_DARK);
        assert_eq!(style.fg, Some(dark.user_gutter_active));
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
    fn history_recall_preserves_buffer_without_gutter() {
        let text = "line1\nline2";
        let mut m = InputModel::default();
        m.set_text(text);
        let rows = render_lines(&m, 60, 8, true);
        assert_eq!(
            rows.iter().filter(|row| row.starts_with(glyph())).count(),
            1
        );
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

    #[test]
    fn multiline_input_scrolls_cursor_into_view() {
        let m = InputModel {
            text: "line1\nline2\nline3\nline4\nline5".into(),
            cursor: "line1\nline2\nline3\nline4\nline5".len(),
            ..Default::default()
        };
        let rows = render_lines(&m, 60, 5, true);
        assert!(rows.iter().any(|row| row.contains("line5")));
        // The prompt gutter belongs to the compositor itself and remains
        // stable while Ratatui scrolls the paragraph content below it.
        assert!(rows.iter().any(|row| row.starts_with(glyph())));
    }
}
