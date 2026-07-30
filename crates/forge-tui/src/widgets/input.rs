//! Input bar — multi-line paste / Shift+Enter newline, visible caret.

use crate::composer_layout::{build_visual_rows, locate_cursor, scroll_offset, visual_row_count};
use crate::theme;
use crate::user_message_gutter::{gutter_glyph, gutter_prefix_width, GutterRole, GUTTER_GAP};
use forge_config::Theme;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};
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
        visual_row_count(&self.text, content_width.max(1))
            .min(MAX_VISIBLE_ROWS)
            .max(1) as u16
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
    pub focused: bool,
}

fn render_content_spans(
    fragment: &str,
    cursor_in_row: Option<usize>,
    base: Style,
    focused: bool,
) -> Vec<Span<'static>> {
    let Some(col) = cursor_in_row.filter(|_| focused) else {
        return vec![Span::styled(fragment.to_string(), base)];
    };
    let col = col.min(fragment.len());
    let (left, right) = fragment.split_at(col);
    if right.is_empty() {
        return vec![
            Span::styled(left.to_string(), base),
            Span::styled(" ", theme::caret()),
        ];
    }
    let ch = right.chars().next().unwrap();
    let n = ch.len_utf8();
    let under = &right[..n];
    let under_disp = if under == " " { " " } else { under };
    vec![
        Span::styled(left.to_string(), base),
        Span::styled(under_disp.to_string(), theme::caret()),
        Span::styled(right[n..].to_string(), base),
    ]
}

