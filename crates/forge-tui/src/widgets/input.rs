//! Input bar — multi-line paste / Shift+Enter newline, visible caret.

use crate::theme;
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
        n.min(6)
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
    pub mode_label: &'a str,
}

impl Widget for InputBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        // Reserve one row for the attachment indicator when set.
        let lines_area = if self.attachment.is_some() && area.height > 1 {
            // Render the attachment line at the top of the area.
            let (att_row, input_area) = {
                let rows = ratatui::layout::Layout::default()
                    .direction(ratatui::layout::Direction::Vertical)
                    .constraints([
                        ratatui::layout::Constraint::Length(1),
                        ratatui::layout::Constraint::Min(0),
                    ])
                    .split(area);
                (rows[0], rows[1])
            };
            let att_text = self.attachment.unwrap_or("");
            let att_line = Line::from(vec![
                Span::styled("» ", theme::info()),
                Span::styled(att_text, theme::info()),
                Span::styled("  [Ctrl+A or /cf to remove]", theme::dim()),
            ]);
            Paragraph::new(att_line).render(att_row, buf);
            input_area
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

        // Block cursor: solid cell at caret (█ at EOL, inverted char mid-text).
        let lines: Vec<Line> = if self.model.text.is_empty() && !self.model.hint.is_empty() {
            vec![Line::from(vec![
                Span::styled("› ", theme::brand()),
                Span::styled(" ", if self.focused { theme::caret() } else { base }),
                Span::styled(self.model.hint.as_str(), theme::dim()),
            ])]
        } else if self.model.text.is_empty() {
            vec![Line::from(vec![
                Span::styled("› ", theme::brand()),
                Span::styled(" ", if self.focused { theme::caret() } else { base }),
            ])]
        } else {
            let t = &self.model.text;
            let cur = self.model.cursor.min(t.len());
            let before = &t[..cur];
            let line_idx = before.matches('\n').count();
            let mut out = Vec::new();
            for (i, raw) in t.split('\n').enumerate() {
                let prefix = if i == 0 { "› " } else { "  " };
                if self.focused && i == line_idx {
                    let line_start = if i == 0 {
                        0
                    } else {
                        before.rfind('\n').map(|p| p + 1).unwrap_or(0)
                    };
                    let col = cur.saturating_sub(line_start).min(raw.len());
                    let (left, right) = raw.split_at(col);
                    if right.is_empty() {
                        // EOL: solid block cell
                        out.push(Line::from(vec![
                            Span::styled(prefix, theme::brand()),
                            Span::styled(left, base),
                            Span::styled(" ", theme::caret()),
                        ]));
                    } else {
                        // Mid-line: invert the character under the cursor (block cursor)
                        let ch = right.chars().next().unwrap();
                        let n = ch.len_utf8();
                        let under = &right[..n];
                        // Non-space under cursor stays readable; space becomes full block cell
                        let under_disp = if under == " " { " " } else { under };
                        out.push(Line::from(vec![
                            Span::styled(prefix, theme::brand()),
                            Span::styled(left, base),
                            Span::styled(under_disp, theme::caret()),
                            Span::styled(&right[n..], base),
                        ]));
                    }
                } else {
                    out.push(Line::from(vec![
                        Span::styled(prefix, theme::brand()),
                        Span::styled(raw, base),
                    ]));
                }
            }
            if self.focused && t.ends_with('\n') && cur == t.len() {
                out.push(Line::from(vec![
                    Span::styled("  ", theme::brand()),
                    Span::styled(" ", theme::caret()),
                ]));
            }
            out
        };

        let border = if self.focused {
            theme::brand()
        } else if self.model.not_connected {
            theme::warn()
        } else {
            theme::border()
        };
        let mut title = format!(" Chat · {} ", self.mode_label);
        if self.model.not_connected {
            title.push_str("· not connected /connect required ");
        } else if self.model.history_browse {
            title.push_str("· history ");
        } else if self.model.text.contains('\n') {
            title.push_str("· multi-line Shift+Enter newline ");
        }
        let block = Block::default()
            .borders(Borders::TOP)
            .border_style(border)
            .style(if self.model.history_browse {
                theme::panel_alt()
            } else {
                theme::panel()
            });
        let block = block.title(Span::styled(
            title,
            if self.focused {
                theme::brand()
            } else {
                theme::muted()
            },
        ));
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
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

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
        m.cursor = 2; // EOL — block cell after text
        let backend = TestBackend::new(40, 5);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            f.render_widget(
                InputBar {
                    model: &m,
                    attachment: None,
                    focused: true,
                    mode_label: "INPUT",
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
                    mode_label: "INPUT",
                },
                f.area(),
            );
        })
        .unwrap();
        let buf = term.backend().buffer();
        // Empty input renders prompt first, then the caret cell.
        let cell = &buf[(2, 1)];
        assert_eq!(cell.symbol(), " ");
        assert_eq!(cell.style().bg, Some(theme::TEXT));
    }

    #[test]
    fn mid_line_block_cursor_inverts_char() {
        let mut m = InputModel::default();
        m.set_text("ab");
        m.cursor = 0; // over 'a'
        let backend = TestBackend::new(40, 5);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            f.render_widget(
                InputBar {
                    model: &m,
                    attachment: None,
                    focused: true,
                    mode_label: "INPUT",
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
                    mode_label: "NAV",
                },
                f.area(),
            );
        })
        .unwrap();
        let buf = term.backend().buffer();
        let rendered: String = (0..buf.area().height)
            .map(|y| {
                (0..buf.area().width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("Chat · NAV"));
        assert!(rendered.contains("not connected /connect required"));
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
                    mode_label: "INPUT",
                },
                f.area(),
            );
        })
        .unwrap();
        let buf = term.backend().buffer();
        let rendered: String = (0..buf.area().height)
            .map(|y| {
                (0..buf.area().width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("history"));
        assert!(rendered.contains("file.txt"));
    }
}
