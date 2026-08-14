//! CommonMark rendering for assistant answers.
//!
//! The answer text flows through `pulldown_cmark` and is mapped onto ratatui
//! `Line`s, so full markdown — headings, emphasis, strikethrough, nested
//! ordered/unordered/task lists, block quotes, syntax-highlighted fenced code,
//! tables, links, and rules — renders instead of falling through as literal
//! markup. Inline code and fenced blocks keep the code styling used elsewhere
//! in the TUI.
//!
//! The input may be a partial stream while the model is still writing. The
//! parser tolerates unclosed constructs: a fenced block that never closes
//! still renders its body (no synthetic closing fence is emitted).

use crate::theme;
use forge_syntax::highlight_to_lines;
use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

/// Space on each side of a cell, inside the `│` walls.
const CELL_PAD: usize = 1;

pub fn render_markdown(text: &str, width: usize) -> Vec<Line<'static>> {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES;
    let mut renderer = MdRenderer::new(width.max(1));
    renderer.feed(Parser::new_ext(text, options));
    renderer.finish()
}

struct ListFrame {
    ordered: bool,
    index: u64,
    indent: usize,
    marker_w: usize,
    saved_cont: String,
}

struct CodeBuffer {
    fenced: bool,
    language: String,
    body: String,
}

struct TableRow {
    header: bool,
    cells: Vec<Vec<Span<'static>>>,
}

struct TableBuilder {
    alignments: Vec<Alignment>,
    rows: Vec<TableRow>,
}

impl TableBuilder {
    fn new(alignments: Vec<Alignment>) -> Self {
        TableBuilder {
            alignments,
            rows: Vec::new(),
        }
    }

    fn start_row(&mut self, header: bool) {
        self.rows.push(TableRow {
            header,
            cells: Vec::new(),
        });
    }

    fn end_cell(&mut self, spans: Vec<Span<'static>>) {
        if let Some(row) = self.rows.last_mut() {
            row.cells.push(spans);
        }
    }
}

struct MdRenderer {
    width: usize,
    out: Vec<Line<'static>>,
    inline: Vec<Span<'static>>,
    style_stack: Vec<Style>,
    list_stack: Vec<ListFrame>,
    quote_depth: usize,
    prefix: String,
    cont_prefix: String,
    code: Option<CodeBuffer>,
    table: Option<TableBuilder>,
    in_html_block: bool,
    html_buf: String,
}

impl MdRenderer {
    fn new(width: usize) -> Self {
        MdRenderer {
            width,
            out: Vec::new(),
            inline: Vec::new(),
            style_stack: Vec::new(),
            list_stack: Vec::new(),
            quote_depth: 0,
            prefix: String::new(),
            cont_prefix: String::new(),
            code: None,
            table: None,
            in_html_block: false,
            html_buf: String::new(),
        }
    }

    fn feed(&mut self, parser: Parser<'_>) {
        for event in parser {
            match event {
                Event::Start(tag) => self.on_start(tag),
                Event::End(tag) => self.on_end(tag),
                Event::Text(t) => self.on_text(t.into_string()),
                Event::Code(t) => self.inline.push(Span::styled(
                    t.into_string(),
                    theme::text_secondary().add_modifier(Modifier::BOLD),
                )),
                Event::InlineHtml(t) => self.push_span(t.into_string()),
                Event::Html(t) => {
                    if self.in_html_block {
                        self.html_buf.push_str(&t);
                    } else {
                        self.push_span(t.into_string());
                    }
                }
                Event::SoftBreak => {}
                Event::HardBreak => self.flush_para(),
                Event::Rule => {
                    self.flush_para();
                    self.out.push(Line::from(Span::styled(
                        "─".repeat(self.width),
                        theme::muted(),
                    )));
                }
                Event::TaskListMarker(checked) => {
                    let mark = if checked { "[x]" } else { "[ ]" };
                    self.inline.push(Span::styled(
                        mark.to_string(),
                        theme::text().add_modifier(Modifier::BOLD),
                    ));
                    self.inline.push(Span::raw(" "));
                }
                Event::FootnoteReference(name) => {
                    self.inline
                        .push(Span::styled(format!("[^{name}]"), theme::text_secondary()));
                }
                Event::InlineMath(_) | Event::DisplayMath(_) => {}
            }
        }
    }

