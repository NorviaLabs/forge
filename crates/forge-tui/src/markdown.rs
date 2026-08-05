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
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

const TABLE_SEP: &str = " │ ";

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

struct TableBuilder {
    rows: Vec<Vec<Vec<Span<'static>>>>,
}

impl TableBuilder {
    fn new() -> Self {
        TableBuilder { rows: Vec::new() }
    }

    fn start_row(&mut self) {
        self.rows.push(Vec::new());
    }

    fn end_cell(&mut self, spans: Vec<Span<'static>>) {
        if let Some(row) = self.rows.last_mut() {
            row.push(spans);
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
            Tag::Table(_) => {
                self.flush_para();
                self.table = Some(TableBuilder::new());
            }
            Tag::TableHead | Tag::TableRow => {
                if let Some(table) = &mut self.table {
                    table.start_row();
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
                    self.out.extend(render_table(&table, self.width));
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

/// Greedy word-wrap `spans` to `width`, prefixing the first line with `prefix`
/// and continuation lines with `cont`. Styles travel with the words, so inline
/// code and emphasis stay styled across wraps.
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
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut cur: Vec<Span<'static>> = vec![prefix_span];
    let mut cur_w = prefix.len();
    let mut has_content = false;
    for (word, style) in spans.iter().flat_map(tokenize) {
        let wlen = word.len();
        if has_content && cur_w + 1 + wlen > width {
            out.push(Line::from(std::mem::take(&mut cur)));
            cur.push(cont_span.clone());
            cur_w = cont.len();
            has_content = false;
        }
        if has_content {
            cur.push(Span::raw(" "));
            cur_w += 1;
        }
        cur.push(Span::styled(word, style));
        cur_w += wlen;
        has_content = true;
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

fn render_table(table: &TableBuilder, width: usize) -> Vec<Line<'static>> {
    let col_count = table.rows.iter().map(|row| row.len()).max().unwrap_or(0);
    if col_count == 0 {
        return Vec::new();
    }
    let mut rows: Vec<Vec<Vec<Span<'static>>>> = Vec::with_capacity(table.rows.len());
    for row in &table.rows {
        let mut padded = row.clone();
        padded.resize(col_count, Vec::new());
        rows.push(padded);
    }
    let cell_len =
        |cell: &[Span<'static>]| cell.iter().map(|span| span.content.len()).sum::<usize>();
    let mut natural = vec![0usize; col_count];
    for row in &rows {
        for (column, cell) in row.iter().enumerate() {
            natural[column] = natural[column].max(cell_len(cell));
        }
    }
    let sep_total = TABLE_SEP.len() * col_count.saturating_sub(1);
    let widths = if natural.iter().sum::<usize>() + sep_total <= width {
        natural
    } else {
        shrink_widths(&natural, width.saturating_sub(sep_total))
    };

    let mut out = Vec::new();
    if let Some(header) = rows.first() {
        out.extend(render_row(header, &widths));
        let rule: String = widths
            .iter()
            .map(|w| "─".repeat(*w))
            .collect::<Vec<_>>()
            .join("─┼─");
        out.push(Line::from(Span::styled(rule, theme::border_muted())));
    }
    for row in rows.iter().skip(1) {
        out.extend(render_row(row, &widths));
    }
    out
}

/// Distribute available width across columns, keeping at least the narrowest
/// natural content visible and shrinking the widest columns first.
fn shrink_widths(natural: &[usize], available: usize) -> Vec<usize> {
    let mut widths = natural
        .iter()
        .copied()
        .map(|n| n.min(4))
        .collect::<Vec<usize>>();
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

fn render_row(cells: &[Vec<Span<'static>>], widths: &[usize]) -> Vec<Line<'static>> {
    let wrapped: Vec<Vec<Line<'static>>> = cells
        .iter()
        .zip(widths)
        .map(|(cell, w)| wrap_spans(cell, *w, "", ""))
        .collect();
    let height = wrapped.iter().map(|lines| lines.len()).max().unwrap_or(1);
    let mut out = Vec::new();
    for row_line in 0..height {
        let mut spans: Vec<Span<'static>> = Vec::new();
        for (column, lines) in wrapped.iter().enumerate() {
            if column > 0 {
                spans.push(Span::raw(TABLE_SEP));
            }
            let mut line = lines.get(row_line).cloned().unwrap_or_default();
            let used: usize = line.spans.iter().map(|s| s.content.len()).sum();
            if used < widths[column] {
                line.spans
                    .push(Span::raw(" ".repeat(widths[column] - used)));
            }
            spans.extend(line.spans);
        }
        out.push(Line::from(spans));
    }
    out
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
        assert!(rendered.contains("┼"), "{rendered}");
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