fn build_input_lines(
    model: &InputModel,
    content_width: usize,
    visible_rows: usize,
    focused: bool,
    base: Style,
    gutter_style: Style,
    theme: Theme,
    force_fallback: bool,
) -> Vec<Line<'static>> {
    let glyph = gutter_glyph(theme, force_fallback);
    let text = &model.text;
    let cursor = model.cursor.min(text.len());

    if text.is_empty() {
        let mut spans = vec![
            Span::styled(glyph, gutter_style),
            Span::styled(GUTTER_GAP, base),
        ];
        if focused {
            spans.push(Span::styled(" ", theme::caret()));
        }
        if !model.hint.is_empty() {
            spans.push(Span::styled(model.hint.clone(), theme::dim()));
        }
        return vec![Line::from(spans)];
    }

    let rows = build_visual_rows(text, content_width);
    let (cursor_row, cursor_col) = locate_cursor(text, cursor, content_width);
    let offset = scroll_offset(cursor_row, rows.len(), visible_rows);
    let end = (offset + visible_rows).min(rows.len());

    rows[offset..end]
        .iter()
        .enumerate()
        .map(|(visible_idx, row)| {
            let row_idx = offset + visible_idx;
            let cursor_in_row = (row_idx == cursor_row).then_some(cursor_col);
            let mut spans = vec![
                Span::styled(glyph, gutter_style),
                Span::styled(GUTTER_GAP, base),
            ];
            spans.extend(render_content_spans(
                &row.fragment,
                cursor_in_row,
                base,
                focused,
            ));
            Line::from(spans)
        })
        .collect()
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

        let base = if self.model.dimmed {
            theme::dim()
        } else if self.model.history_browse {
            theme::history_active()
        } else {
            theme::text()
        };
        let theme = Theme::default();
        let gutter_style = crate::user_message_gutter::gutter_style_for(theme, GutterRole::Active);

        let glyph = gutter_glyph(theme, false);
        let prefix_width = gutter_prefix_width(glyph);
        let inner_width = lines_area.width as usize;
        let content_width = inner_width.saturating_sub(prefix_width).max(1);
        let visible_rows = lines_area.height.max(1) as usize;
        let lines = build_input_lines(
            self.model,
            content_width,
            visible_rows,
            self.focused,
            base,
            gutter_style,
            theme,
            false,
        );

        let border = if self.focused {
            theme::brand()
        } else if self.model.not_connected {
            theme::warn()
        } else {
            theme::border()
        };
        let block = Block::default()
            .borders(Borders::TOP)
            .border_style(border)
            .style(if self.model.history_browse {
                theme::panel_alt()
            } else {
                theme::panel()
            });
        Paragraph::new(lines)
            .style(Style::default().add_modifier(if self.model.dimmed {
                Modifier::DIM
            } else {
                Modifier::empty()
            }))
            .block(block)
            .render(lines_area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composer_layout::{click_to_cursor, copy_buffer, strip_rendered_prefix};
    use crate::theme::{USER_GUTTER_ACTIVE_DARK, USER_MESSAGE_GUTTER_DARK};
    use crate::user_message_gutter::{
        gutter_glyph, gutter_prefix_width, gutter_style_for, GutterRole, GUTTER_GAP,
    };
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn glyph() -> &'static str {
        gutter_glyph(Theme::Dark, false)
    }

    fn line_plain(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn render_lines(model: &InputModel, width: u16, height: u16, focused: bool) -> Vec<String> {
        let backend = TestBackend::new(width, height);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            f.render_widget(
                InputBar {
                    model,
                    attachment: None,
                    focused,
                },
                f.area(),
            );
        })
        .unwrap();
        let buf = term.backend().buffer();
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
    fn caret_cell_has_background() {
        let mut m = InputModel::default();
        m.set_text("ab");
        m.cursor = 2;
        let backend = TestBackend::new(40, 5);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            f.render_widget(
                InputBar {
                    model: &m,
                    attachment: None,
                    focused: true,
                },
                f.area(),
            );
        })
        .unwrap();
        let buf = term.backend().buffer();
        let area = buf.area();
        let mut found = false;
        for y in 0..area.height {
            for x in 0..area.width {
                let cell = &buf[(x, y)];
                if cell.style().bg == Some(theme::TEXT) {
                    found = true;
                }
            }
        }
        assert!(found, "expected block cursor cell with solid background");
    }

    #[test]
    fn empty_input_starts_with_caret_cell() {
        let m = InputModel::default();
        let backend = TestBackend::new(40, 5);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            f.render_widget(
                InputBar {
                    model: &m,
                    attachment: None,
                    focused: true,
                },
                f.area(),
            );
        })
        .unwrap();
        let buf = term.backend().buffer();
        let cell = &buf[(2, 1)];
        assert_eq!(cell.symbol(), " ");
        assert_eq!(cell.style().bg, Some(theme::TEXT));
    }

    #[test]
    fn mid_line_block_cursor_inverts_char() {
        let mut m = InputModel::default();
        m.set_text("ab");
        m.cursor = 0;
        let backend = TestBackend::new(40, 5);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            f.render_widget(
                InputBar {
                    model: &m,
                    attachment: None,
                    focused: true,
                },
                f.area(),
            );
        })
        .unwrap();
        let buf = term.backend().buffer();
        let area = buf.area();
        let mut found_a = false;
        for y in 0..area.height {
            for x in 0..area.width {
                let cell = &buf[(x, y)];
                if cell.symbol() == "a" && cell.style().bg == Some(theme::TEXT) {
                    found_a = true;
                }
            }
        }
        assert!(
            found_a,
            "expected inverted block cursor on character under caret"
        );
    }

    #[test]
    fn renders_mode_label_and_connection_hint() {
        let m = InputModel {
            not_connected: true,
            hint: "type here".into(),
            ..Default::default()
        };
        let backend = TestBackend::new(48, 5);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            f.render_widget(
                InputBar {
                    model: &m,
                    attachment: None,
                    focused: false,
                },
                f.area(),
            );
        })
        .unwrap();
        let buf = term.backend().buffer();
        let border = &buf[(0, 0)];
        assert_eq!(border.style().fg, Some(theme::WARN));
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
        let backend = TestBackend::new(48, 5);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            f.render_widget(
                InputBar {
                    model: &m,
                    attachment: Some("file.txt"),
                    focused: true,
                },
                f.area(),
            );
        })
        .unwrap();
        let buf = term.backend().buffer();
        let mut saw_history_bg = false;
        for y in 0..buf.area().height {
            for x in 0..buf.area().width {
                if buf[(x, y)].style().bg == Some(theme::HISTORY_BG) {
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
        assert_eq!(rendered.matches(glyph()).count(), 2);
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
        assert!(rows[0].starts_with(&format!("{} Summarize", glyph())));
        assert_eq!(m.copy_text(), "Summarize this codebase");
    }

    #[test]
    fn wrapped_input_has_gutter_on_every_row() {
        let m = InputModel {
            text: "word ".repeat(30).trim().to_string(),
            cursor: 0,
            ..Default::default()
        };
        let rows = render_lines(&m, 24, 8, true);
        assert!(rows.len() >= 3);
        assert!(rows.iter().all(|row| row.starts_with(glyph())));
    }

    #[test]
    fn explicit_newlines_each_have_gutter() {
        let m = InputModel {
            text: "one\ntwo\nthree".into(),
            ..Default::default()
        };
        let rows = render_lines(&m, 60, 8, true);
        assert_eq!(
            rows.iter().filter(|row| row.starts_with(glyph())).count(),
            3
        );
    }

    #[test]
    fn blank_line_renders_gutter_row() {
        let m = InputModel {
            text: "First.\n\nSecond.".into(),
            ..Default::default()
        };
        let rows = render_lines(&m, 60, 8, true);
        assert_eq!(rows.len(), 3);
        assert!(rows[1].starts_with(glyph()));
    }

    #[test]
    fn active_gutter_uses_distinct_theme_from_submitted() {
        let style = gutter_style_for(Theme::Dark, GutterRole::Active);
        assert_eq!(style.fg, Some(USER_GUTTER_ACTIVE_DARK));
        assert_ne!(style.fg, Some(USER_MESSAGE_GUTTER_DARK));
    }

    #[test]
    fn copy_entire_buffer_excludes_gutter() {
        let text = "Explain how session recovery works";
        let m = InputModel {
            text: text.into(),
            ..Default::default()
        };
        assert_eq!(m.copy_text(), text);
        assert_eq!(copy_buffer(m.copy_text()), text);
    }

    #[test]
    fn partial_copy_strips_rendered_prefix() {
        let rows = render_lines(
            &InputModel {
                text: "alpha beta gamma".into(),
                ..Default::default()
            },
            12,
            8,
            true,
        );
        let copied = rows
            .iter()
            .map(|row| strip_rendered_prefix(row, glyph()).to_string())
            .collect::<Vec<_>>()
            .join("");
        assert!(!copied.contains(glyph()));
    }

    #[test]
    fn mouse_click_on_gutter_clamps_to_start() {
        let text = "hello";
        assert_eq!(click_to_cursor(text, 0, 0, glyph(), 40), 0);
    }

    #[test]
    fn history_recall_preserves_buffer_without_gutter() {
        let text = "line1\nline2";
        let mut m = InputModel::default();
        m.set_text(text);
        let rows = render_lines(&m, 60, 8, true);
        assert_eq!(
            rows.iter().filter(|row| row.starts_with(glyph())).count(),
            2
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
    fn forced_fallback_gutter_renders_on_all_rows() {
        let glyph = gutter_glyph(Theme::Ansi, false);
        assert_ne!(glyph, "▎");
        let lines = build_input_lines(
            &InputModel {
                text: "one two three four five".into(),
                ..Default::default()
            },
            10,
            6,
            true,
            theme::text(),
            gutter_style_for(Theme::Ansi, GutterRole::Active),
            Theme::Ansi,
            false,
        );
        assert!(lines.iter().all(|line| line_plain(line).starts_with(glyph)));
        assert!(gutter_prefix_width(glyph) >= 2);
    }
}
