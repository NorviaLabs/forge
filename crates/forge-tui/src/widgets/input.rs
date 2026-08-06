//! Input bar — multi-line paste / Shift+Enter newline.

use crate::theme;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Widget, Wrap};
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
const MAX_VISIBLE_ROWS: usize = 8;
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

/// Steady-state control chip under the composer text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposerChipKind {
    Mode,
    Connect,
    Model,
    Effort,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposerChip {
    pub kind: ComposerChipKind,
    pub label: String,
}

/// Priority when truncating: keep mode, then model, then connect, then effort.
const CHIP_DROP_ORDER: [ComposerChipKind; 4] = [
    ComposerChipKind::Effort,
    ComposerChipKind::Connect,
    ComposerChipKind::Model,
    ComposerChipKind::Mode,
];

/// Build the full chip set before width fitting.
pub fn composer_chips(
    mode_label: &str,
    connected: bool,
    vendor: Option<&str>,
    model: &str,
    effort: &str,
) -> Vec<ComposerChip> {
    let mut chips = vec![ComposerChip {
        kind: ComposerChipKind::Mode,
        label: mode_label.to_string(),
    }];
    chips.push(ComposerChip {
        kind: ComposerChipKind::Connect,
        label: if connected {
            vendor
                .filter(|v| !v.is_empty())
                .unwrap_or("connected")
                .to_string()
        } else {
            "not connected".into()
        },
    });
    let short = crate::widgets::footer::footer_short_model_id(model);
    if !short.is_empty() {
        chips.push(ComposerChip {
            kind: ComposerChipKind::Model,
            label: short.to_string(),
        });
    }
    if !effort.is_empty() {
        chips.push(ComposerChip {
            kind: ComposerChipKind::Effort,
            label: effort.to_string(),
        });
    }
    chips
}

/// Drop lowest-priority chips until the bracketed row fits `width`
/// (`[label]` per chip, single-space separators).
pub fn fit_composer_chips(mut chips: Vec<ComposerChip>, width: u16) -> Vec<ComposerChip> {
    let width = width as usize;
    let row_width = |chips: &[ComposerChip]| {
        if chips.is_empty() {
            return 0;
        }
        chips
            .iter()
            .map(|c| c.label.chars().count() + 2)
            .sum::<usize>()
            + chips.len().saturating_sub(1)
    };
    // Keep explicit N/A effort over vendor when space is tight — it signals
    // "this model has no effort control".
    let na_effort = chips
        .iter()
        .any(|c| c.kind == ComposerChipKind::Effort && c.label == "N/A");
    let drop_order: &[ComposerChipKind] = if na_effort {
        &[
            ComposerChipKind::Connect,
            ComposerChipKind::Model,
            ComposerChipKind::Effort,
            ComposerChipKind::Mode,
        ]
    } else {
        &CHIP_DROP_ORDER
    };
    for drop_kind in drop_order {
        if row_width(&chips) <= width || chips.len() <= 1 {
            break;
        }
        if let Some(i) = chips.iter().position(|c| c.kind == *drop_kind) {
            // Never drop Mode if anything else remains.
            if *drop_kind == ComposerChipKind::Mode && chips.len() > 1 {
                continue;
            }
            chips.remove(i);
        }
    }
    while row_width(&chips) > width && chips.len() > 1 {
        chips.pop();
    }
    let w = row_width(&chips);
    if w > width && width > 1 {
        if let Some(last) = chips.last_mut() {
            let excess = w - width;
            let keep = last.label.chars().count().saturating_sub(excess + 1).max(1);
            last.label = last.label.chars().take(keep).collect::<String>() + "…";
        }
    }
    chips
}

pub struct InputBar<'a> {
    pub model: &'a InputModel,
    /// Optional file-attachment label shown above the prompt line.
    pub attachment: Option<&'a str>,
    pub dimmed: bool,
    pub not_connected: bool,
    pub focused: bool,
    /// Approval pending — show the distinct waiting border (see `InputModel.waiting`).
    pub waiting: bool,
    /// Non-interactive send affordance on the first text row.
    pub show_send_hint: bool,
}