    fn on_start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {}
            Tag::Heading { .. } => {
                self.push_style(Style::default().add_modifier(Modifier::BOLD));
            }
            Tag::BlockQuote(_) => {
                self.flush_para();
                self.quote_depth += 1;
            }
            Tag::CodeBlock(kind) => {
                self.flush_para();
                let (fenced, language) = match kind {
                    CodeBlockKind::Fenced(info) => (true, info.to_ascii_lowercase()),
                    CodeBlockKind::Indented => (false, String::new()),
                };
                self.code = Some(CodeBuffer {
                    fenced,
                    language,
                    body: String::new(),
                });
            }
            Tag::List(start) => {
                let frame = ListFrame {
                    ordered: start.is_some(),
                    index: start.unwrap_or(1),
                    indent: self.cont_prefix.len(),
                    marker_w: 0,
                    saved_cont: self.cont_prefix.clone(),
                };
                self.list_stack.push(frame);
            }
            Tag::Item => {
                self.flush_para();
                if let Some(frame) = self.list_stack.last_mut() {
                    let marker = if frame.ordered {
                        let marker = format!("{}. ", frame.index);
                        frame.index += 1;
                        marker
                    } else {
                        "• ".to_string()
                    };
                    frame.marker_w = marker.len();
                    self.prefix = format!("{}{}", " ".repeat(frame.indent), marker);
                    self.cont_prefix = " ".repeat(frame.indent + frame.marker_w);
                }
            }
            Tag::Emphasis => self.push_style(Style::default().add_modifier(Modifier::ITALIC)),
            Tag::Strong => self.push_style(Style::default().add_modifier(Modifier::BOLD)),
            Tag::Strikethrough => {
                self.push_style(Style::default().add_modifier(Modifier::CROSSED_OUT))
            }
            Tag::Link { .. } => self.push_style(
                Style::default()
                    .fg(theme::accent_color())
                    .add_modifier(Modifier::UNDERLINED),
            ),
            Tag::Image { .. } => {
                self.push_span("[".into());
                self.push_style(
                    Style::default()
                        .fg(theme::text_dim_color())
                        .add_modifier(Modifier::ITALIC),
                );
            }
            Tag::Table(alignments) => {
                self.flush_para();
                self.table = Some(TableBuilder::new(alignments));
            }
            Tag::TableHead => {
                if let Some(table) = &mut self.table {
                    table.start_row(true);
                }
            }
            Tag::TableRow => {
                if let Some(table) = &mut self.table {
                    table.start_row(false);
                }
            }
            Tag::TableCell => {
                self.inline.clear();
            }
            Tag::FootnoteDefinition(_) => {
                self.flush_para();
                self.quote_depth += 1;
            }
            Tag::HtmlBlock => {
                self.flush_para();
                self.in_html_block = true;
                self.html_buf.clear();
            }
            _ => {}
        }
    }

    fn on_end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.flush_para();
                if self.list_stack.is_empty() && self.quote_depth == 0 {
                    self.out.push(Line::from(""));
                }
            }
            TagEnd::Heading(_) => {
                self.pop_style();
                self.flush_para();
            }
            TagEnd::Item => {
                self.flush_para();
                self.prefix.clear();
                self.cont_prefix.clear();
            }
            TagEnd::List(_) => {
                if let Some(frame) = self.list_stack.pop() {
                    self.cont_prefix = frame.saved_cont;
                }
            }
            TagEnd::BlockQuote(_) | TagEnd::FootnoteDefinition => {
                self.flush_para();
                self.quote_depth = self.quote_depth.saturating_sub(1);
            }
            TagEnd::CodeBlock => {
                if let Some(code) = self.code.take() {
                    self.render_code(code);
                }
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => self.pop_style(),
            TagEnd::Link => self.pop_style(),
            TagEnd::Image => {
                self.pop_style();
                self.push_span("]".into());
            }
            TagEnd::Table => {
                if let Some(table) = self.table.take() {
                    // The item marker belongs to the list text, not the box.
                    // Indent every table line with the quote rail plus the
                    // list continuation so the left wall stays a straight line.
                    let prefix = format!("{}{}", "│ ".repeat(self.quote_depth), self.cont_prefix);
                    self.out
                        .extend(render_table(&table, self.width, &prefix, &prefix));
                }
            }
            TagEnd::TableRow => {}
            TagEnd::TableCell => {
                if let Some(table) = &mut self.table {
                    table.end_cell(std::mem::take(&mut self.inline));
                }
            }
            TagEnd::HtmlBlock => {
                self.in_html_block = false;
                if !self.html_buf.is_empty() {
                    for line in wrap_spans(
                        &[Span::styled(
                            std::mem::take(&mut self.html_buf),
                            theme::muted(),
                        )],
                        self.width,
                        "",
                        "",
                    ) {
                        self.out.push(line);
                    }
                }
            }
            _ => {}
        }
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        self.flush_para();
        if let Some(code) = self.code.take() {
            self.render_code(code);
        }
        while self.out.last().is_some_and(|line| line.width() == 0) {
            self.out.pop();
        }
        if self.out.is_empty() {
            self.out.push(Line::from(String::new()));
        }
        self.out
    }

    fn on_text(&mut self, t: String) {
        if let Some(code) = &mut self.code {
            code.body.push_str(&t);
        } else {
            self.push_span(t);
        }
    }

    fn push_style(&mut self, style: Style) {
        self.style_stack.push(style);
    }

    fn pop_style(&mut self) {
        self.style_stack.pop();
    }

    fn current_style(&self) -> Style {
        let mut style = theme::text();
        for s in &self.style_stack {
            style = style.patch(*s);
        }
        style
    }

    fn push_span(&mut self, text: String) {
        self.inline.push(Span::styled(text, self.current_style()));
    }

    fn flush_para(&mut self) {
        if self.inline.is_empty() {
            return;
        }
        let quote = "│ ".repeat(self.quote_depth);
        let prefix = format!("{quote}{}", self.prefix);
        let cont = format!("{quote}{}", self.cont_prefix);
        for line in wrap_spans(&self.inline, self.width, &prefix, &cont) {
            self.out.push(line);
        }
        self.inline.clear();
    }

    fn render_code(&mut self, code: CodeBuffer) {
        if code.fenced {
            let label = if code.language.is_empty() {
                "  ```".to_string()
            } else {
                format!("  ```{}", code.language)
            };
            self.out
                .push(Line::from(Span::styled(label, theme::code_punctuation())));
        }
        let body = code.body.trim_end_matches('\n');
        if body.is_empty() {
            return;
        }
        let theme = theme::syntax_theme();
        for line_segments in highlight_to_lines(&code.language, body, &theme).iter() {
            self.out.push(
                Line::from(render_highlighted_line(line_segments)).style(theme::code_block()),
            );
        }
    }
}

