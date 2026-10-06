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

use crate::links::{
    autolink_matches, destination_for, link_label_style, HyperlinkLine, TerminalHyperlink,
};
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
fn trim_trailing_blanks(mut out: Vec<HyperlinkLine>) -> Vec<HyperlinkLine> {
    while out.last().is_some_and(|line| line.width() == 0) {
        out.pop();
    }
    if out.is_empty() {
        out.push(Line::from(String::new()).into());
    }
    out
}

fn markdown_options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES
}

/// Transcript density (FORGE-DESIGN §7.5).
///
/// `Airy` is the app default at comfortable pane heights: extra breathing
/// room around headings and fenced code. `Compact` is the
/// historical spacing, used as the fallback on short terminals (and pinned by
/// older renderer tests).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Density {
    #[default]
    Compact,
    Airy,
}

pub(crate) fn render_markdown_open_with(
    text: &str,
    width: usize,
    density: Density,
) -> Vec<Line<'static>> {
    strip_links(render_markdown_open_links(text, width, density))
}

/// [`render_markdown_open_with`], keeping the destinations.
pub(crate) fn render_markdown_open_links(
    text: &str,
    width: usize,
    density: Density,
) -> Vec<HyperlinkLine> {
    let mut renderer = MdRenderer::new(width.max(1), density);
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
    let mut out: Vec<Line<'static>> = settled_open.to_vec();
    out.extend(tail);
    // The same trim `finish` performs, on the plain form: the streaming path
    // joins text that was never asked to carry destinations.
    while out.last().is_some_and(|line| line.width() == 0) {
        out.pop();
    }
    if out.is_empty() {
        out.push(Line::from(String::new()));
    }
    out
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
    render_markdown_with_density(text, width, Density::Compact)
}

/// Render `text` with each link's destination carried beside its columns.
///
/// Callers that paint into a terminal buffer want this one: the destination has
/// to survive layout, because an OSC 8 sequence written into a `Span` would be
/// measured as columns and shift every wrapped line after it.
pub(crate) fn render_markdown_links(text: &str, width: usize) -> Vec<HyperlinkLine> {
    render_markdown_with_density_links(text, width, Density::Compact)
}

pub(crate) fn render_markdown_with_density(
    text: &str,
    width: usize,
    density: Density,
) -> Vec<Line<'static>> {
    strip_links(render_markdown_with_density_links(text, width, density))
}

pub(crate) fn render_markdown_with_density_links(
    text: &str,
    width: usize,
    density: Density,
) -> Vec<HyperlinkLine> {
    let mut renderer = MdRenderer::new(width.max(1), density);
    renderer.feed(Parser::new_ext(text, markdown_options()));
    renderer.finish()
}

/// Drop link metadata for callers that only want the text. Measuring, copying
/// and counting all read the `Line`; only the terminal buffer needs the rest.
fn strip_links(lines: Vec<HyperlinkLine>) -> Vec<Line<'static>> {
    lines.into_iter().map(|line| line.line).collect()
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
    out: Vec<HyperlinkLine>,
    inline: Vec<Span<'static>>,
    /// Destination per entry of `inline`, index for index. Links travel beside
    /// the spans rather than inside them: an OSC 8 sequence in a `Span` would
    /// be counted by `Span::width` and shift every column after it.
    inline_links: Vec<Option<String>>,
    /// Destination of the link currently open, if any.
    current_link: Option<String>,
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
    /// whether to draw the rank rule.
    heading_level: Option<u8>,
    /// Style for the run-of-lines prefix (list markers, quote rails): bullets
    /// read as answer structure and take the structure hue; every other
    /// prefix stays muted.
    marker_style: Style,
    /// Out-line index where the outermost open list began, while a top-level
    /// list block is open — closed lists get the scan-band ground painted
    /// across their whole line range (`band_depth` nests).
    band_start: Option<usize>,
    band_depth: usize,
    /// Airy adds breathing room around headings and code; compact
    /// is the historical spacing for short terminals.
    density: Density,
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