fn composer_text(model: &InputModel, show_cursor: bool) -> String {
    if model.text.is_empty() {
        if model.waiting {
            return String::new();
        }
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

/// `(row, col, total_rows)` of `offset` in `model.text`'s full, unscrolled
/// wrapped layout at `width` — independent of the current viewport/scroll
/// (unlike [`cursor_scroll`], which is about keeping a position visible in
/// a *limited* window). Used to drive [`InputModel::move_cursor_up`]/
/// [`InputModel::move_cursor_down`]: splices the same cursor sentinel used
/// for display at `offset` and renders through the real `Paragraph` +
/// `Wrap`, so results are pixel-identical to what's on screen rather than
/// reimplementing word-wrap in string space.
fn visual_row_col(model: &InputModel, width: u16, offset: usize) -> (u16, u16, u16) {
    let width = width.max(1);
    let offset = offset.min(model.text.len());
    let (before, after) = model.text.split_at(offset);
    let probe_text = format!("{before}{CURSOR_GLYPH}{after}");
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
            if scratch[(x, y)].symbol() == CURSOR_GLYPH {
                return (y, x, total_rows);
            }
        }
    }
    (0, 0, total_rows)
}

const SEND_HINT: &str = "⏎";

/// Left inset before composer text — simple breathing room from the border,
/// replacing the removed prompt-glyph gutter column.
pub(crate) const TEXT_INSET: u16 = 1;

/// Composer geometry derived from `model`/`area`/`attachment`/
/// `show_send_hint` — no styling. Shared by [`InputBar::render`] and
/// [`composer_cursor_position`] so the two never drift apart.
struct ComposerGeometry {
    input_area: Rect,
    text_area: Rect,
    send_w: u16,
}