fn display_width(s: &str) -> usize {
    Span::raw(s).width()
}

/// Split `s` so the first piece is at most `max` columns wide. A single
/// display-wider-than-`max` grapheme is taken anyway so wrapping can progress.
fn split_at_width(s: &str, max: usize) -> (&str, &str) {
    if max == 0 {
        return ("", s);
    }
    let mut used = 0;
    for (idx, ch) in s.char_indices() {
        let cw = display_width(ch.encode_utf8(&mut [0; 4]));
        if used + cw > max {
            if idx == 0 {
                let end = idx + ch.len_utf8();
                return (&s[..end], &s[end..]);
            }
            return (&s[..idx], &s[idx..]);
        }
        used += cw;
    }
    (s, "")
}

/// Greedy word-wrap `spans` to `width`, prefixing the first line with `prefix`
/// and continuation lines with `cont`. Styles travel with the words, so inline
/// code and emphasis stay styled across wraps. Words that still do not fit a
/// fresh line are hard-broken so a table cell cannot blow the pane width.
fn wrap_spans(
    spans: &[Span<'static>],
    width: usize,
    prefix: &str,
    cont: &str,
) -> Vec<Line<'static>> {
    if spans.is_empty() {
        return Vec::new();
    }
    let prefix_span = Span::styled(prefix.to_string(), theme::muted());
    let cont_span = Span::styled(cont.to_string(), theme::muted());
    let prefix_w = display_width(prefix);
    let cont_w = display_width(cont);
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut cur: Vec<Span<'static>> = vec![prefix_span];
    let mut cur_w = prefix_w;
    let mut has_content = false;

    for (word, style) in spans.iter().flat_map(tokenize) {
        let mut remaining = word.as_str();
        while !remaining.is_empty() {
            let gap = usize::from(has_content);
            let room = width.saturating_sub(cur_w + gap);
            let wlen = display_width(remaining);
            if wlen <= room {
                if has_content {
                    cur.push(Span::raw(" "));
                    cur_w += 1;
                }
                cur.push(Span::styled(remaining.to_string(), style));
                cur_w += wlen;
                has_content = true;
                break;
            }
            if has_content {
                out.push(Line::from(std::mem::take(&mut cur)));
                cur.push(cont_span.clone());
                cur_w = cont_w;
                has_content = false;
                continue;
            }
            if room == 0 {
                out.push(Line::from(std::mem::take(&mut cur)));
                cur.push(cont_span.clone());
                cur_w = cont_w;
                continue;
            }
            let (chunk, rest) = split_at_width(remaining, room);
            cur.push(Span::styled(chunk.to_string(), style));
            out.push(Line::from(std::mem::take(&mut cur)));
            cur.push(cont_span.clone());
            cur_w = cont_w;
            has_content = false;
            remaining = rest;
        }
    }
    out.push(Line::from(cur));
    out
}