/// Append one blank separator line unless the last line is already blank.
/// No-op at the very top, so a block never opens with a leading blank.
fn ensure_blank_separator(out: &mut Vec<HyperlinkLine>) {
    match out.last() {
        None => {}
        Some(last) if last.width() == 0 => {}
        Some(_) => out.push(Line::from("").into()),
    }
}

impl MdRenderer {
    fn new(width: usize, density: Density) -> Self {
        MdRenderer {
            width,
            out: Vec::new(),
            inline: Vec::new(),
            inline_links: Vec::new(),
            current_link: None,
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
            marker_style: theme::muted(),
            band_start: None,
            band_depth: 0,
            density,
        }
    }

    fn airy(&self) -> bool {
        self.density == Density::Airy
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
                    self.out.push(
                        Line::from(Span::styled("─".repeat(self.width), theme::muted())).into(),
                    );
                    self.blank_after_top_level_block();
                }
                Event::TaskListMarker(checked) => {
                    let mark = if checked { "[✓]" } else { "[ ]" };
                    self.push_inline(
                        Span::styled(mark.to_string(), theme::text().add_modifier(Modifier::BOLD)),
                        None,
                    );
                    self.push_inline(Span::raw(" "), None);
                }
                Event::FootnoteReference(name) => {
                    self.push_inline(
                        Span::styled(format!("[^{name}]"), theme::text_secondary()),
                        None,
                    );
                }
                Event::InlineMath(_) | Event::DisplayMath(_) => {}
            }
        }
    }

    /// One blank line after a top-level block, so distinct blocks never touch.
    ///
    /// Trailing rather than leading on purpose: the streaming split renderer
    /// renders a settled prefix and a tail separately and concatenates them, so
    /// a separator must live in the block that ends, not the one that follows —
    /// otherwise the tail renderer (starting empty) cannot see it. Nested blocks
    /// (inside a list or quote) own their own spacing and are left alone.
    fn blank_after_top_level_block(&mut self) {
        if self.list_stack.is_empty() && self.quote_depth == 0 {
            ensure_blank_separator(&mut self.out);
        }
    }

    fn on_start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {}
            Tag::Heading { level, .. } => {
                // H1/H2 use structure weight and a quiet rule; preserve the
                // author's case. One resting row separates sections without
                // stacking padding from adjacent blocks.
                if self.airy() {
                    ensure_blank_separator(&mut self.out);
                }
                let level = heading_rank(level);
                self.heading_level = Some(level);
                self.push_style(if level <= 2 {
                    theme::response_heading()
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
                // Airy density gives a fenced block a full blank row of air
                // on each side so it reads as its own object.
                if self.airy() {
                    ensure_blank_separator(&mut self.out);
                }
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
                self.band_depth += 1;
                // Only an outermost, non-quoted list opens a band: bands are
                // answer-level furniture, and a list inside a quote already
                // carries the quote rail.
                if self.band_depth == 1 && self.quote_depth == 0 {
                    self.band_start = Some(self.out.len());
                }
            }
            Tag::Item => {
                self.flush_para();
                // The marker (and its continuation column) is answer
                // structure, so it takes the structure hue for the lifetime
                // of the item; prose inside stays neutral.
                self.marker_style = theme::response_marker();
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
            // Editorial emphasis (FORGE-DESIGN §6, §9.4): `**strong**` takes
            // the orange prose hue, `*emphasis*` the greenish-yellow one. The
            // modifier stays so weight/style — not colour alone — carries the
            // emphasis on monochrome terminals.
            Tag::Emphasis => self.push_style(theme::md_emph().add_modifier(Modifier::ITALIC)),
            Tag::Strong => self.push_style(theme::md_strong().add_modifier(Modifier::BOLD)),
            Tag::Strikethrough => {
                self.push_style(Style::default().add_modifier(Modifier::CROSSED_OUT))
            }
            // A link is styled as a link only when its destination may actually
            // be emitted: the underline is the promise that something can be
            // clicked or copied, and a scheme the policy refuses (`file:`,
            // `mailto:`, an injected control byte) must not make that promise.
            // The accent stays out of it — it is the focus color ("where am
            // I"), so painting prose links with it makes every URL look
            // interactive in the pane that owns the caret. Primary text +
            // underline keeps the affordance without borrowing focus meaning.
            Tag::Link { dest_url, .. } => {
                self.current_link = destination_for(&dest_url);
                let linkable = self.current_link.is_some();
                self.push_style(if linkable {
                    link_label_style()
                } else {
                    Style::default().fg(theme::text_primary_color())
                });
            }
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
                self.inline_links.clear();
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
                    self.out.push(Line::from("").into());
                }
            }
            TagEnd::Heading(_) => {
                self.pop_style();
                self.flush_para();
                self.prefix.clear();
                self.cont_prefix.clear();
                if self.heading_level.take().is_some_and(|rank| rank <= 2) {
                    self.out.push(
                        Line::from(Span::styled("─".repeat(self.width), theme::border_muted()))
                            .into(),
                    );
                }
                if self.airy() {
                    ensure_blank_separator(&mut self.out);
                } else {
                    self.blank_after_top_level_block();
                }
            }
            TagEnd::Item => {
                self.flush_para();
                self.prefix.clear();
                self.cont_prefix.clear();
                self.marker_style = theme::muted();
            }
            TagEnd::List(_) => {
                if let Some(frame) = self.list_stack.pop() {
                    self.cont_prefix = frame.saved_cont;
                }
                if self.band_depth > 0 {
                    self.band_depth -= 1;
                }
                if self.band_depth == 0 {
                    // Reversal of insert order matters: band_start taken
                    // before slicing so paint can't see its own None state.
                    if let Some(start) = self.band_start.take() {
                        if self.quote_depth == 0 {
                            paint_scan_band(&mut self.out[start..], self.width);
                        }
                    }
                }
                self.blank_after_top_level_block();
            }
            TagEnd::BlockQuote(_) | TagEnd::FootnoteDefinition => {
                self.flush_para();
                self.quote_depth = self.quote_depth.saturating_sub(1);
                self.blank_after_top_level_block();
            }
            TagEnd::CodeBlock => {
                if let Some(code) = self.code.take() {
                    self.render_code(code);
                }
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => self.pop_style(),
            TagEnd::Link => {
                self.current_link = None;
                self.pop_style();
            }
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
                    self.out.extend(
                        render_table(&table, self.width, &prefix, &prefix)
                            .into_iter()
                            .map(HyperlinkLine::from),
                    );
                }
                self.blank_after_top_level_block();
            }
            TagEnd::TableRow => {}
            TagEnd::TableCell => {
                if let Some(table) = &mut self.table {
                    // A table's columns are laid out by `render_table`, which
                    // re-wraps and re-aligns every cell against its own
                    // geometry — so a cell's link would need its columns
                    // recomputed there. Until that exists, cells keep the
                    // underline they have always had and the picker remains
                    // the way to a destination written inside one.
                    self.inline_links.clear();
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
                        &[],
                        self.width,
                        "",
                        "",
                        theme::muted(),
                    ) {
                        self.out.push(line);
                    }
                }
            }
            _ => {}
        }
    }

    fn finish(self) -> Vec<HyperlinkLine> {
        trim_trailing_blanks(self.finish_open())
    }

    /// Everything `finish` does except the trailing-blank trim.
    ///
    /// A top-level paragraph pushes a blank line *after* itself, and `finish`
    /// strips those from the very end. That blank is what separates it from
    /// whatever comes next, so a prefix rendered with `finish` has already lost
    /// its separator and cannot be concatenated with a continuation. Keeping
    /// the open form is what makes [`render_markdown_split`] exact.
    fn finish_open(mut self) -> Vec<HyperlinkLine> {
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

    /// Add prose to the open paragraph, giving every bare URL in it a
    /// destination.
    ///
    /// Three cases stay inert, each for its own reason. Inside an explicit
    /// link the author has already named the destination, so a URL in the
    /// label is just text. Headings keep the plain-label treatment for bare
    /// URLs. A table cell drops its link table on the way into the row, so an
    /// underline there would promise a click that cannot happen.
    fn push_span(&mut self, text: String) {
        if let Some(link) = self.current_link.clone() {
            self.push_inline(Span::styled(text, self.current_style()), Some(link));
            return;
        }
        let matches = if self.heading_level.is_some() || self.table.is_some() {
            Vec::new()
        } else {
            autolink_matches(&text)
        };
        if matches.is_empty() {
            self.push_inline(Span::styled(text, self.current_style()), None);
            return;
        }
        let mut cursor = 0usize;
        for (range, destination) in matches {
            if range.start > cursor {
                let plain = text[cursor..range.start].to_string();
                self.push_inline(Span::styled(plain, self.current_style()), None);
            }
            let label = text[range.clone()].to_string();
            let style = self.current_style().patch(link_label_style());
            self.push_inline(Span::styled(label, style), Some(destination));
            cursor = range.end;
        }
        if cursor < text.len() {
            let rest = text[cursor..].to_string();
            self.push_inline(Span::styled(rest, self.current_style()), None);
        }
    }

    /// Add one span to the open paragraph, carrying the link it belongs to (if
    /// any). Every push goes through here so `inline` and `inline_links` can
    /// never drift apart.
    fn push_inline(&mut self, span: Span<'static>, link: Option<String>) {
        self.inline.push(span);
        self.inline_links.push(link);
    }

    fn flush_para(&mut self) {
        if self.inline.is_empty() {
            return;
        }
        let quote = "│ ".repeat(self.quote_depth);
        let prefix = format!("{quote}{}", self.prefix);
        let cont = format!("{quote}{}", self.cont_prefix);
        for line in wrap_spans(
            &self.inline,
            &self.inline_links,
            self.width,
            &prefix,
            &cont,
            self.marker_style,
        ) {
            self.out.push(line);
        }
        self.inline.clear();
        self.inline_links.clear();
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
        // No tinted ground. The rail and the syntax colours already say this
        // is code, and a filled slab running the width of the pane is a lot of
        // paint for that — more so since prose stopped capping at 72 columns.
        // Rows are no longer padded either: the padding existed only to carry
        // the tint to the right edge, and without it the trailing spaces are
        // just trailing spaces.
        let mut spans = vec![Span::styled(
            format!("{CODE_INDENT}{CODE_GUTTER}"),
            theme::code_gutter(),
        )];
        spans.extend(content);
        Line::from(spans)
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
            self.out.push(self.language_chip(&code.language).into());
        }
        let body = code.body.trim_end_matches('\n');
        if code.fenced && matches!(code.language.as_str(), "mermaid" | "mmd") {
            if let Some(lines) = crate::mermaid::render(body, self.width) {
                self.out.extend(lines.into_iter().map(HyperlinkLine::from));
                self.out.push(Line::from("").into());
                return;
            }
        }
        if body.is_empty() {
            return;
        }
        let theme = theme::syntax_theme();
        for line_segments in highlight_to_lines(&code.language, body, &theme).iter() {
            let row = self.code_row(render_highlighted_line(line_segments));
            self.out.push(row.into());
        }
        // A block opens with air above it and used to close with none, so the
        // prose that follows started on the row under the last line of code —
        // touching a tinted slab it has nothing to do with. Unlike a heading,
        // which belongs to what comes after it, a fenced block belongs to
        // itself.
        self.out.push(Line::from("").into());
    }
}

