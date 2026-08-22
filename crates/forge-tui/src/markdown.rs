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

/// How much of a streaming buffer is settled — safe to render once and cache.
///
/// Returns the byte offset up to which no byte that arrives later can change how
/// the text renders. Everything from there on must be re-rendered on each tick.
///
/// # Why this is conservative
///
/// Caching a half-parsed construct never corrects itself: the wrong lines are
/// frozen for the rest of the turn. Slow streaming is visible, wrong streaming
/// is not, so every uncertain case returns *less*. The cost of being too
/// cautious is a smaller speed-up; the cost of being too eager is corrupted
/// output.
///
/// # What keeps a block unsettled
///
/// "Ends in a newline" is not enough — a following line can reach backwards:
///
/// * an open fence: the closing ``` decides where code stops
/// * a table: the delimiter row turns the line above it into a header
/// * a list: a later item can make the whole list loose, re-spacing every item
/// * a block quote: a following `>` line continues it
/// * a setext heading: `Title` becomes a heading when `===` follows
/// * a trailing partial line: no newline yet, so nothing about it is fixed
///
/// So the unsettled region is the whole trailing block, extended back over a
/// run of list or quote blocks, or to an open fence's opening line.
pub fn settled_prefix_len(buffer: &str) -> usize {
    // A line without its newline is still being written.
    let Some(last_newline) = buffer.rfind('\n') else {
        return 0;
    };
    let complete = &buffer[..last_newline + 1];

    #[derive(Clone, Copy, PartialEq)]
    enum Kind {
        Other,
        ListOrQuote,
    }

    // (start offset, kind) for each block, plus the offset of an open fence.
    let mut blocks: Vec<(usize, Kind)> = Vec::new();
    let mut open_fence: Option<usize> = None;
    let mut offset = 0usize;
    let mut at_block_start = true;

    for line in complete.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let fence = trimmed.starts_with("```") || trimmed.starts_with("~~~");

        if let Some(start) = open_fence {
            // Inside a fence, blank lines are content and cannot end a block.
            if fence {
                open_fence = None;
            }
            let _ = start;
            offset += line.len();
            continue;
        }

        if line.trim().is_empty() {
            at_block_start = true;
            offset += line.len();
            continue;
        }

        if at_block_start {
            let kind = if is_list_or_quote(trimmed) {
                Kind::ListOrQuote
            } else {
                Kind::Other
            };
            blocks.push((offset, kind));
            at_block_start = false;
        }
        if fence {
            open_fence = Some(offset);
        }
        offset += line.len();
    }

    // An open fence swallows everything from where it opened.
    if let Some(fence_start) = open_fence {
        let starts: Vec<usize> = blocks.iter().map(|(start, _)| *start).collect();
        return block_start_at_or_before(&starts, fence_start);
    }

    // A trailing blank line closes the last block: nothing can reach back over
    // it — except a list, where a blank line only makes the list loose.
    let Some(&(last_start, last_kind)) = blocks.last() else {
        return complete.len();
    };
    // `at_block_start` is true exactly when the last line consumed was blank,
    // which is the only thing that closes a block.
    if at_block_start && last_kind == Kind::Other {
        return complete.len();
    }

    // Walk back over a contiguous run of list/quote blocks: a later item can
    // re-space every earlier one.
    let mut start = last_start;
    if last_kind == Kind::ListOrQuote {
        for &(block_start, kind) in blocks.iter().rev() {
            if kind != Kind::ListOrQuote {
                break;
            }
            start = block_start;
        }
    }
    start
}

fn is_list_or_quote(trimmed: &str) -> bool {
    if trimmed.starts_with('>') {
        return true;
    }
    let mut chars = trimmed.chars();
    match chars.next() {
        Some('-') | Some('*') | Some('+') => {
            matches!(chars.next(), Some(' ') | Some('\t') | None)
        }
        Some(c) if c.is_ascii_digit() => {
            let rest = trimmed.trim_start_matches(|c: char| c.is_ascii_digit());
            rest.starts_with(". ") || rest.starts_with(") ")
        }
        _ => false,
    }
}

fn block_start_at_or_before(starts: &[usize], offset: usize) -> usize {
    starts
        .iter()
        .rev()
        .copied()
        .find(|start| *start <= offset)
        .unwrap_or(offset)
}

/// Trailing blanks are separators with nothing after them; a finished render
/// drops them, and never returns an empty vector.
fn trim_trailing_blanks(mut out: Vec<Line<'static>>) -> Vec<Line<'static>> {
    while out.last().is_some_and(|line| line.width() == 0) {
        out.pop();
    }
    if out.is_empty() {
        out.push(Line::from(String::new()));
    }
    out
}

fn markdown_options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES
}