fn tokenize(span: &Span<'static>) -> Vec<(String, Style)> {
    span.content
        .split_whitespace()
        .map(|word| (word.to_string(), span.style))
        .collect()
}

/// Walls + inner verticals + `CELL_PAD` on each side of every column.
fn table_chrome(col_count: usize) -> usize {
    2 + col_count.saturating_sub(1) + col_count.saturating_mul(2 * CELL_PAD)
}

fn spans_width(spans: &[Span<'static>]) -> usize {
    spans.iter().map(Span::width).sum()
}

fn style_header_cell(cell: &[Span<'static>]) -> Vec<Span<'static>> {
    let header = theme::tag_style(false).add_modifier(Modifier::BOLD);
    cell.iter()
        .map(|span| {
            let mut styled = span.clone();
            styled.style = styled.style.patch(header);
            styled
        })
        .collect()
}

fn align_cell(line: Line<'static>, width: usize, alignment: Alignment) -> Vec<Span<'static>> {
    let used = line.width();
    let pad = width.saturating_sub(used);
    let (left, right) = match alignment {
        Alignment::Right => (pad, 0),
        Alignment::Center => (pad / 2, pad - pad / 2),
        Alignment::None | Alignment::Left => (0, pad),
    };
    let mut out = Vec::new();
    if left > 0 {
        out.push(Span::raw(" ".repeat(left)));
    }
    out.extend(line.spans);
    if right > 0 {
        out.push(Span::raw(" ".repeat(right)));
    }
    out
}

fn frame_line(prefix: &str, widths: &[usize], left: char, mid: char, right: char) -> Line<'static> {
    let mut rule = String::new();
    rule.push(left);
    for (i, w) in widths.iter().enumerate() {
        if i > 0 {
            rule.push(mid);
        }
        rule.push_str(&"─".repeat(*w + 2 * CELL_PAD));
    }
    rule.push(right);
    Line::from(vec![
        Span::styled(prefix.to_string(), theme::muted()),
        Span::styled(rule, theme::border_muted()),
    ])
}

fn render_table(
    table: &TableBuilder,
    width: usize,
    first_prefix: &str,
    rest_prefix: &str,
) -> Vec<Line<'static>> {
    let col_count = table
        .rows
        .iter()
        .map(|row| row.cells.len())
        .max()
        .unwrap_or(0);
    if col_count == 0 {
        return Vec::new();
    }

    let mut alignments = table.alignments.clone();
    alignments.resize(col_count, Alignment::None);

    let mut rows: Vec<(bool, Vec<Vec<Span<'static>>>)> = Vec::with_capacity(table.rows.len());
    for (idx, row) in table.rows.iter().enumerate() {
        let mut cells = row.cells.clone();
        cells.resize(col_count, Vec::new());
        let header = row.header || idx == 0;
        let cells = if header {
            cells.iter().map(|cell| style_header_cell(cell)).collect()
        } else {
            cells
        };
        rows.push((header, cells));
    }

    let mut natural = vec![0usize; col_count];
    for (_, cells) in &rows {
        for (column, cell) in cells.iter().enumerate() {
            natural[column] = natural[column].max(spans_width(cell));
        }
    }

    let prefix_w = display_width(first_prefix).max(display_width(rest_prefix));
    let available = width.saturating_sub(prefix_w);
    let chrome = table_chrome(col_count);
    let inner_budget = available.saturating_sub(chrome);
    let widths = if natural.iter().copied().sum::<usize>() <= inner_budget {
        natural
    } else {
        shrink_widths(&natural, inner_budget)
    };

    let mut out = Vec::new();
    let mut emitted = 0usize;
    let mut take_prefix = || {
        let prefix = if emitted == 0 {
            first_prefix
        } else {
            rest_prefix
        };
        emitted += 1;
        prefix.to_string()
    };

    out.push(frame_line(&take_prefix(), &widths, '┌', '┬', '┐'));

    let mut saw_body = false;
    for (header, cells) in &rows {
        if !header && !saw_body {
            out.push(frame_line(&take_prefix(), &widths, '├', '┼', '┤'));
            saw_body = true;
        }
        out.extend(render_boxed_row(
            &mut take_prefix,
            cells,
            &widths,
            &alignments,
        ));
    }
    if !saw_body {
        out.push(frame_line(&take_prefix(), &widths, '├', '┼', '┤'));
    }
    out.push(frame_line(&take_prefix(), &widths, '└', '┴', '┘'));
    out
}

fn render_boxed_row(
    take_prefix: &mut impl FnMut() -> String,
    cells: &[Vec<Span<'static>>],
    widths: &[usize],
    alignments: &[Alignment],
) -> Vec<Line<'static>> {
    let wrapped: Vec<Vec<Line<'static>>> = cells
        .iter()
        .zip(widths)
        .map(|(cell, w)| {
            if *w == 0 {
                return vec![Line::from("")];
            }
            let lines = wrap_spans(cell, *w, "", "");
            if lines.is_empty() {
                vec![Line::from("")]
            } else {
                lines
            }
        })
        .collect();
    let height = wrapped.iter().map(|lines| lines.len()).max().unwrap_or(1);
    let border = theme::border_muted();
    let mut out = Vec::new();
    for row_line in 0..height {
        let mut spans = vec![
            Span::styled(take_prefix(), theme::muted()),
            Span::styled("│".to_string(), border),
        ];
        for (column, lines) in wrapped.iter().enumerate() {
            if column > 0 {
                spans.push(Span::styled("│".to_string(), border));
            }
            spans.push(Span::raw(" ".repeat(CELL_PAD)));
            let line = lines.get(row_line).cloned().unwrap_or_default();
            let alignment = alignments.get(column).copied().unwrap_or(Alignment::None);
            spans.extend(align_cell(line, widths[column], alignment));
            spans.push(Span::raw(" ".repeat(CELL_PAD)));
        }
        spans.push(Span::styled("│".to_string(), border));
        out.push(Line::from(spans));
    }
    out
}

/// Distribute available width across columns, keeping at least the narrowest
/// natural content visible and shrinking the widest columns first.
fn shrink_widths(natural: &[usize], available: usize) -> Vec<usize> {
    let col_count = natural.len();
    let floor = 4usize;
    let mut widths = if floor.saturating_mul(col_count) <= available {
        natural
            .iter()
            .map(|n| (*n).min(floor))
            .collect::<Vec<usize>>()
    } else {
        // Even the floor doesn't fit: split `available` evenly instead of
        // letting each column claim `floor` regardless, which would push the
        // row past the render width.
        let base = available / col_count.max(1);
        let extra = available % col_count.max(1);
        (0..col_count)
            .map(|i| base + usize::from(i < extra))
            .collect()
    };
    let mut remaining = available.saturating_sub(widths.iter().sum::<usize>());
    loop {
        let over_wide = widths
            .iter()
            .zip(natural)
            .filter(|(w, n)| **w < **n)
            .count();
        if over_wide == 0 || remaining == 0 {
            break;
        }
        let step = remaining / over_wide + usize::from(remaining % over_wide > 0);
        let mut grew = false;
        for (w, n) in widths.iter_mut().zip(natural) {
            if *w < *n {
                let grow = step.min(*n - *w).min(remaining);
                *w += grow;
                remaining -= grow;
                grew |= grow > 0;
            }
        }
        if !grew {
            break;
        }
    }
    widths
}

fn render_highlighted_line(segments: &[forge_syntax::HighlightedSegment]) -> Vec<Span<'static>> {
    let block = theme::code_block();
    segments
        .iter()
        .map(|(text, rgb, bold, italic)| {
            let mut style =
                theme::syntax_segment(*rgb, Some(block.bg.unwrap_or(theme::panel_alt_bg())));
            if *bold {
                style = style.add_modifier(Modifier::BOLD);
            }
            if *italic {
                style = style.add_modifier(Modifier::ITALIC);
            }
            Span::styled(text.clone(), style)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(rendered: &[Line<'static>]) -> String {
        rendered
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn renders_blocks_without_literal_markup() {
        let md = "\
# Title

Some **bold** and *italic* and ~struck~ and `code` text.

- item one
- item two

1. first
2. second

> a quote

| a | b |
|---|---|
| 1 | 2 |
";
        let rendered = text(&render_markdown(md, 80));
        assert!(!rendered.contains("**"), "{rendered}");
        assert!(!rendered.contains("~"), "{rendered}");
        assert!(!rendered.contains("|"), "{rendered}");
        assert!(rendered.contains("Title"), "{rendered}");
        assert!(rendered.contains("bold"), "{rendered}");
        assert!(rendered.contains("• item one"), "{rendered}");
        assert!(rendered.contains("1. first"), "{rendered}");
        assert!(rendered.contains("│ a quote"), "{rendered}");
        assert!(rendered.contains("┌"), "{rendered}");
        assert!(rendered.contains("┼"), "{rendered}");
        assert!(rendered.contains("└"), "{rendered}");
    }

    #[test]
    fn styles_inline_constructs() {
        let rendered = render_markdown("plain with `inline code` in it", 80);
        let code_span = rendered
            .iter()
            .flat_map(|line| line.spans.iter())
            .find(|span| span.content.as_ref() == "inline")
            .expect("inline code token present");
        assert_eq!(code_span.style.fg, Some(theme::text_secondary_color()));
    }

    #[test]
    fn separates_top_level_paragraphs() {
        let rendered = render_markdown("First paragraph.\n\nSecond paragraph.", 80);
        assert_eq!(rendered.len(), 3);
        assert!(rendered[1].width() == 0);
    }

    #[test]
    fn shrink_widths_never_exceeds_available_with_many_narrow_columns() {
        // 10 columns each wanting >= 4 cols (the old floor) but only 15
        // available: floor * col_count (40) blows past `available` (15).
        let natural = vec![10usize; 10];
        let widths = shrink_widths(&natural, 15);
        assert_eq!(widths.len(), 10);
        assert!(
            widths.iter().sum::<usize>() <= 15,
            "widths {widths:?} sum past available width"
        );
    }

    #[test]
    fn shrink_widths_still_grows_widest_columns_when_floor_fits() {
        // Unaffected case: floor fits comfortably, so the existing
        // grow-widest-first behavior should be untouched.
        let natural = vec![10usize, 20usize];
        let widths = shrink_widths(&natural, 24);
        assert_eq!(widths.iter().sum::<usize>(), 24);
        assert!(widths[1] >= widths[0]);
    }

    #[test]
    fn table_never_exceeds_width_with_many_narrow_columns() {
        let width = 20usize;
        let md = "\
| wa wb wc | xa xb xc | ya yb yc | za zb zc |
|---|---|---|---|
| p q r | s t u | v w x | y z aa |
";
        let rendered = render_markdown(md, width);
        for line in &rendered {
            assert!(
                line.width() <= width,
                "line exceeds width {width}: {line:?}"
            );
        }
    }

    fn table_md() -> &'static str {
        "\
| Name | Age |
| --- | --- |
| Ana | 30 |
| Bob | 31 |
"
    }

    #[test]
    fn table_draws_outer_box_with_header_rule_only() {
        let rendered = text(&render_markdown(table_md(), 80));
        let lines: Vec<&str> = rendered.lines().collect();
        assert!(
            lines[0].starts_with('┌') && lines[0].contains('┬') && lines[0].ends_with('┐'),
            "{rendered}"
        );
        assert!(
            lines[1].starts_with('│') && lines[1].contains("Name"),
            "{rendered}"
        );
        assert!(
            lines[2].starts_with('├') && lines[2].contains('┼') && lines[2].ends_with('┤'),
            "{rendered}"
        );
        assert!(
            lines[3].contains("Ana") && lines[3].contains("30"),
            "{rendered}"
        );
        assert!(
            lines[4].contains("Bob") && !lines[4].contains('┼'),
            "{rendered}"
        );
        assert!(
            lines
                .last()
                .is_some_and(|l| l.starts_with('└') && l.contains('┴') && l.ends_with('┘')),
            "{rendered}"
        );
        assert_eq!(
            rendered.matches('┼').count(),
            1,
            "only the header rule should have ┼:\n{rendered}"
        );
    }

    #[test]
    fn table_hugs_content_instead_of_stretching() {
        let rendered = render_markdown(table_md(), 80);
        for line in &rendered {
            assert!(
                line.width() < 30,
                "hug failed, line width {}:\n{:?}",
                line.width(),
                line
            );
        }
    }

    #[test]
    fn table_header_uses_tag_and_bold() {
        let rendered = render_markdown(table_md(), 80);
        let name = rendered
            .iter()
            .flat_map(|line| line.spans.iter())
            .find(|span| span.content.as_ref() == "Name")
            .expect("header cell");
        assert_eq!(name.style.fg, theme::tag_style(false).fg);
        assert!(name.style.add_modifier.contains(Modifier::BOLD));
        let ana = rendered
            .iter()
            .flat_map(|line| line.spans.iter())
            .find(|span| span.content.as_ref() == "Ana")
            .expect("body cell");
        assert_ne!(ana.style.fg, theme::tag_style(false).fg);
    }

    #[test]
    fn table_honors_gfm_alignment() {
        let md = "\
| left | mid | right |
|:-----|:---:|------:|
| a | b | c |
";
        let rendered = text(&render_markdown(md, 80));
        let body = rendered
            .lines()
            .find(|l| l.contains('a') && l.contains('b') && l.contains('c'))
            .expect(&rendered);
        // inner widths follow the headers: left=4, mid=3, right=5
        assert!(
            body.contains("│ a    │  b  │     c │"),
            "alignment padding:\n{rendered}"
        );
    }

    #[test]
    fn table_wraps_header_and_body_inside_the_box() {
        let md = "\
| Very long column name | Status |
| --- | --- |
| a fairly long value | ok |
";
        let width = 24usize;
        let rendered = render_markdown(md, width);
        for line in &rendered {
            assert!(
                line.width() <= width,
                "line exceeds width {width}: {line:?}"
            );
        }
        let text = text(&rendered);
        assert!(text.contains("Very"), "{text}");
        assert!(text.contains("column"), "{text}");
        assert!(text.contains("fairly"), "{text}");
        assert!(text.contains('┌') && text.contains('└'), "{text}");
        // wrapped header/body still carry verticals
        let vertical_rows = text.lines().filter(|l| l.starts_with('│')).count();
        assert!(
            vertical_rows >= 4,
            "expected wrapped rows inside the box:\n{text}"
        );
    }

    #[test]
    fn table_inside_quote_keeps_quote_rail() {
        let md = "\
> | Name | Age |
> | --- | --- |
> | Ana | 30 |
";
        let rendered = text(&render_markdown(md, 80));
        for line in rendered.lines() {
            assert!(
                line.starts_with("│ "),
                "quoted table line must keep the quote rail:\n{rendered}"
            );
        }
        assert!(rendered.contains("│ ┌"), "{rendered}");
        assert!(rendered.contains("│ │ Name"), "{rendered}");
        assert!(rendered.contains("│ └"), "{rendered}");
    }

    #[test]
    fn table_inside_list_item_stays_indented() {
        let md = "\
- item

  | Name | Age |
  | --- | --- |
  | Ana | 30 |
";
        let rendered = text(&render_markdown(md, 80));
        let top = rendered
            .lines()
            .find(|l| l.contains('┌'))
            .unwrap_or_else(|| panic!("boxed table missing:\n{rendered}"));
        // List continuation uses marker.len() (bytes), so `• ` indents 4.
        // Match that existing indent rather than inventing a new one.
        assert!(
            top.starts_with("    ┌"),
            "table should sit on the list continuation indent:\n{rendered}"
        );
        assert!(
            !top.contains('•'),
            "item marker must not attach to the box:\n{rendered}"
        );
    }

    #[test]
    fn table_column_width_uses_display_width() {
        let md = "\
| 中 | x |
| --- | --- |
| 中 | y |
";
        let rendered = text(&render_markdown(md, 80));
        let top = rendered.lines().next().expect("top rule");
        // 中 is 2 columns. Inner 2 + 2 pad = 4 dashes, then ┬, then x is 1 + 2 pad = 3.
        assert_eq!(top, "┌────┬───┐", "{rendered}");
    }

    #[test]
    fn table_header_tag_survives_answer_line_style() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        use ratatui::widgets::Widget;

        let parts = render_markdown(table_md(), 40);
        let mut saw_header = false;
        for line in parts {
            let mut spans = vec![Span::raw("  ")];
            spans.extend(line.spans);
            let styled = Line::from(spans).style(theme::assistant_answer_style());
            let width = styled.width().max(1) as u16;
            let mut buf = Buffer::empty(Rect::new(0, 0, width, 1));
            Widget::render(styled, buf.area, &mut buf);
            for x in 0..width {
                if buf[(x, 0)].symbol() == "N" {
                    assert_eq!(
                        buf[(x, 0)].style().fg,
                        theme::tag_style(false).fg,
                        "header fg flattened by answer line style"
                    );
                    saw_header = true;
                }
            }
        }
        assert!(saw_header, "did not find header 'N' in painted buffer");
    }

    #[test]
    fn unterminated_fence_still_renders_its_code() {
        let streaming =
            "Here is the function:\n\n```rust\npub fn alpha() -> usize { 41 }\npub fn beta() -> usize { 42 }";
        let rendered = text(&render_markdown(streaming, 80));
        assert!(rendered.contains("```rust"), "{rendered}");
        assert!(rendered.contains("alpha"), "{rendered}");
        assert!(rendered.contains("beta"), "{rendered}");
        assert!(
            !rendered.contains("  ```\n") && !rendered.ends_with("  ```"),
            "no closing fence should be invented:\n{rendered}"
        );
    }
}