/// Left inset of a fenced code block. Empty so the rail sits on the same left
/// edge as prose, list markers, tables and quote rails — a code block is a
/// block like any other, and an extra inset made it the one content type that
/// started two columns to the right of everything else.
const CODE_INDENT: &str = "";
/// Rule drawn down the left edge of every row of a fenced block.
const CODE_GUTTER: &str = "▌ ";

/// Paint the scan-band ground across a whole list block's lines.
///
/// Runs once, when the outermost list closes. Every span keeps its own
/// foreground and modifiers; only a span with *no* background picks up the
/// band, so inline-code chips and nested tables keep their own grounds.
/// Rows are padded out to the full prose width on the right so the band
/// reads as one slab, not a halo behind glyphs; the prefix column already
/// starts at the left edge, so its own cells carry the band once filled.
fn paint_scan_band(lines: &mut [HyperlinkLine], width: usize) {
    let band = theme::scan_band_bg();
    let fill = |span: &mut Span<'static>| {
        if span.style.bg.is_none() {
            span.style = span.style.patch(band);
        }
    };
    for line in lines.iter_mut() {
        if line.spans.is_empty() {
            line.spans.push(Span::styled(" ".repeat(width), band));
            continue;
        }
        let used: usize = line.spans.iter().map(|s| s.width()).sum();
        if used < width {
            line.spans
                .push(Span::styled(" ".repeat(width - used), band));
        }
        for span in &mut line.spans {
            fill(span);
        }
    }
}