/// Render `text` keeping its trailing separator, so the result can be
/// continued. Only [`render_markdown_split`] and its cache should hold one.
pub(crate) fn render_markdown_open(text: &str, width: usize) -> Vec<Line<'static>> {
    let mut renderer = MdRenderer::new(width.max(1));
    renderer.feed(Parser::new_ext(text, markdown_options()));
    renderer.finish_open()
}

/// Join an open settled prefix with already-rendered open tail lines.
///
/// `settled_open` must come from [`render_markdown_open`] on exactly
/// `buffer[..cut]`, where `cut` is [`settled_prefix_len`]. Because that cut
/// lands on a top-level block boundary, the renderer's state there is its
/// initial state, so feeding only the tail produces the same events the whole
/// buffer would — and the separator is already in `settled_open`.
///
/// `settled_and_tail_render_as_the_whole` pins the equality.
pub(crate) fn render_markdown_join(
    settled_open: &[Line<'static>],
    tail: Vec<Line<'static>>,
) -> Vec<Line<'static>> {
    let mut out = settled_open.to_vec();
    out.extend(tail);
    trim_trailing_blanks(out)
}

/// The streaming caret appended to the live preview by `forge-transcript`.
pub(crate) const STREAM_CARET: char = '▌';

/// Dim the lines of the unsettled tail so a streaming answer visibly *sets*.
///
/// `settled_prefix_len` already knows exactly which suffix of the buffer later
/// bytes can still re-render — an open fence, a list that a further item can
/// re-space, a half-written paragraph. Painting that region one step down in
/// value is the only honest signal the transcript can give that the text on
/// screen is not final yet, and it costs one pass over the tail lines.
///
/// The caret keeps its own colour: it marks the live edge, so fading it would
/// hide the one thing that is definitely alive.
pub(crate) fn fade_streaming_tail(lines: &mut [Line<'static>]) {
    let dim = theme::text_dim_color();
    for line in lines.iter_mut() {
        for span in &mut line.spans {
            span.style = span.style.fg(dim);
        }
    }
    // An open render keeps its trailing separator, so the caret is on the last
    // line that has any width, not necessarily the last line.
    let Some(last) = lines.iter_mut().rev().find(|line| line.width() > 0) else {
        return;
    };
    let Some(span) = last.spans.last_mut() else {
        return;
    };
    if !span.content.ends_with(STREAM_CARET) {
        return;
    }
    let body = span
        .content
        .strip_suffix(STREAM_CARET)
        .expect("checked above")
        .to_string();
    span.content = body.into();
    last.spans
        .push(Span::styled(STREAM_CARET.to_string(), theme::text()));
}

pub fn render_markdown(text: &str, width: usize) -> Vec<Line<'static>> {
    let mut renderer = MdRenderer::new(width.max(1));
    renderer.feed(Parser::new_ext(text, markdown_options()));
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
    /// Rank of the heading currently open, so `TagEnd::Heading` can decide
    /// whether to draw the H1 rule.
    heading_level: Option<u8>,
}

/// Map `pulldown_cmark`'s heading level onto 1-6.
fn heading_rank(level: pulldown_cmark::HeadingLevel) -> u8 {
    use pulldown_cmark::HeadingLevel::*;
    match level {
        H1 => 1,
        H2 => 2,
        H3 => 3,
        H4 => 4,
        H5 => 5,
        H6 => 6,
    }
}

/// Open a block with one blank line of separation, never two, and never a
/// leading blank at the very top of the answer.
/// Marks an H2 in the margin, where H1 has a rule under it instead.
const HEADING_BAR: &str = "▌ ";