fn composer_geometry(
    model: &InputModel,
    area: Rect,
    attachment: Option<&str>,
    show_send_hint: bool,
) -> Option<ComposerGeometry> {
    let block = Block::default().borders(Borders::ALL);
    let inner = block.inner(area);
    if inner.width == 0 || inner.height == 0 {
        return None;
    }

    let attach_h = u16::from(attachment.is_some() && inner.height > 1);
    let y = inner.y.saturating_add(attach_h);
    let remain = inner.height.saturating_sub(attach_h);
    let text_h = remain.max(1);
    let input_area = Rect::new(inner.x, y, inner.width, text_h);

    let send_w = if show_send_hint && input_area.width > TEXT_INSET + 2 {
        SEND_HINT.chars().count() as u16
    } else {
        0
    };
    let raw_text_area = Rect::new(
        input_area.x.saturating_add(TEXT_INSET),
        input_area.y,
        input_area
            .width
            .saturating_sub(TEXT_INSET)
            .saturating_sub(send_w.saturating_add(u16::from(send_w > 0))),
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

    Some(ComposerGeometry {
        input_area,
        text_area,
        send_w,
    })
}

/// Absolute `(x, y)` of the composer's cursor cell within `area`, for driving
/// the real terminal cursor (`Frame::set_cursor_position`). Renders the same
/// composer text used for display into a scratch buffer and scans for the
/// cursor sentinel — mirrors the display render exactly rather than
/// reimplementing wrap math independently.
pub fn composer_cursor_position(
    model: &InputModel,
    area: Rect,
    attachment: Option<&str>,
) -> Option<(u16, u16)> {
    let geometry = composer_geometry(model, area, attachment, false)?;
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
            if scratch[(x, y)].symbol() == CURSOR_GLYPH {
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
    let geometry = composer_geometry(model, area, attachment, false)?;
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
        let theme = crate::theme::active();

        let text_focused = self.focused;
        let border = if text_focused {
            theme::active_panel_border()
        } else if self.waiting {
            theme::waiting_border()
        } else if self.not_connected {
            theme::warn()
        } else {
            theme::composer_border_idle()
        };
        let block = Block::default()
            .borders(Borders::ALL)
            // Thick border — no other panel in the app (Files, Chat, editor)
            // uses anything but the default Plain shape, so this is a
            // structural "this is an input" signal that survives even
            // without color (reduced-color terminals, colorblind users).
            .border_type(BorderType::Thick)
            .border_style(border)
            .style(if self.dimmed {
                theme::surface_hover()
            } else {
                theme::composer_surface()
            });
        let inner = block.inner(area);
        if inner.width == 0 || inner.height == 0 {
            return;
        }
        block.render(area, buf);

        let Some(geometry) =
            composer_geometry(self.model, area, self.attachment, self.show_send_hint)
        else {
            return;
        };
        let input_area = geometry.input_area;
        let text_area = geometry.text_area;
        let send_w = geometry.send_w;

        if self.attachment.is_some() && inner.height > 1 {
            let att_text = self.attachment.unwrap_or("");
            let att_line = Line::from(vec![
                Span::styled("» ", theme::info()),
                Span::styled(att_text, theme::info()),
                Span::styled("  [Ctrl+A or /cf to remove]", theme::dim()),
            ]);
            Paragraph::new(att_line).render(Rect::new(inner.x, inner.y, inner.width, 1), buf);
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
        if send_w > 0 {
            let sx = input_area.x + input_area.width.saturating_sub(send_w);
            buf.set_stringn(sx, input_area.y, SEND_HINT, send_w as usize, theme::dim());
        }
        if text_focused {
            // The real terminal cursor (driven by `composer_cursor_position`
            // from the caller) renders the visible caret now — blend this
            // sentinel cell into its own background instead of styling it as
            // a fake caret, so it doesn't double up with the real cursor.
            for y in text_area.top()..text_area.bottom() {
                for x in text_area.left()..text_area.right() {
                    if buf[(x, y)].symbol() == CURSOR_GLYPH {
                        let bg = buf[(x, y)]
                            .style()
                            .bg
                            .unwrap_or_else(|| theme::palette(&theme).panel);
                        buf[(x, y)].set_style(Style::default().fg(bg).bg(bg));
                        break;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme;
    use forge_config::THEME_SOLARIZED_DARK;
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
                    waiting: model.waiting,
                    show_send_hint: false,
                },
                f.area(),
            );
        })
        .unwrap();
        term.backend().buffer().clone()
    }

    fn render_lines(model: &InputModel, width: u16, height: u16, focused: bool) -> Vec<String> {
        let buf = draw_input_bar(model, width, height, focused, model.not_connected, None);
        let inner = Rect::new(
            1,
            1,
            buf.area().width.saturating_sub(2),
            buf.area().height.saturating_sub(2),
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

    /// The real terminal cursor (driven by `composer_cursor_position`) is now
    /// the visible caret, so the in-buffer sentinel is intentionally blended
    /// into its own background — this asserts the sentinel cell still exists
    /// (for `composer_cursor_position` to find) and that it's actually
    /// invisible (fg == bg), not that it's rendered with caret styling.
    #[test]
    fn cursor_sentinel_cell_exists_and_is_blended_invisible() {
        let mut m = InputModel::default();
        m.set_text("ab");
        m.cursor = 2;
        let buf = draw_input_bar(&m, 40, 5, true, false, None);
        let area = buf.area();
        let mut found = None;
        for y in 0..area.height {
            for x in 0..area.width {
                if buf[(x, y)].symbol() == CURSOR_GLYPH {
                    found = Some(buf[(x, y)].style());
                }
            }
        }
        let style = found.expect("expected cursor sentinel cell");
        assert_eq!(
            style.fg, style.bg,
            "sentinel should blend into its background"
        );
    }

    #[test]
    fn empty_input_starts_with_caret_cell() {
        let m = InputModel::default();
        let buf = draw_input_bar(&m, 40, 5, true, false, None);
        // Short content is vertically centered in the box (height 5 → 3
        // inner rows, 1 line of content → 1 row of top padding), and text
        // starts right after TEXT_INSET, not a removed gutter column.
        let cell = &buf[(2, 2)];
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
    fn composer_cursor_position_locates_glyph_for_single_line_text() {
        let mut m = InputModel::default();
        m.set_text("ab");
        m.cursor = 2;
        let area = Rect::new(0, 0, 40, 5);
        let (x, y) = composer_cursor_position(&m, area, None).expect("expected cursor position");
        // Height 5 → 3 inner rows; 1 line of short content is vertically
        // centered, landing on the second inner row (1 row of padding
        // above it).
        assert_eq!(y, 2);
        assert!(x > area.x, "cursor x should be inside the border");
    }

    #[test]
    fn composer_cursor_position_none_when_waiting_hides_the_composer() {
        let m = InputModel {
            waiting: true,
            ..Default::default()
        };
        let area = Rect::new(0, 0, 40, 5);
        assert_eq!(composer_cursor_position(&m, area, None), None);
    }

    #[test]
    fn composer_cursor_position_advances_to_the_wrapped_row() {
        let mut m = InputModel::default();
        m.set_text("word ".repeat(30).trim().to_string());
        m.cursor = m.text.len();
        let area = Rect::new(0, 0, 24, 8);
        let (_, y) = composer_cursor_position(&m, area, None).expect("expected cursor position");
        assert!(
            y > 1,
            "cursor should have advanced past the first wrapped row"
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
    }

    #[test]
    fn waiting_state_uses_waiting_border_and_hides_hint() {
        let m = InputModel {
            waiting: true,
            hint: "type here".into(),
            ..Default::default()
        };
        let buf = draw_input_bar(&m, 48, 5, false, false, None);
        let border = &buf[(0, 0)];
        assert_eq!(
            border.style().fg,
            Some(theme::palette(&theme::active()).waiting_border)
        );
        let rendered: String = (0..buf.area().height)
            .map(|y| {
                (0..buf.area().width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!rendered.contains("type here"), "{rendered}");
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
        assert!(rendered.contains("line1"));
        assert!(rendered.contains("line2"));
    }

    #[test]
    fn fit_composer_chips_drops_effort_before_mode() {
        let chips = composer_chips(
            "Accept Edits",
            true,
            Some("Anthropic"),
            "anthropic/claude-sonnet-4",
            "High",
        );
        let fitted = fit_composer_chips(chips, 28);
        assert!(
            fitted.iter().any(|c| c.kind == ComposerChipKind::Mode),
            "{fitted:?}"
        );
        assert!(
            !fitted.iter().any(|c| c.kind == ComposerChipKind::Effort),
            "effort should drop first: {fitted:?}"
        );
    }

    #[test]
    fn empty_composer_renders_placeholder_without_marker() {
        let m = InputModel {
            hint: "Ask Forge anything…".into(),
            ..Default::default()
        };
        let rows = render_lines(&m, 60, 5, true);
        assert_eq!(rows.len(), 1);
        // Leading space is TEXT_INSET; focused so the cursor sentinel
        // precedes the hint text — neither is the removed marker glyph.
        assert!(rows[0]
            .trim_start()
            .starts_with(&format!("{CURSOR_GLYPH}Ask Forge anything…")));
        assert!(m.copy_text().is_empty());
    }

    #[test]
    fn single_line_input_has_no_marker() {
        let m = InputModel {
            text: "Summarize this codebase".into(),
            cursor: 0,
            ..Default::default()
        };
        let rows = render_lines(&m, 60, 5, true);
        assert_eq!(rows.len(), 1);
        assert!(rows[0]
            .trim_start()
            .starts_with(&format!("{CURSOR_GLYPH}Summarize")));
        assert_eq!(m.copy_text(), "Summarize this codebase");
    }

    #[test]
    fn wrapped_input_has_no_marker_on_any_row() {
        let m = InputModel {
            text: "word ".repeat(30).trim().to_string(),
            cursor: 0,
            ..Default::default()
        };
        let rows = render_lines(&m, 24, 8, true);
        assert!(rows.len() >= 3);
        for row in &rows {
            assert!(!row.trim_start().starts_with(glyph()), "row: {row}");
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
    fn explicit_newlines_render_one_row_each() {
        let m = InputModel {
            text: "one\ntwo\nthree".into(),
            ..Default::default()
        };
        let raw_rows = render_lines(&m, 60, 8, false);
        let rows: Vec<&str> = raw_rows.iter().map(|row| row.trim_start()).collect();
        assert_eq!(rows, vec!["one", "two", "three"]);
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

    #[test]
    fn multiline_input_scrolls_cursor_into_view() {
        let m = InputModel {
            text: "line1\nline2\nline3\nline4\nline5".into(),
            cursor: "line1\nline2\nline3\nline4\nline5".len(),
            ..Default::default()
        };
        let rows = render_lines(&m, 60, 5, true);
        assert!(rows.iter().any(|row| row.contains("line5")));
    }
}