pub(crate) fn display_width(s: &str) -> usize {
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
/// `marker_style` paints the prefix/continuation gutter itself — list markers
/// take the structure hue, everything else stays muted.
fn wrap_spans(
    spans: &[Span<'static>],
    links: &[Option<String>],
    width: usize,
    prefix: &str,
    cont: &str,
    marker_style: Style,
) -> Vec<HyperlinkLine> {
    if spans.is_empty() {
        return Vec::new();
    }
    let prefix_span = Span::styled(prefix.to_string(), marker_style);
    let cont_span = Span::styled(cont.to_string(), marker_style);
    let prefix_w = display_width(prefix);
    let cont_w = display_width(cont);
    let mut out: Vec<HyperlinkLine> = Vec::new();
    let mut cur: Vec<Span<'static>> = vec![prefix_span];
    // Link columns for the line under construction. A destination recorded
    // here is a column range on *this* row; when a wrap splits a link across
    // rows the range is closed on the row it started on and a fresh one opens
    // on the row after, which is exactly what the terminal needs.
    let mut cur_links: Vec<TerminalHyperlink> = Vec::new();
    let mut cur_w = prefix_w;
    let mut has_content = false;

    for Token {
        word,
        style,
        glued,
        span_index,
    } in tokenize(spans)
    {
        let destination = links.get(span_index).and_then(|link| link.as_deref());
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
                extend_link(&mut cur_links, destination, cur_w, cur_w + wlen);
                cur_w += wlen;
                has_content = true;
                break;
            }
            if has_content {
                out.push(HyperlinkLine::with_links(
                    Line::from(std::mem::take(&mut cur)),
                    std::mem::take(&mut cur_links),
                ));
                cur.push(cont_span.clone());
                cur_w = cont_w;
                has_content = false;
                continue;
            }
            if room == 0 {
                out.push(HyperlinkLine::with_links(
                    Line::from(std::mem::take(&mut cur)),
                    std::mem::take(&mut cur_links),
                ));
                cur.push(cont_span.clone());
                cur_w = cont_w;
                continue;
            }
            let (chunk, rest) = split_at_width(remaining, room);
            cur.push(Span::styled(chunk.to_string(), style));
            extend_link(
                &mut cur_links,
                destination,
                cur_w,
                cur_w + display_width(chunk),
            );
            out.push(HyperlinkLine::with_links(
                Line::from(std::mem::take(&mut cur)),
                std::mem::take(&mut cur_links),
            ));
            cur.push(cont_span.clone());
            cur_w = cont_w;
            has_content = false;
            remaining = rest;
        }
    }
    out.push(HyperlinkLine::with_links(
        Line::from(cur),
        std::mem::take(&mut cur_links),
    ));
    out
}