fn blank_before_block(out: &mut Vec<Line<'static>>) {
    match out.last() {
        None => {}
        Some(last) if last.width() == 0 => {}
        Some(_) => out.push(Line::from("")),
    }
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
            heading_level: None,
        }
    }

    fn feed(&mut self, parser: Parser<'_>) {
        for event in parser {
            match event {
                Event::Start(tag) => self.on_start(tag),
                Event::End(tag) => self.on_end(tag),
                Event::Text(t) => self.on_text(t.into_string()),
                Event::Code(t) => self
                    .inline
                    .push(Span::styled(t.into_string(), theme::inline_code())),
                Event::InlineHtml(t) => self.push_span(t.into_string()),
                Event::Html(t) => {
                    if self.in_html_block {
                        self.html_buf.push_str(&t);
                    } else {
                        self.push_span(t.into_string());
                    }
                }
                // A soft break is a word boundary in the source, so it has to
                // leave one behind. Dropping it was harmless while wrapping
                // spaced every token unconditionally; now that adjacent spans
                // with no whitespace between them are deliberately glued (so
                // `foo`. keeps its full stop), a dropped break reads as glue
                // and renders "gate.Shell".
                Event::SoftBreak => self.push_span(" ".into()),
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
            Tag::Heading { level, .. } => {
                // Rank reads by weight and value, never hue (see
                // `theme::heading`). H1 and H2 stay primary text; H3 and below
                // step down to secondary, and H1 gets a rule under it. Before
                // this, every level was the same bold line, so a structured
                // answer rendered flat.
                let level = heading_rank(level);
                self.heading_level = Some(level);
                blank_before_block(&mut self.out);
                // H1 is told apart by the rule under it. H2 had only bold,
                // which in a monospace face at terminal sizes is nearly no
                // signal against body text — a section heading read as another
                // paragraph. A bar in the margin is ornament rather than more
                // weight, so rank still reads without spending hue.
                if level == 2 {
                    self.prefix = HEADING_BAR.into();
                    self.cont_prefix = " ".repeat(display_width(HEADING_BAR));
                }
                self.push_style(if level <= 2 {
                    theme::heading()
                } else {
                    theme::text_secondary().add_modifier(Modifier::BOLD)
                });
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
                    indent: display_width(&self.cont_prefix),
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
                    // Columns, not bytes: the bullet marker `• ` is four bytes
                    // wide and two columns wide, so `.len()` pushed every
                    // wrapped line two columns past the text it continues.
                    frame.marker_w = display_width(&marker);
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
                self.prefix.clear();
                self.cont_prefix.clear();
                if self.heading_level.take() == Some(1) {
                    self.out.push(Line::from(Span::styled(
                        "─".repeat(self.width),
                        theme::border_muted(),
                    )));
                }
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

    fn finish(self) -> Vec<Line<'static>> {
        trim_trailing_blanks(self.finish_open())
    }

    /// Everything `finish` does except the trailing-blank trim.
    ///
    /// A top-level paragraph pushes a blank line *after* itself, and `finish`
    /// strips those from the very end. That blank is what separates it from
    /// whatever comes next, so a prefix rendered with `finish` has already lost
    /// its separator and cannot be concatenated with a continuation. Keeping
    /// the open form is what makes [`render_markdown_split`] exact.
    fn finish_open(mut self) -> Vec<Line<'static>> {
        self.flush_para();
        if let Some(code) = self.code.take() {
            self.render_code(code);
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

    /// One row of a fenced block: indent, gutter rule, content, then padding
    /// out to the prose width so the tint reads as a block rather than as a
    /// ragged highlight behind the text.
    /// The language label above a fenced block: outside the gutter, on the
    /// canvas rather than the code tint, right-aligned to the block's edge so
    /// it caps the block instead of starting it.
    fn language_chip(&self, language: &str) -> Line<'static> {
        let label = language.to_string();
        let pad = self
            .width
            .saturating_sub(display_width(&label))
            .saturating_sub(display_width(CODE_INDENT));
        Line::from(vec![
            Span::raw(format!("{CODE_INDENT}{}", " ".repeat(pad))),
            Span::styled(label, theme::code_punctuation()),
        ])
    }

    fn code_row(&self, content: Vec<Span<'static>>) -> Line<'static> {
        let mut spans = vec![Span::styled(
            format!("{CODE_INDENT}{CODE_GUTTER}"),
            theme::code_gutter(),
        )];
        let mut used = display_width(CODE_INDENT) + display_width(CODE_GUTTER);
        for span in content {
            used += span.width();
            spans.push(span);
        }
        if used < self.width {
            spans.push(Span::raw(" ".repeat(self.width - used)));
        }
        Line::from(spans).style(theme::chat_code_block())
    }

    /// Render a code block as a block.
    ///
    /// This used to print the source fence — a literal ```` ``` ```` line —
    /// above the body, which put raw markdown syntax in rendered output and,
    /// because no closing fence is emitted, left the block with no visible end:
    /// the next paragraph of prose ran straight into the code. The tint, the
    /// gutter and the language chip carry the same information without
    /// borrowing the author's syntax, and the tinted rows show where the block
    /// stops.
    fn render_code(&mut self, code: CodeBuffer) {
        if code.fenced && !code.language.is_empty() {
            // The chip used to go through `code_row`, which put it inside the
            // gutter and gave it the code tint — so it read as a line of code
            // that says "rust". It belongs above the block, outside the rail,
            // right-aligned to the block's own width.
            self.out.push(self.language_chip(&code.language));
        }
        let body = code.body.trim_end_matches('\n');
        if body.is_empty() {
            return;
        }
        let theme = theme::syntax_theme();
        for line_segments in highlight_to_lines(&code.language, body, &theme).iter() {
            let row = self.code_row(render_highlighted_line(line_segments));
            self.out.push(row);
        }
    }
}

/// Left inset of a fenced code block, matching the prose inset.
const CODE_INDENT: &str = "  ";
/// Rule drawn down the left edge of every row of a fenced block.
const CODE_GUTTER: &str = "▌ ";

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

    for Token { word, style, glued } in tokenize(spans) {
        let mut remaining = word.as_str();
        // `glued` only ever suppresses the separating space, and that space is
        // only considered while `has_content` holds. Every wrap below clears
        // `has_content`, so a token that hard-breaks onto a fresh line cannot
        // pick up a stray space from having been glued.
        while !remaining.is_empty() {
            let gap = usize::from(has_content && !glued);
            let room = width.saturating_sub(cur_w + gap);
            let wlen = display_width(remaining);
            if wlen <= room {
                if has_content && !glued {
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

/// A word to place, its style, and whether it was written flush against the
/// word before it.
struct Token {
    word: String,
    style: Style,
    /// No whitespace separated this token from its predecessor in the source.
    glued: bool,
}

/// Split a styled run into words, remembering where the source had no space.
///
/// Tokenising each span on its own loses that: inline code is its own span and
/// the `.` after it is another, so two characters written flush against each
/// other came back as separate words and the wrapper rejoined them with a
/// space — `ZeroDivisionError .`. Whitespace only ever disappears *between*
/// spans, so the run has to be walked as a whole, carrying whether the previous
/// span ended on whitespace.
fn tokenize(spans: &[Span<'static>]) -> Vec<Token> {
    let mut out: Vec<Token> = Vec::new();
    // Leading whitespace is not a join, so the first token is never glued.
    let mut prev_ended_ws = true;
    for span in spans {
        let content = span.content.as_ref();
        let starts_ws = content.starts_with(char::is_whitespace);
        let mut words = content.split_whitespace();
        if let Some(first) = words.next() {
            out.push(Token {
                word: first.to_string(),
                style: span.style,
                glued: !starts_ws && !prev_ended_ws,
            });
            for word in words {
                out.push(Token {
                    word: word.to_string(),
                    style: span.style,
                    glued: false,
                });
            }
            prev_ended_ws = content.ends_with(char::is_whitespace);
        } else if !content.is_empty() {
            // Whitespace-only span: it separates, it does not produce a word.
            prev_ended_ws = true;
        }
    }
    out
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
    let block = theme::chat_code_block();
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

    /// H1 is told apart by its rule. H2 had only bold, which at terminal
    /// sizes reads as body text — a section heading that announces nothing.
    #[test]
    fn a_second_level_heading_is_marked_in_the_margin() {
        let lines = render_markdown("# Title\n\n## Section\n\nbody text\n", 60);
        let text: Vec<String> = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();

        assert!(
            text.iter().any(|row| row.starts_with("▌ Section")),
            "H2 should carry a bar: {text:?}"
        );
        // H1 keeps the rule and does not also get a bar — two markers for one
        // level would read as two levels.
        assert!(
            text.iter().any(|row| row.starts_with("Title")),
            "H1 should stay unmarked: {text:?}"
        );
        assert!(
            text.iter().any(|row| row.starts_with("───")),
            "H1 should keep its rule: {text:?}"
        );
        // The bar belongs to the heading, not to what follows it.
        assert!(
            text.iter().any(|row| row.starts_with("body text")),
            "body should not inherit the bar: {text:?}"
        );
    }

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
        assert_eq!(code_span.style.fg, theme::inline_code().fg);
        assert_eq!(code_span.style.bg, theme::inline_code().bg);
    }

    /// Inline code carries file paths and identifiers — the tokens a reader
    /// acts on. Rendering them below the prose they sit in was backwards.
    #[test]
    fn inline_code_is_not_dimmer_than_the_prose_around_it() {
        let rendered = render_markdown("see `src/stats.py` for it", 80);
        let spans: Vec<&Span<'static>> = rendered.iter().flat_map(|l| l.spans.iter()).collect();
        let code = spans
            .iter()
            .find(|s| s.content.as_ref() == "src/stats.py")
            .expect("code token");
        let prose = spans
            .iter()
            .find(|s| s.content.as_ref() == "see")
            .expect("prose token");

        assert_eq!(
            code.style.fg,
            Some(theme::text_primary_color()),
            "code should read at primary weight of colour"
        );
        assert_ne!(
            code.style.bg, prose.style.bg,
            "the tint is what marks it as code"
        );
        // The accent belongs to focus, never to content.
        assert_ne!(code.style.fg, Some(theme::accent_color()));
        // Bold now means `**bold**`; code must not compete for it.
        assert!(!code.style.add_modifier.contains(Modifier::BOLD));
    }

    /// A wrapped bullet must line up under the bullet's text. `marker_w` was
    /// measured in bytes, and `• ` is four bytes for two columns, so every
    /// continuation hung two columns to the right.
    #[test]
    fn a_wrapped_bullet_aligns_under_its_text() {
        let rendered = text(&render_markdown(
            "- median() mishandles even-length inputs by returning the upper value",
            40,
        ));
        let mut lines = rendered.lines();
        let first = lines.next().expect("bullet line");
        let cont = lines.next().expect("continuation line");

        // Columns, not bytes — `str::find` would report 4 for the 3-byte
        // bullet, which is the very confusion under test.
        let text_col = first[..first.find("median").expect("bullet text on first line")]
            .chars()
            .count();
        let cont_col = cont.chars().take_while(|c| *c == ' ').count();
        assert_eq!(
            cont_col, text_col,
            "continuation must align with the bullet text:\n{rendered}"
        );
    }

    /// Ordered markers are ASCII, so they were always right — pin them so the
    /// column fix cannot regress them.
    #[test]
    fn a_wrapped_numbered_item_aligns_under_its_text() {
        let rendered = text(&render_markdown(
            "1. median() mishandles even-length inputs by returning the upper value",
            40,
        ));
        let mut lines = rendered.lines();
        let first = lines.next().expect("item line");
        let cont = lines.next().expect("continuation line");

        let text_col = first[..first.find("median").expect("item text on first line")]
            .chars()
            .count();
        let cont_col = cont.chars().take_while(|c| *c == ' ').count();
        assert_eq!(cont_col, text_col, "{rendered}");
    }

    /// A newline inside a paragraph is a word boundary, not glue. Models wrap
    /// prose at arbitrary columns, so this is the common case, and losing the
    /// space renders "gate.Shell" mid-sentence.
    #[test]
    fn a_soft_break_keeps_the_words_apart() {
        let rendered = render_markdown(
            "The shell execution was blocked by the approval gate.\nShell execution was denied.",
            120,
        );
        let out = text(&rendered);
        assert!(out.contains("gate. Shell"), "{out:?}");
    }

    /// The same tokenizer must still glue punctuation onto inline code, which
    /// is the behaviour the soft-break fix has to not regress.
    #[test]
    fn inline_code_still_keeps_its_trailing_punctuation() {
        let out = text(&render_markdown("Raises `ZeroDivisionError`.", 80));
        assert!(out.contains("ZeroDivisionError."), "{out:?}");
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
        // The list continuation indent is the marker's *column* width, so the
        // two-column `• ` puts the table at 2. It used to sit at 4, because
        // `marker.len()` measured the bullet's four bytes.
        assert!(
            top.starts_with("  ┌"),
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

    /// Inline code and the punctuation written flush against it must stay
    /// flush. They arrive as two spans, and rejoining every span boundary with
    /// a space produced `ZeroDivisionError .` in almost every answer.
    #[test]
    fn punctuation_stays_attached_to_inline_code() {
        let rendered = text(&render_markdown(
            "`mean([])` raises `ZeroDivisionError`, and `median([])` raises `IndexError`.",
            120,
        ));
        assert!(
            rendered.contains("raises ZeroDivisionError, and"),
            "comma drifted off the code span:\n{rendered}"
        );
        assert!(
            rendered.contains("raises IndexError."),
            "full stop drifted off the code span:\n{rendered}"
        );
        assert!(
            !rendered.contains(" ,") && !rendered.contains(" ."),
            "a span boundary was rejoined with a space:\n{rendered}"
        );
    }

    /// The flip side: whitespace that *was* in the source must survive, whether
    /// it sat inside a span or between two of them.
    #[test]
    fn real_whitespace_between_styled_runs_is_kept() {
        let rendered = text(&render_markdown("**bold** *italic* `code` plain", 120));
        assert_eq!(rendered.trim(), "bold italic code plain", "{rendered}");
    }

    /// A glued token that lands at a wrap point starts the next line without
    /// inheriting a separating space.
    #[test]
    fn gluing_does_not_leak_a_space_across_a_wrap() {
        let rendered = text(&render_markdown("aaaa `bbbb`, cccc", 10));
        for line in rendered.lines() {
            assert!(
                !line.starts_with(' ') || line.trim().is_empty(),
                "wrapped line opened on a stray space:\n{rendered}"
            );
        }
        assert!(rendered.contains("bbbb,"), "{rendered}");
    }

    /// Painted through the answer line style, an emphasised word must come out
    /// heavier than the prose beside it. This failed while the line style was
    /// itself bold: every cell was bold, so `**does**` and the words around it
    /// were the same weight.
    #[test]
    fn strong_emphasis_survives_the_answer_line_style() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        use ratatui::widgets::Widget;

        let parts = render_markdown("it **does** matter", 40);
        let line = parts.into_iter().next().expect("one rendered line");
        let styled = Line::from(line.spans).style(theme::assistant_answer_style());
        let width = styled.width().max(1) as u16;
        let mut buf = Buffer::empty(Rect::new(0, 0, width, 1));
        Widget::render(styled, buf.area, &mut buf);

        let bold_at = |needle: char| {
            (0..width)
                .find(|x| buf[(*x, 0)].symbol() == needle.to_string())
                .map(|x| buf[(x, 0)].style().add_modifier.contains(Modifier::BOLD))
                .expect("character painted")
        };
        // 'd' only occurs inside "does"; 'm' only inside "matter".
        assert!(bold_at('d'), "emphasised word lost its weight");
        assert!(!bold_at('m'), "ordinary prose came out bold");
    }

    #[test]
    fn unterminated_fence_still_renders_its_code() {
        let streaming =
            "Here is the function:\n\n```rust\npub fn alpha() -> usize { 41 }\npub fn beta() -> usize { 42 }";
        let rendered = text(&render_markdown(streaming, 80));
        assert!(rendered.contains("rust"), "{rendered}");
        assert!(rendered.contains("alpha"), "{rendered}");
        assert!(rendered.contains("beta"), "{rendered}");
        assert!(
            !rendered.contains("```"),
            "the source fence must not reach rendered output:\n{rendered}"
        );
        // Every row of the block carries the gutter; nothing stands in for a
        // closing fence, because none was written.
        let gutters = rendered
            .lines()
            .filter(|l| l.trim_start().starts_with(CODE_GUTTER.trim_end()))
            .count();
        assert_eq!(
            gutters, 2,
            "two code lines; the chip sits outside the gutter:\n{rendered}"
        );
    }

    /// The block must read as a block: a language chip, a gutter down every
    /// row, and a tint that runs the full prose width so prose after the code
    /// cannot look like part of it.
    #[test]
    fn a_fenced_block_renders_as_a_tinted_block() {
        let lines = render_markdown("Intro.\n\n```python\nx = 1\n```\n\nAfter.\n", 40);
        let rendered = text(&lines);
        assert!(rendered.contains("python"), "{rendered}");
        assert!(!rendered.contains("```"), "{rendered}");

        let block_bg = theme::chat_code_block().bg;
        let tinted: Vec<&Line<'static>> = lines
            .iter()
            .filter(|line| line.style.bg == block_bg && block_bg.is_some())
            .collect();
        assert_eq!(
            tinted.len(),
            1,
            "one code row; the chip is not part of the tinted block:\n{rendered}"
        );
        for line in tinted {
            assert_eq!(
                line.width(),
                40,
                "a tinted row must fill the prose width:\n{rendered}"
            );
        }
        assert!(
            rendered.lines().any(|l| l.trim() == "After."),
            "prose after the block must stay untinted prose:\n{rendered}"
        );
    }

    /// Every heading level used to push the same bold style, so an answer with
    /// real structure rendered flat: H1, H2 and H3 were pixel-identical.
    #[test]
    fn heading_levels_are_told_apart() {
        let lines = render_markdown("# One\n\n## Two\n\n### Three\n", 40);
        let style_of = |needle: &str| {
            lines
                .iter()
                .find(|line| line.spans.iter().any(|span| span.content.contains(needle)))
                .map(|line| {
                    line.spans
                        .iter()
                        .find(|s| s.content.contains(needle))
                        .unwrap()
                        .style
                })
                .unwrap_or_else(|| panic!("{needle} rendered"))
        };
        let h1 = style_of("One");
        let h3 = style_of("Three");
        assert!(
            h1.add_modifier.contains(Modifier::BOLD),
            "H1 lost its weight"
        );
        assert!(
            h3.add_modifier.contains(Modifier::BOLD),
            "H3 lost its weight"
        );
        assert_ne!(
            h1.fg, h3.fg,
            "H3 must step down in value from H1, or rank is invisible"
        );
        // The H1 rule runs the full prose width; H2 and H3 get none.
        let rules = lines
            .iter()
            .filter(|line| {
                let text = line
                    .spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>();
                !text.is_empty() && text.chars().all(|c| c == '─')
            })
            .count();
        assert_eq!(rules, 1, "exactly the H1 gets a rule");
    }

    /// A heading opens with one blank line of separation, never two and never
    /// a leading blank at the top of the answer.
    #[test]
    fn a_heading_never_stacks_blank_lines() {
        let rendered = text(&render_markdown("Intro.\n\n## Section\n\nBody.\n", 40));
        assert!(!rendered.starts_with('\n'), "leading blank:\n{rendered}");
        assert!(!rendered.contains("\n\n\n"), "doubled blanks:\n{rendered}");
    }

    /// The chip names the language; it is not a line of code that says "rust".
    #[test]
    fn the_language_chip_sits_outside_the_gutter() {
        let lines = render_markdown("```rust\nfn main() {}\n```\n", 40);
        let chip = lines
            .iter()
            .find(|line| {
                line.spans
                    .iter()
                    .any(|span| span.content.as_ref() == "rust")
            })
            .expect("chip rendered");
        let text: String = chip.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            !text.contains(CODE_GUTTER.trim_end()),
            "the chip is inside the code rail: {text:?}"
        );
        assert_eq!(chip.style.bg, None, "the chip must not carry the code tint");
        assert!(
            text.trim_end().ends_with("rust"),
            "the chip caps the block on the right: {text:?}"
        );
    }

    fn lines_text(lines: &[Line<'static>]) -> String {
        lines
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

    fn render_markdown_split(
        settled_open: &[Line<'static>],
        tail: &str,
        width: usize,
    ) -> Vec<Line<'static>> {
        render_markdown_join(settled_open, render_markdown_open(tail, width))
    }

    /// Helper: the settled prefix, as text, so cases read as intent.
    fn settled(buffer: &str) -> &str {
        &buffer[..settled_prefix_len(buffer)]
    }

    #[test]
    fn a_partial_final_line_is_never_settled() {
        assert_eq!(settled("no newline at all"), "");
        // Lines without a blank between them are one paragraph, so none of it
        // is settled while it is still the trailing block.
        assert_eq!(settled("one\ntwo\nthree without a newline"), "");
        // With a completed block in front, only that block settles.
        assert_eq!(settled("Done.\n\nstill writing this line"), "Done.\n\n");
    }

    /// The construct that matters most: an open fence must keep its whole block
    /// unsettled, because the closing marker decides where code stops.
    #[test]
    fn an_open_fence_holds_back_from_where_it_opened() {
        let buffer = "Here is the fix.\n\n```rust\nfn a() {}\nfn b() {}\n";
        assert_eq!(settled(buffer), "Here is the fix.\n\n");
    }

    #[test]
    fn a_closed_fence_settles_once_a_later_block_starts() {
        // No trailing blank line: "After the block." is still the live block.
        let buffer = "Intro.\n\n```rust\nfn a() {}\n```\n\nAfter the block.\n";
        let settled = settled(buffer);
        assert!(
            settled.contains("```rust") && settled.contains("fn a() {}"),
            "a closed fence cannot change any more: {settled:?}"
        );
        assert!(
            !settled.contains("After the block"),
            "the trailing block stays live: {settled:?}"
        );
    }

    /// A delimiter row turns the line above it into a header, so a table in
    /// flight must not be cached a row at a time.
    #[test]
    fn a_table_in_flight_is_not_settled() {
        let buffer = "Results:\n\n| col | col |\n| --- | --- |\n| a | b |\n";
        assert_eq!(settled(buffer), "Results:\n\n");
    }

    /// A later item can make the whole list loose, which re-spaces every item
    /// already on screen — so the run extends back over blank lines.
    #[test]
    fn a_list_run_is_held_back_across_blank_lines() {
        let buffer = "Steps:\n\n- first\n\n- second\n";
        assert_eq!(
            settled(buffer),
            "Steps:\n\n",
            "a blank line inside a list does not end it"
        );
    }

    #[test]
    fn a_block_quote_run_is_held_back() {
        let buffer = "Quoting:\n\n> one\n\n> two\n";
        assert_eq!(settled(buffer), "Quoting:\n\n");
    }

    /// `Title` becomes a heading only when `===` arrives on the next line, so
    /// the trailing paragraph is never settled while it is still the last block.
    #[test]
    fn a_trailing_paragraph_could_still_become_a_setext_heading() {
        let buffer = "Intro paragraph.\n\nTitle\n";
        assert_eq!(settled(buffer), "Intro paragraph.\n\n");
    }

    #[test]
    fn a_completed_block_followed_by_a_blank_line_is_settled() {
        let buffer = "First paragraph.\n\n";
        assert_eq!(settled(buffer), buffer, "nothing can reach back over it");
    }

    /// The property the cache depends on: the boundary only ever moves forward
    /// as more text arrives, so cached lines are never invalidated.
    #[test]
    fn the_boundary_never_moves_backwards_as_text_arrives() {
        let full = "Intro.\n\n- a\n- b\n\nProse here.\n\n```rust\nfn x() {}\n```\n\nDone.\n\n";
        let mut previous = 0usize;
        for end in 1..=full.len() {
            if !full.is_char_boundary(end) {
                continue;
            }
            let settled = settled_prefix_len(&full[..end]);
            assert!(
                settled >= previous,
                "boundary went backwards at {end}: {previous} -> {settled}"
            );
            assert!(settled <= end, "boundary ran past the buffer at {end}");
            previous = settled;
        }
    }

    /// Whatever is declared settled must render identically on its own as it
    /// does inside the full buffer — otherwise caching it changes the output.
    #[test]
    fn the_settled_prefix_renders_the_same_alone_as_in_context() {
        for buffer in [
            "Intro.\n\n- a\n- b\n\nProse.\n\n```rust\nfn x() {}\n```\n\nTail.\n",
            "One.\n\nTwo.\n\n| a | b |\n| - | - |\n",
            "Text.\n\n> quoted\n",
        ] {
            let cut = settled_prefix_len(buffer);
            let alone = lines_text(&render_markdown(&buffer[..cut], 80));
            let in_context = lines_text(&render_markdown(buffer, 80));
            assert!(
                in_context.starts_with(alone.trim_end()) || alone.trim().is_empty(),
                "settled prefix renders differently in context\n--- alone ---\n{alone}\n--- in context ---\n{in_context}"
            );
        }
    }

    /// The property the cache rests on: a settled prefix rendered *open*, plus
    /// the tail, must equal rendering the whole buffer.
    ///
    /// The earlier attempt concatenated two *finished* renders and lost the
    /// separator, because a paragraph's trailing blank is stripped by `finish`.
    /// Keeping the prefix open is what makes this exact — including after a code
    /// block, which emits no trailing blank at all.
    #[test]
    fn settled_and_tail_render_as_the_whole() {
        let samples = [
            "Intro.\n\n- a\n- b\n\nProse.\n\n```rust\nfn x() {}\n```\n\nTail.\n",
            "One.\n\nTwo.\n\n| a | b |\n| - | - |\n| 1 | 2 |\n",
            "Text.\n\n> quoted\n\n> more\n",
            "Para.\n\n```rust\nfn open() {\n",
            "# Heading\n\nBody text here.\n\nAnother paragraph.\n\n",
            "```rust\nfn x() {}\n```\n\nAfter code.\n",
            "Just one unfinished line",
            "",
        ];
        for buffer in samples {
            for width in [40usize, 80] {
                let cut = settled_prefix_len(buffer);
                let settled_open = render_markdown_open(&buffer[..cut], width);
                let split =
                    lines_text(&render_markdown_split(&settled_open, &buffer[cut..], width));
                let whole = lines_text(&render_markdown(buffer, width));
                assert_eq!(
                    split, whole,
                    "split differs at width {width}, cut {cut}, for {buffer:?}"
                );
            }
        }
    }

    /// Every prefix of a realistic answer, not just the hand-picked ones.
    #[test]
    fn every_streaming_prefix_renders_identically_when_split() {
        let full = "Here is the plan.\n\n- step one\n- step two\n\nNow the code:\n\n```rust\nfn apply(x: usize) -> usize {\n    x + 1\n}\n```\n\nAnd a table:\n\n| a | b |\n| - | - |\n| 1 | 2 |\n\nDone.\n";
        for end in 0..=full.len() {
            if !full.is_char_boundary(end) {
                continue;
            }
            let buffer = &full[..end];
            let cut = settled_prefix_len(buffer);
            let settled_open = render_markdown_open(&buffer[..cut], 60);
            let split = lines_text(&render_markdown_split(&settled_open, &buffer[cut..], 60));
            let whole = lines_text(&render_markdown(buffer, 60));
            assert_eq!(split, whole, "split differs at prefix length {end}");
        }
    }
}