/// Record `[start, end)` as part of `destination`'s run on the current row,
/// merging with the run it continues.
///
/// Wrapping splits a link into several spans — and a space between two words of
/// the same link is its own span — so a row's links arrive in pieces. Pieces are
/// merged across the single column the wrapper's separator occupies, so a
/// two-word label is one range rather than two, while two different
/// destinations are never joined.
fn extend_link(
    links: &mut Vec<TerminalHyperlink>,
    destination: Option<&str>,
    start: usize,
    end: usize,
) {
    let Some(destination) = destination else {
        return;
    };
    if end <= start {
        return;
    }
    if let Some(last) = links.last_mut() {
        // `+ 1` tolerates the separator space the wrapper inserted between two
        // words: it is the link's own text that is underlined, and the range
        // spanning it keeps one clickable run per row.
        if last.destination == destination && start <= last.columns.end + 1 {
            last.columns.end = last.columns.end.max(end);
            return;
        }
    }
    links.push(TerminalHyperlink::new(start..end, destination));
}

/// A word to place, its style, and whether it was written flush against the
/// word before it.
struct Token {
    word: String,
    style: Style,
    /// No whitespace separated this token from its predecessor in the source.
    glued: bool,
    /// Which input span the word came from, so its link — carried beside the
    /// spans in `inline_links` — can be found without copying a destination
    /// into every word.
    span_index: usize,
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
    for (span_index, span) in spans.iter().enumerate() {
        let content = span.content.as_ref();
        let starts_ws = content.starts_with(char::is_whitespace);
        let mut words = content.split_whitespace();
        if let Some(first) = words.next() {
            out.push(Token {
                word: first.to_string(),
                style: span.style,
                glued: !starts_ws && !prev_ended_ws,
                span_index,
            });
            for word in words {
                out.push(Token {
                    word: word.to_string(),
                    style: span.style,
                    glued: false,
                    span_index,
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
    let mut body_index = 0usize;
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
        // Zebra parity counts body rows only, so the header never shifts the
        // stripes and the first body row stays untinted.
        let zebra = !header && body_index % 2 == 1;
        if !header {
            body_index += 1;
        }
        out.extend(render_boxed_row(
            &mut take_prefix,
            cells,
            &widths,
            &alignments,
            zebra,
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
    zebra: bool,
) -> Vec<Line<'static>> {
    let wrapped: Vec<Vec<Line<'static>>> = cells
        .iter()
        .zip(widths)
        .map(|(cell, w)| {
            if *w == 0 {
                return vec![Line::from("")];
            }
            // Table cells keep the renderer's plain geometry: their links are
            // recovered by the picker from the source, not marked here.
            let lines: Vec<Line<'static>> = wrap_spans(cell, &[], *w, "", "", theme::muted())
                .into_iter()
                .map(|line| line.line)
                .collect();
            if lines.is_empty() {
                vec![Line::from("")]
            } else {
                lines
            }
        })
        .collect();
    let height = wrapped.iter().map(|lines| lines.len()).max().unwrap_or(1);
    let border = theme::border_muted();
    let stripe = zebra.then(theme::zebra_row_bg);
    let tint = |span: &mut Span<'static>| {
        if let Some(stripe) = stripe {
            if span.style.bg.is_none() {
                span.style = span.style.patch(stripe);
            }
        }
    };
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
        if stripe.is_some() {
            // A stripe carries across the full row — gutter, walls and cell
            // padding included — so the band reads as one bar, not patches.
            // Spans that already own a background (inline-code chips) keep
            // it.
            for span in &mut spans {
                tint(span);
            }
        }
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
    segments
        .iter()
        .map(|(text, rgb, bold, italic)| {
            // Foreground only. Each token used to carry the block's ground on
            // its own span, so the tint ended wherever the token did and the
            // block had a ragged edge rather than a shape.
            let mut style = theme::syntax_segment(*rgb, None);
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

    /// One row's text, for slicing against the columns a link claims. The
    /// destination is never in the text, so the slice is what the reader sees.
    fn row_text(line: &HyperlinkLine) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    /// The underline is a promise that the destination can be acted on. A
    /// scheme the policy refuses must not make it.
    #[test]
    fn a_rejected_scheme_stays_plain_text() {
        let rendered = render_markdown_links("[write me](mailto:someone@example.com)", 80);
        assert!(rendered.iter().all(|line| line.links.is_empty()));
        // The label still renders — the reader sees the words, they simply
        // carry no affordance — and nothing in the row is underlined.
        assert_eq!(
            rendered.iter().map(row_text).collect::<String>(),
            "write me"
        );
        let underlined = rendered
            .iter()
            .flat_map(|line| line.spans.iter())
            .any(|span| span.style.add_modifier.contains(Modifier::UNDERLINED));
        assert!(!underlined, "a refused destination must not look clickable");
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

    #[test]
    fn a_block_quote_run_is_held_back() {
        let buffer = "Quoting:\n\n> one\n\n> two\n";
        assert_eq!(settled(buffer), "Quoting:\n\n");
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
}
