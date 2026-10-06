//! Drawing a transcript: turning `forge_transcript`'s projection into
//! ratatui lines.
//!
//! Everything here produces `Line<'static>`; nothing here decides *what*
//! the transcript shows. That half is `forge-transcript`, which this module
//! re-exports so `crate::conversation::` keeps naming both.

pub use forge_transcript::*;

use crate::links::{self, HyperlinkLine};
use crate::markdown::{
    render_markdown, render_markdown_links, render_markdown_open_with,
    render_markdown_with_density_links, Density, STREAM_CARET,
};
use crate::status_glyph::{lifecycle_marker, Lifecycle};
use crate::theme;
use crate::user_message_gutter;
use forge_syntax::highlight_to_lines;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Padding, Widget};

/// Compact tool/progress rows: left-rail glyph only, no extra blank gap.
/// Placement is event order from `forge-transcript`; this is paint, not gather.
pub(super) fn is_railed_block(block: &ConversationBlock) -> bool {
    matches!(
        block,
        ConversationBlock::ActivityGroup(_) | ConversationBlock::ActiveProgress(_)
    )
}

/// Shared "· N chars · R tok/s · N tools" suffix, used by both the live
/// `TurnSummary` line.
fn turn_stats_detail(chars: usize, tokens_per_second: Option<f64>, tools: usize) -> String {
    let mut detail = format!("·  {} chars", compact_count(chars));
    if let Some(rate) = tokens_per_second {
        detail.push_str(&format!("  ·  {rate:.0} tok/s"));
    }
    if tools > 0 {
        let unit = if tools == 1 { "tool" } else { "tools" };
        detail.push_str(&format!("  ·  {tools} {unit}"));
    }
    detail
}

/// Round a count for display: `842`, `1.2k`, `48k`.
pub(super) fn compact_count(n: usize) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=9_999 => format!("{:.1}k", n as f64 / 1_000.0),
        _ => format!("{}k", n / 1_000),
    }
}

/// Add a blank separator line unless the last line is already blank.
pub(super) fn ensure_blank_line(lines: &mut TranscriptRows) {
    let last_blank = lines
        .last()
        .is_none_or(|l| l.spans.iter().all(|s| s.content.is_empty()));
    if !last_blank {
        lines.push(Line::from(""));
    }
}

/// The rows of one rendered transcript, under construction.
///
/// Most rows are plain text. A few — the ones rendered from markdown — also
/// carry the destinations their columns hide, and those destinations have to
/// survive assembly: the transcript is scrolled, windowed, joined and padded
/// before it is painted, and a destination lost at any of those steps is a link
/// the reader can see but not use. `push` and `extend` accept both, so a row's
/// links travel with the row and no call site has to say which kind it is
/// building.
#[derive(Default)]
pub(super) struct TranscriptRows(Vec<HyperlinkLine>);

impl TranscriptRows {
    fn push(&mut self, line: impl Into<HyperlinkLine>) {
        self.0.push(line.into());
    }

    fn extend<I>(&mut self, lines: I)
    where
        I: IntoIterator,
        I::Item: Into<HyperlinkLine>,
    {
        self.0.extend(lines.into_iter().map(Into::into));
    }

    fn into_inner(self) -> Vec<HyperlinkLine> {
        self.0
    }
}

impl std::ops::Deref for TranscriptRows {
    type Target = Vec<HyperlinkLine>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// A bordered card's top edge: `┌─ {title} ───┐` when `title` is set
/// (Approval/Question), or a plain `┌────┐` when it isn't. `total_width` is
/// the full rendered line width (the card's content width plus its 2 side
/// borders and 2 padding columns).
pub(super) fn card_top_border(
    total_width: usize,
    title: Option<&str>,
    border: Style,
) -> Line<'static> {
    let _ = (title, border);
    Line::from(" ".repeat(total_width))
}

/// A bordered card's bottom edge: `└────┘`.
pub(super) fn card_bottom_border(total_width: usize, border: Style) -> Line<'static> {
    let _ = border;
    Line::from(" ".repeat(total_width))
}

/// A bordered card's content row: `│ {content, padded to interior_width} │`.
/// `fill`, when set, paints the row's background edge-to-edge (Approval
/// wants `panel_alt`; Plan wants none — canvas shows through).
#[allow(dead_code)]
pub(super) fn card_content_line(
    content: &str,
    interior_width: usize,
    style: Style,
    border: Style,
    fill: Option<Color>,
) -> Line<'static> {
    let pad = " ".repeat(interior_width.saturating_sub(content.chars().count()));
    let border_style = match fill {
        Some(bg) => border.bg(bg),
        None => border,
    };
    let content_style = match fill {
        Some(bg) => style.bg(bg),
        None => style,
    };
    let _ = border_style;
    Line::from(vec![Span::styled(format!("{content}{pad}"), content_style)])
}

/// Like [`card_content_line`], but for a row built from several differently
/// styled spans (e.g. a colored status marker followed by plain body text)
/// instead of one uniformly styled string.
pub(super) fn card_content_spans(
    mut spans: Vec<Span<'static>>,
    interior_width: usize,
    border: Style,
    fill: Option<Color>,
) -> Line<'static> {
    let used: usize = spans.iter().map(Span::width).sum();
    let pad = " ".repeat(interior_width.saturating_sub(used));
    if let Some(bg) = fill {
        for span in &mut spans {
            span.style = span.style.bg(bg);
        }
    }
    let border_style = match fill {
        Some(bg) => border.bg(bg),
        None => border,
    };
    let mut line_spans = Vec::new();
    line_spans.append(&mut spans);
    line_spans.push(Span::styled(
        pad,
        fill.map_or(Style::default(), |bg| Style::default().bg(bg)),
    ));
    let _ = border_style;
    Line::from(line_spans)
}

/// Prepend a rail glyph in the given style to a rendered line.
pub(super) fn prefix_line_with(line: &mut Line<'static>, glyph_style: Style) {
    let mut spans = vec![Span::styled("  ", glyph_style)];
    spans.extend(std::mem::take(&mut line.spans));
    line.spans = spans;
}

/// Prepend the left-rail glyph to a rendered line.
pub(super) fn prefix_line_rail(line: &mut Line<'static>) {
    prefix_line_with(line, theme::border_muted());
}

pub(super) fn diff_title_line(path: &str, diff: &[String]) -> Line<'static> {
    let numbered = number_diff_lines(diff);
    let additions = numbered.iter().filter(|line| line.marker == '+').count();
    let removals = numbered.iter().filter(|line| line.marker == '-').count();
    Line::from(vec![
        Span::raw(DIFF_BLOCK_MARKER),
        Span::raw(" "),
        Span::styled(path.to_string(), theme::text().add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled(format!("+{additions}"), theme::ok()),
        Span::raw(" "),
        Span::styled(format!("-{removals}"), theme::danger()),
        Span::raw(" "),
    ])
}

pub(super) fn render_numbered_diff(
    path: &str,
    diff: &[String],
    width: usize,
) -> Vec<Line<'static>> {
    let numbered = number_diff_lines(diff);
    let number_width = numbered
        .iter()
        .flat_map(|line| [line.old, line.new])
        .flatten()
        .max()
        .map(|line| line.to_string().len())
        .unwrap_or(1);
    let code = numbered
        .iter()
        .filter(|line| !line.header)
        .map(|line| line.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let highlighted =
        lang_from_path(path).map(|lang| highlight_to_lines(lang, &code, &theme::syntax_theme()));
    let mut code_index = 0;
    let mut rendered = Vec::with_capacity(numbered.len());

    for line in numbered {
        if line.header {
            let text = line.content;
            let padding = " ".repeat(width.saturating_sub(text.chars().count()));
            rendered.push(Line::from(Span::styled(
                format!("{text}{padding}"),
                theme::diff_hunk(),
            )));
            continue;
        }

        let old = line.old.map(|line| line.to_string()).unwrap_or_default();
        let new = line.new.map(|line| line.to_string()).unwrap_or_default();
        let line_style = match line.marker {
            '+' => theme::diff_add(),
            '-' => theme::diff_remove(),
            _ => theme::diff_context(),
        };
        let gutter = format!(
            "  {old:>number_width$} {new:>number_width$} │ {} ",
            line.marker
        );
        let row_width = gutter.chars().count() + line.content.chars().count();
        let mut spans = vec![Span::styled(gutter, line_style)];

        if let Some(Some(parts)) = highlighted.as_ref().map(|lines| lines.get(code_index)) {
            for (text, rgb, bold, italic) in parts {
                let mut style = theme::syntax_segment(
                    *rgb,
                    Some(line_style.bg.unwrap_or(theme::panel_alt_bg())),
                );
                if *bold {
                    style = style.add_modifier(Modifier::BOLD);
                }
                if *italic {
                    style = style.add_modifier(Modifier::ITALIC);
                }
                spans.push(Span::styled(text.clone(), style));
            }
        } else {
            spans.push(Span::styled(line.content, line_style));
        }
        spans.push(Span::styled(
            " ".repeat(width.saturating_sub(row_width)),
            line_style,
        ));
        rendered.push(Line::from(spans));
        code_index += 1;
    }

    rendered
}

/// Dim half of the shared running pulse: true when the active plan `[>]`
/// marker should render dim rather than bright. Mirrors the footer's running
/// dot rhythm so both breathe together on the event-loop tick.
pub(super) fn plan_pulse_dim(state: &throbber_widgets_tui::ThrobberState) -> bool {
    !crate::widgets::footer::running_dot_bright(state)
}

pub(super) fn render_plan_checklist(
    plan: &PlanChecklistPresentation,
    width: usize,
) -> Vec<Line<'static>> {
    render_plan_checklist_with_pulse(plan, width, false)
}

/// Plan checklist with an optional pulse on the active step.
///
/// The glyph never changes — only the active marker's brightness breathes
/// (bold ↔ dim in the activity hue, mirroring the footer's running dot) —
/// so every frame keeps the same width and reads calm next to the live turn
/// line. `pulse_dim` is true on the dim half of the event-loop tick cycle;
/// pass false (bright) for settled history and tests.
pub(super) fn render_plan_checklist_with_pulse(
    plan: &PlanChecklistPresentation,
    width: usize,
    pulse_dim: bool,
) -> Vec<Line<'static>> {
    use forge_types::PlanStepStatus;
    let mut lines = Vec::new();
    let explanation = plan
        .explanation
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(explanation) = explanation {
        for l in wrap(explanation, width.saturating_sub(2)) {
            lines.push(Line::from(vec![
                Span::raw(INDENT_UNIT),
                Span::styled(l, theme::muted()),
            ]));
        }
        // The explanation is a lead-in to the checklist, not part of it; one
        // blank keeps the `Plan · N of M done` header from reading as a
        // continuation of the explanation sentence.
        lines.push(Line::from(""));
    }

    // Checkboxes keep every state legible without colour. Reserve the
    // interaction accent for focus; active work uses the information token.
    let rail_line = |content: Vec<Span<'static>>| -> Line<'static> {
        let mut spans = vec![Span::raw("  ")];
        spans.extend(content);
        Line::from(spans)
    };

    let total = plan.steps.len();
    let done = plan
        .steps
        .iter()
        .filter(|item| item.status == PlanStepStatus::Completed)
        .count();
    let header = if total == 0 {
        "Plan".to_string()
    } else {
        format!("Plan · {done} of {total} done")
    };
    lines.push(rail_line(vec![Span::styled(
        header,
        theme::metadata_style(),
    )]));

    let body_width = width.saturating_sub(6).max(1);
    for (idx, item) in plan.steps.iter().enumerate() {
        // 2026 lifecycle grammar via shared helpers (DESIGN-002).
        let lifecycle = match item.status {
            PlanStepStatus::Completed => Lifecycle::Complete,
            PlanStepStatus::InProgress => Lifecycle::Active,
            PlanStepStatus::Pending => Lifecycle::Pending,
        };
        let marker_span = lifecycle_marker(lifecycle);
        let (marker, marker_style, text_style) = match item.status {
            PlanStepStatus::Completed => (
                "[✓]",
                theme::muted(),
                theme::muted()
                    .add_modifier(Modifier::ITALIC)
                    .add_modifier(Modifier::CROSSED_OUT),
            ),
            PlanStepStatus::InProgress => {
                let base = theme::activity();
                let marker_style = if pulse_dim {
                    base.remove_modifier(Modifier::BOLD)
                        .add_modifier(Modifier::DIM)
                } else {
                    base.add_modifier(Modifier::BOLD)
                };
                (
                    "[>]",
                    marker_style,
                    theme::text().add_modifier(Modifier::BOLD),
                )
            }
            PlanStepStatus::Pending => ("[ ]", theme::muted(), theme::muted()),
        };
        debug_assert!(
            marker == marker_span.content.as_ref() || item.status == PlanStepStatus::Completed
        );
        let mut wrapped = wrap(&item.step, body_width).into_iter();
        if let Some(first) = wrapped.next() {
            lines.push(rail_line(vec![
                Span::styled(format!("{marker} "), marker_style),
                Span::styled(first, text_style),
            ]));
        }
        for cont in wrapped {
            lines.push(rail_line(vec![
                Span::raw("    "),
                Span::styled(cont, text_style),
            ]));
        }
        // What actually ran under this step. A plan states intent; without
        // this, a step marked done is only the model's word for it.
        if let Some(evidence) = plan.evidence.get(idx).filter(|e| !e.is_empty()) {
            let summary = evidence.join(", ");
            let shown = if evidence.len() > PLAN_EVIDENCE_ITEMS {
                format!(
                    "{}, +{} more",
                    evidence[..PLAN_EVIDENCE_ITEMS].join(", "),
                    evidence.len() - PLAN_EVIDENCE_ITEMS
                )
            } else {
                summary
            };
            for wrapped in wrap(&shown, body_width.saturating_sub(2))
                .into_iter()
                .take(2)
            {
                lines.push(rail_line(vec![
                    Span::raw("    "),
                    Span::styled(wrapped, theme::metadata_style()),
                ]));
            }
        }
    }
    lines
}

fn estimate_wrapped_lines(text: &str, width: usize) -> usize {
    let width = width.max(1);
    text.lines()
        .map(|line| line.chars().count().div_ceil(width).max(1))
        .sum::<usize>()
        .max(1)
}

fn estimate_block_lines(block: &ConversationBlock, width: usize, prose_width: usize) -> usize {
    let body = match block {
        ConversationBlock::UserMessage(p) => {
            estimate_wrapped_lines(&p.text, width.saturating_sub(MESSAGE_PADDING)).saturating_add(1)
        }
        ConversationBlock::AssistantAnswer(p) => estimate_wrapped_lines(&p.text, prose_width)
            .saturating_add(usize::from(!p.streaming && !p.text.trim().is_empty())),
        ConversationBlock::Thinking(p) if p.collapsed => 1,
        ConversationBlock::Thinking(p) => estimate_wrapped_lines(&p.text, prose_width),
        ConversationBlock::CodeBlock(p) => estimate_wrapped_lines(&p.text, width),
        ConversationBlock::DiffBlock(p) => p.lines.len().saturating_add(2),
        ConversationBlock::VerificationBlock(p) => p.evidence.len().saturating_add(1),
        ConversationBlock::Callout(p) => estimate_wrapped_lines(&p.text, width).saturating_add(1),
        ConversationBlock::PlanChecklist(p) => p.steps.len().saturating_add(3),
        ConversationBlock::ActivityGroup(p) => 2usize.saturating_add(p.items.len().min(6)),
        ConversationBlock::ActiveProgress(_)
        | ConversationBlock::Metadata(_)
        | ConversationBlock::TurnSummary(_) => 1,
        // Measured, not guessed. The card's height depends on the width (how
        // far the question and each option's consequence wrap) and on how many
        // options there are, and under-budgeting it scrolls its own top border
        // — including the title — off the pane.
        ConversationBlock::ApprovalPending(p) => render_approval_card(p, prose_width).len(),
        ConversationBlock::Home(p) => render_home_card(p, prose_width, false).len(),
        ConversationBlock::QuestionPending(p) => render_question_card(p, prose_width).len(),
    };
    body.saturating_add(2)
}

fn start_block_for_tail(
    blocks: &[ConversationBlock],
    width: usize,
    prose_width: usize,
    keep_from_end: usize,
) -> usize {
    if keep_from_end == usize::MAX || blocks.is_empty() {
        return 0;
    }
    let mut acc = 0usize;
    let mut start = blocks.len();
    while start > 0 && acc < keep_from_end {
        start -= 1;
        acc = acc.saturating_add(estimate_block_lines(&blocks[start], width, prose_width));
    }
    start
}

/// Memoises the settled prefix of a streaming answer across frames.
///
/// The live preview used to re-parse and re-highlight the whole accumulated
/// answer on every rebuild, which is quadratic over a turn. This keeps the
/// settled prefix — everything `settled_prefix_len` says later bytes cannot
/// change — and re-parses only the tail.
///
/// The prefix is held in *open* form (see `render_markdown_open`) because a
/// paragraph's trailing blank is the separator from whatever follows, and a
/// finished render drops it.
#[derive(Default)]
pub struct StreamMarkdownCache {
    width: usize,
    /// Density the cached lines were rendered at. A density flip (short pane
    /// ↔ comfortable pane) changes line counts, so it invalidates like width.
    density: Density,
    /// The exact text the cached lines came from. Compared by content rather
    /// than length so a boundary that moves backwards rebuilds instead of
    /// silently reusing the wrong lines.
    prefix: String,
    open_lines: Vec<Line<'static>>,
    tail: String,
    tail_suffix: String,
    tail_lines: Vec<Line<'static>>,
}

/// Reasoning and answer are independent streams; neither may invalidate the other.
#[derive(Default)]
pub(crate) struct StreamPreviewCache {
    thinking: StreamMarkdownCache,
    answer: StreamMarkdownCache,
}

/// Map the app's compact flag onto the renderer's density.
pub(crate) fn transcript_density(compact: bool) -> Density {
    if compact {
        Density::Compact
    } else {
        Density::Airy
    }
}

impl StreamMarkdownCache {
    /// Lines for `text`, materialising at most `keep_from_end` of them.
    ///
    /// Caching the parse stops the answer being re-read, but copying every
    /// cached line into the output is O(total lines) on its own, so a long turn
    /// stays quadratic. Only the tail is ever on screen, so only the tail is
    /// built — the same windowing `lines_for_width_from_end` already applies to
    /// the transcript, moved inside a single block.
    fn render(
        &mut self,
        text: &str,
        width: usize,
        keep_from_end: usize,
        density: Density,
    ) -> Vec<Line<'static>> {
        self.render_inner(text, width, keep_from_end, "", density)
    }

    fn render_live(
        &mut self,
        text: &str,
        width: usize,
        keep_from_end: usize,
        density: Density,
    ) -> Vec<Line<'static>> {
        let suffix = if text.ends_with(STREAM_CARET) {
            String::new()
        } else {
            STREAM_CARET.to_string()
        };
        self.render_inner(text, width, keep_from_end, &suffix, density)
    }

    fn render_inner(
        &mut self,
        text: &str,
        width: usize,
        keep_from_end: usize,
        suffix: &str,
        density: Density,
    ) -> Vec<Line<'static>> {
        // Grow the cache rather than rebuild it. Re-rendering the whole settled
        // prefix on every boundary advance is O(n) per advance, which is the
        // quadratic this cache exists to remove. Appending is sound for the
        // same reason the split is: each advance lands on a top-level block
        // boundary, where the renderer's state is its initial state.
        let reset =
            self.width != width || self.density != density || !text.starts_with(&self.prefix);
        if reset {
            self.width = width;
            self.density = density;
            self.prefix.clear();
            self.open_lines.clear();
        }
        // `prefix` already ends at a proven top-level boundary. Scanning it
        // again on every delta made boundary detection quadratic even though
        // parsing and highlighting were cached. Only new bytes can advance the
        // boundary now.
        let cut =
            self.prefix.len() + crate::markdown::settled_prefix_len(&text[self.prefix.len()..]);
        if cut > self.prefix.len() {
            let fresh = &text[self.prefix.len()..cut];
            self.open_lines
                .extend(render_markdown_open_with(fresh, width, density));
            self.prefix.push_str(fresh);
        }
        let raw_tail = &text[cut..];
        // Reuse even the unsettled tail when only the other stream or the
        // viewport changed. A duration/caret belongs to the tail, not the prefix.
        if reset || self.tail != raw_tail || self.tail_suffix != suffix {
            self.tail.clear();
            self.tail.push_str(raw_tail);
            self.tail_suffix.clear();
            self.tail_suffix.push_str(suffix);
            let suffixed;
            let tail_text = if suffix.is_empty() {
                raw_tail
            } else {
                suffixed = format!("{raw_tail}{suffix}");
                &suffixed
            };
            // ponytail: an unfinished markdown block still reparses on each
            // change; a stateful parser is needed if giant open blocks dominate.
            self.tail_lines = render_markdown_open_with(tail_text, width, density);
            crate::markdown::fade_streaming_tail(&mut self.tail_lines);
        }
        let from_prefix = keep_from_end.saturating_sub(self.tail_lines.len());
        let skip = self.open_lines.len().saturating_sub(from_prefix);
        let tail_skip = self.tail_lines.len().saturating_sub(keep_from_end);
        crate::markdown::render_markdown_join(
            &self.open_lines[skip..],
            self.tail_lines[tail_skip..].to_vec(),
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn render_assistant_answer(
    text: &str,
    streaming: bool,
    width: usize,
    prose_width: usize,
    keep_from_end: usize,
    stream_cache: Option<&mut StreamMarkdownCache>,
    append_stream_caret: bool,
    density: Density,
) -> Vec<HyperlinkLine> {
    // Only the settled branch carries destinations. The streaming branches come
    // out of `StreamMarkdownCache`, which holds plain lines: a link written into
    // an answer that is still arriving becomes clickable when the block settles.
    // The destination is never lost to the reader either way — the picker reads
    // the transcript's source text, not its rendered columns.
    let parts: Vec<HyperlinkLine> = match stream_cache {
        Some(cache) if streaming && append_stream_caret => cache
            .render_live(text, prose_width, keep_from_end, density)
            .into_iter()
            .map(HyperlinkLine::from)
            .collect(),
        Some(cache) if streaming => cache
            .render(text, prose_width, keep_from_end, density)
            .into_iter()
            .map(HyperlinkLine::from)
            .collect(),
        _ => render_markdown_with_density_links(text, prose_width, density),
    };
    parts
        .into_iter()
        .map(|mut row| {
            let mut spans = vec![Span::raw(" ".repeat(MESSAGE_PADDING))];
            spans.extend(std::mem::take(&mut row.line.spans));
            let used = spans.iter().map(Span::width).sum::<usize>();
            if used < width {
                spans.push(Span::raw(" ".repeat(width - used)));
            }
            // The indent is part of the row, so every column it moves — the
            // link's included — moves with it.
            for link in &mut row.links {
                link.columns =
                    link.columns.start + MESSAGE_PADDING..link.columns.end + MESSAGE_PADDING;
            }
            row.line = Line::from(spans).style(theme::assistant_answer_style());
            row
        })
        .collect()
}

fn render_thinking(text: &str, duration_secs: Option<f64>, width: usize) -> Vec<Line<'static>> {
    let content_width = width.saturating_sub(INDENT_UNIT.chars().count() * 2);
    let full_text = match duration_secs {
        Some(secs) => format!("{text} · {}", format_elapsed_tenths(secs)),
        None => text.to_string(),
    };
    style_thinking_lines(render_markdown(&full_text, content_width))
}

fn style_thinking_lines(lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    let indent = INDENT_UNIT.repeat(2);
    lines
        .into_iter()
        .map(|line| {
            let mut spans = vec![Span::styled(
                indent.clone(),
                theme::dim().add_modifier(Modifier::ITALIC),
            )];
            spans.extend(line.spans.into_iter().map(|mut span| {
                span.style.fg = theme::dim().fg;
                span.style = span.style.add_modifier(Modifier::ITALIC);
                span
            }));
            Line::from(spans)
        })
        .collect()
}

/// Render live reasoning and answer text without cloning them through the
/// owned transcript presentation model on every token batch.
pub(crate) fn render_streaming_preview(
    thinking: &str,
    text: &str,
    thought_secs: Option<f64>,
    available_width: usize,
    keep_from_end: usize,
    cache: &mut StreamPreviewCache,
    density: Density,
) -> Vec<HyperlinkLine> {
    let width = available_width.max(4);
    let prose_width = prose_width_for(width);
    let mut lines = TranscriptRows::default();
    if !thinking.trim().is_empty() {
        let suffix = thought_secs
            .map(|secs| format!(" · {}", format_elapsed_tenths(secs)))
            .unwrap_or_default();
        let thinking_lines = cache.thinking.render_inner(
            thinking,
            width.saturating_sub(INDENT_UNIT.chars().count() * 2),
            keep_from_end,
            &suffix,
            Density::Compact,
        );
        lines.extend(style_thinking_lines(thinking_lines));
        lines.push(Line::from(""));
    }
    if !text.is_empty() {
        if text.contains("\\confidence{") {
            let mut raw = String::with_capacity(text.len() + STREAM_CARET.len_utf8());
            raw.push_str(text);
            if !raw.ends_with(STREAM_CARET) {
                raw.push(STREAM_CARET);
            }
            let body = sanitize_final_answer_text(&raw);
            lines.extend(render_assistant_answer(
                &body,
                true,
                width,
                prose_width,
                keep_from_end,
                Some(&mut cache.answer),
                false,
                density,
            ));
        } else {
            lines.extend(render_assistant_answer(
                text.trim_start(),
                true,
                width,
                prose_width,
                keep_from_end,
                Some(&mut cache.answer),
                true,
                density,
            ));
        }
        lines.push(Line::from(""));
    }
    lines.into_inner()
}

/// Render-cache details needed by the TUI frame renderer.
pub(crate) trait ConversationRenderInternals {
    fn lines_and_plan_dock_with_completeness(
        &self,
        available_width: usize,
        keep_from_end: usize,
    ) -> (Vec<HyperlinkLine>, Option<PlanDock>, bool);

    fn render_lines_with_completeness(
        &self,
        available_width: usize,
        keep_from_end: usize,
        stream_cache: Option<&mut StreamMarkdownCache>,
    ) -> (Vec<HyperlinkLine>, bool);
}

/// Drawing a [`ConversationModel`].
///
/// An extension trait rather than an inherent impl, because Rust requires
/// inherent impls to live with their type and the model now lives in
/// `forge-transcript`. Callers need this trait in scope.
pub trait ConversationRender {
    /// Render at the transcript's default width.
    fn lines(&self) -> Vec<HyperlinkLine>;
    /// Render wrapped to `available_width` columns.
    fn lines_for_width(&self, available_width: usize) -> Vec<HyperlinkLine>;
    /// Render only the last `keep_from_end` estimated lines, walking blocks
    /// from the tail. Follow-mode frames use this so a long transcript does
    /// not rebuild off-screen history.
    fn lines_for_width_from_end(
        &self,
        available_width: usize,
        keep_from_end: usize,
    ) -> Vec<HyperlinkLine>;
    /// As [`Self::lines_for_width_from_end`], reusing `cache` for the settled
    /// prefix of a streaming answer. Only the live preview passes one.
    fn lines_for_width_from_end_cached(
        &self,
        available_width: usize,
        keep_from_end: usize,
        cache: &mut StreamMarkdownCache,
    ) -> Vec<HyperlinkLine>;
    /// As [`Self::lines_for_width_from_end`], also reporting where the plan
    /// card sits so it can be docked once it scrolls away.
    fn lines_and_plan_dock(
        &self,
        available_width: usize,
        keep_from_end: usize,
    ) -> (Vec<HyperlinkLine>, Option<PlanDock>);
}

impl ConversationRender for ConversationModel {
    fn lines(&self) -> Vec<HyperlinkLine> {
        self.lines_for_width(if self.opts.compact { 88 } else { 100 })
    }

    /// Build display lines for the actual conversation viewport. Prose gets a
    /// readable cap; code and structured blocks keep the full pane width.
    fn lines_for_width(&self, available_width: usize) -> Vec<HyperlinkLine> {
        self.lines_for_width_from_end(available_width, usize::MAX)
    }

    fn lines_for_width_from_end(
        &self,
        available_width: usize,
        keep_from_end: usize,
    ) -> Vec<HyperlinkLine> {
        self.render_lines_with_completeness(available_width, keep_from_end, None)
            .0
    }

    fn lines_for_width_from_end_cached(
        &self,
        available_width: usize,
        keep_from_end: usize,
        cache: &mut StreamMarkdownCache,
    ) -> Vec<HyperlinkLine> {
        self.render_lines_with_completeness(available_width, keep_from_end, Some(cache))
            .0
    }

    fn lines_and_plan_dock(
        &self,
        available_width: usize,
        keep_from_end: usize,
    ) -> (Vec<HyperlinkLine>, Option<PlanDock>) {
        let (lines, dock, _) =
            self.lines_and_plan_dock_with_completeness(available_width, keep_from_end);
        (lines, dock)
    }
}

impl ConversationRenderInternals for ConversationModel {
    fn lines_and_plan_dock_with_completeness(
        &self,
        available_width: usize,
        keep_from_end: usize,
    ) -> (Vec<HyperlinkLine>, Option<PlanDock>, bool) {
        let (lines, complete) =
            self.render_lines_with_completeness(available_width, keep_from_end, None);
        let dock = plan_dock_for(self, available_width, &lines);
        (lines, dock, complete)
    }

    fn render_lines_with_completeness(
        &self,
        available_width: usize,
        keep_from_end: usize,
        mut stream_cache: Option<&mut StreamMarkdownCache>,
    ) -> (Vec<HyperlinkLine>, bool) {
        let width = available_width.max(4);
        let prose_width = prose_width_for(width);
        let mut lines = TranscriptRows::default();
        let gap = !self.opts.compact;
        let density = transcript_density(self.opts.compact);
        let rail = width >= RAIL_MIN_WIDTH;
        let blocks = self.semantic_blocks();
        // §12: empty reasoning carries no actionable information in the
        // default presentation — skip the placeholder row entirely. The
        // underlying item is untouched, so visibility controls still apply.
        let blocks: Vec<ConversationBlock> = blocks
            .into_iter()
            .filter(|b| match b {
                ConversationBlock::Thinking(p) => !p.text.trim().is_empty(),
                _ => true,
            })
            .collect();
        let start_block = start_block_for_tail(&blocks, width, prose_width, keep_from_end);
        // DESIGN-007: completed turns recede — a successful group in an
        // older turn renders neutral, never green. Failures, denials,
        // unknowns and the latest turn keep their full emphasis.
        let total_turns = blocks
            .iter()
            .filter(|b| matches!(b, ConversationBlock::UserMessage(_)))
            .count();
        let mut turn_index = blocks[..start_block.min(blocks.len())]
            .iter()
            .filter(|b| matches!(b, ConversationBlock::UserMessage(_)))
            .count();
        // A full-width rule opens every turn boundary (every UserMessage
        // after the first block in the transcript) — independent of whether
        // that turn has a plan checklist. The rule is cushioned by one blank
        // row on each side so turns visibly separate; distinct tool/activity
        // groups get a blank separator of their own in airy density, while
        // rows inside one group stay tight. Major blocks get a blank
        // separator.
        let mut seen_any_block = start_block > 0;
        let mut prev_railed = false;
        for block in blocks.into_iter().skip(start_block) {
            let is_turn_start = matches!(block, ConversationBlock::UserMessage(_));
            if is_turn_start {
                turn_index += 1;
            }
            let in_latest_turn = total_turns == 0 || turn_index == total_turns;
            let railed = is_railed_block(&block);
            if !is_turn_start && !railed && gap && !lines.is_empty() {
                // Major blocks read as boundaries: separate them from the
                // preceding tool trail with a single blank line. Turn starts
                // have their own separator treatment below.
                ensure_blank_line(&mut lines);
            }
            if railed && prev_railed && gap && !lines.is_empty() {
                // Two adjacent activity groups are distinct outlines; one
                // blank row keeps their headers from reading as one list.
                ensure_blank_line(&mut lines);
            }
            prev_railed = railed;
            if is_turn_start && seen_any_block {
                // A turn boundary is one rule with one row of breathing room
                // on each side. `ensure_blank_line` keeps a preceding block's
                // own trailing blank from doubling into a stack.
                if gap {
                    ensure_blank_line(&mut lines);
                }
                lines.push(Line::from(Span::styled(
                    "─".repeat(width),
                    theme::border_muted(),
                )));
                if gap {
                    lines.push(Line::from(""));
                }
            }
            seen_any_block = true;
            match block {
                ConversationBlock::UserMessage(p) => {
                    // §5 role landmark: renderer-owned neutral author label,
                    // legible without color (shape, not hue, carries authorship).
                    lines.push(Line::from(Span::styled(
                        format!("{}You", " ".repeat(MESSAGE_PADDING)),
                        theme::text().add_modifier(Modifier::BOLD),
                    )));
                    if gap {
                        lines.push(Line::from(""));
                    }
                    let theme_id = crate::theme::active();
                    let prefix_width = MESSAGE_PADDING;
                    let user_lines = user_message_gutter::render_user_message_lines(
                        &p.text,
                        message_content_width(width),
                        &theme_id,
                        false,
                        wrap,
                    );
                    // Restrained neutral ground (`selection`), not an accent
                    // tint: the prompt is a transcript region, and a saturated
                    // bar outranks the answer below it. Carried to the pane's
                    // right edge so the block still reads as one seamless bar.
                    let ground = theme::user_message();
                    for mut row in user_lines.into_iter() {
                        // No leading marker — just an indent matching
                        // assistant messages' own left padding.
                        let mut spans = vec![Span::styled(
                            " ".repeat(prefix_width),
                            theme::text().patch(ground),
                        )];
                        spans.extend(std::mem::take(&mut row.line.spans).into_iter().map(
                            |mut span| {
                                span.style = span.style.patch(ground);
                                span
                            },
                        ));
                        let content_width = spans.iter().map(Span::width).sum::<usize>();
                        if content_width < width {
                            spans.push(Span::styled(
                                " ".repeat(width - content_width),
                                theme::text().patch(ground),
                            ));
                        }
                        // The indent is part of the row, so every column it
                        // moves — the link's included — moves with it.
                        for link in &mut row.links {
                            link.columns =
                                link.columns.start + prefix_width..link.columns.end + prefix_width;
                        }
                        row.line = Line::from(spans);
                        lines.push(row);
                    }
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ConversationBlock::AssistantAnswer(p) => {
                    // §5: one renderer-owned Answer label per final-response
                    // region — never one per streamed fragment. Streaming
                    // previews stay unlabeled; the label lands once settled.
                    if !p.streaming && !p.text.trim().is_empty() {
                        lines.push(Line::from(Span::styled(
                            format!("{}Answer", " ".repeat(MESSAGE_PADDING)),
                            theme::text().add_modifier(Modifier::BOLD),
                        )));
                        if gap {
                            lines.push(Line::from(""));
                        }
                    }
                    lines.extend(render_assistant_answer(
                        &p.text,
                        p.streaming,
                        width,
                        prose_width,
                        keep_from_end,
                        stream_cache.as_deref_mut(),
                        false,
                        density,
                    ));
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ConversationBlock::ActiveProgress(p) => {
                    let label = format!("{} · {}", p.label, p.summary);
                    let mut line = Line::from(vec![
                        Span::styled(label, theme::progress_style().add_modifier(Modifier::BOLD)),
                        Span::styled("  ", theme::metadata_style()),
                        Span::styled(
                            match p.status {
                                ActiveProgressStatus::Started => "started",
                                ActiveProgressStatus::Updated => "updated",
                                ActiveProgressStatus::Completed => "completed",
                                ActiveProgressStatus::Failed => "failed",
                            },
                            theme::metadata_style(),
                        ),
                    ]);
                    if rail {
                        prefix_line_rail(&mut line);
                    }
                    lines.push(line);
                }
                ConversationBlock::ActivityGroup(p) => {
                    // DESIGN-007: routine success recedes once its turn is
                    // history. Every other outcome keeps its emphasis so
                    // failures, denials and uncertainty stay findable.
                    let label_style = match p.outcome {
                        ActivityOutcome::Success if !in_latest_turn => {
                            theme::text_secondary().add_modifier(Modifier::BOLD)
                        }
                        ActivityOutcome::Success => theme::tool_success_style(),
                        ActivityOutcome::Failure => theme::danger().add_modifier(Modifier::BOLD),
                        ActivityOutcome::Blocked => theme::warn().add_modifier(Modifier::BOLD),
                        ActivityOutcome::Warning => theme::warn().add_modifier(Modifier::BOLD),
                        ActivityOutcome::Neutral => theme::text().add_modifier(Modifier::BOLD),
                        ActivityOutcome::Denied => theme::tool_denied_style(),
                        ActivityOutcome::Cancelled => theme::muted().add_modifier(Modifier::BOLD),
                        ActivityOutcome::TimedOut => theme::tool_timeout_style(),
                    };
                    let mut spans = Vec::new();
                    spans.push(Span::styled(
                        if p.expanded { "  ▾ " } else { "  ▸ " },
                        theme::metadata_style(),
                    ));
                    if p.category.is_some() {
                        // The accent bar marks live structure; history gets
                        // the quiet separator weight instead (DESIGN-007).
                        // Uncategorized tool rows keep the same 2-cell slot
                        // as blank space so labels align with grouped rows.
                        let bar = if in_latest_turn {
                            theme::accent_style()
                        } else {
                            theme::metadata_style()
                        };
                        spans.push(Span::styled("  ", bar));
                    } else {
                        spans.push(Span::styled("  ", theme::metadata_style()));
                    }
                    spans.push(Span::styled(p.label, label_style));
                    spans.push(Span::styled("  ", theme::metadata_style()));
                    if p.subcommands.is_empty() {
                        match collapsed_command_summary(&p.count_label, &p.items) {
                            Some((command, output_lines)) => {
                                spans.push(Span::styled(command, theme::metadata_style()));
                                spans.push(Span::styled(
                                    format!(" · {output_lines} output lines"),
                                    theme::dim(),
                                ));
                            }
                            None => {
                                spans.push(Span::styled(p.count_label, theme::metadata_style()))
                            }
                        }
                    }
                    // Only once recovered: a still-failing group shouldn't
                    // show a badge that reads as "this is fine now."
                    // Chronological outcome counts only — never a
                    // "retry/recovered" claim from adjacency (§7). Recovery
                    // wording requires evidence linking the attempts.
                    if p.outcome == ActivityOutcome::Success && p.retries > 0 {
                        let failed_label = if p.retries == 1 {
                            "1 failed".to_string()
                        } else {
                            format!("{} failed", p.retries)
                        };
                        let label = format!(
                            "{} {} · {}",
                            p.items.len().saturating_add(p.retries),
                            if p.items.len().saturating_add(p.retries) == 1 {
                                "command"
                            } else {
                                "commands"
                            },
                            failed_label
                        );
                        spans.push(Span::styled(format!(" · {label}"), theme::dim()));
                    }
                    spans.push(Span::styled(
                        activity_detail_label(p.expanded),
                        theme::dim(),
                    ));
                    let line = Line::from(spans);
                    lines.push(line);
                    for subcommand in p.subcommands.iter() {
                        let sub_width = width.saturating_sub(5);
                        for wrapped in wrap(subcommand, sub_width) {
                            let sub_line = Line::from(Span::styled(
                                format!("{INDENT_UNIT}{wrapped}"),
                                theme::muted(),
                            ));
                            lines.push(sub_line);
                        }
                    }
                    if p.expanded {
                        let rendered_items: Vec<String> = p
                            .items
                            .iter()
                            .flat_map(|item| {
                                wrap(item, width.saturating_sub(MESSAGE_PADDING)).into_iter()
                            })
                            .collect();
                        let visible = rendered_items.len();
                        let shown = rendered_items.iter().take(7);
                        for wrapped in shown {
                            let item_line = Line::from(Span::styled(
                                format!("{INDENT_UNIT}{wrapped}"),
                                theme::muted(),
                            ));
                            lines.push(item_line);
                        }
                        if visible > 7 {
                            lines.push(Line::from(Span::styled(
                                format!("{INDENT_UNIT}... {} more lines", visible - 7),
                                theme::dim(),
                            )));
                        }
                    }
                }
                ConversationBlock::ApprovalPending(p) => {
                    lines.extend(render_approval_card(&p, prose_width));
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ConversationBlock::QuestionPending(p) => {
                    lines.extend(render_question_card(&p, prose_width));
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ConversationBlock::Callout(p) => {
                    let st = match p.kind {
                        BannerKind::Info => theme::info(),
                        BannerKind::Warn => theme::warn(),
                        BannerKind::Error => theme::error_callout(),
                        BannerKind::Ok => theme::ok(),
                    };
                    for l in wrap(&p.text, width) {
                        lines.push(Line::from(Span::styled(l, st)));
                    }
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ConversationBlock::CodeBlock(p) => {
                    // `render_markdown` already renders a fenced block with
                    // its rail and syntax colours. Styling the returned lines
                    // again painted a second ground over the top of it.
                    // Tool output is markdown-rendered too, so a URL the tool
                    // printed is a link here exactly as it is in an answer.
                    for line in render_markdown_links(&p.text, width) {
                        lines.push(line);
                    }
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ConversationBlock::DiffBlock(p) => {
                    lines.push(diff_title_line(&p.path, &p.lines));
                    if !p.rationale.is_empty() {
                        // Inside the card's own border+padding, so the
                        // rationale shares the diff content's left edge rather
                        // than sitting two columns in from it.
                        for l in wrap(&p.rationale, width.saturating_sub(4))
                            .into_iter()
                            .take(2)
                        {
                            lines.push(Line::from(Span::styled(
                                l,
                                theme::muted().add_modifier(Modifier::ITALIC),
                            )));
                        }
                        // A blank keeps the rationale from reading as the first
                        // line of the diff.
                        lines.push(Line::from(""));
                    }
                    lines.extend(render_numbered_diff(
                        &p.path,
                        &p.lines,
                        width.saturating_sub(2),
                    ));
                    lines.push(Line::from(DIFF_BLOCK_END_MARKER));
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ConversationBlock::VerificationBlock(p) => {
                    lines.extend(render_verification_card(&p, width));
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ConversationBlock::PlanChecklist(p) => {
                    // Pulse only while a turn runs: settled history renders
                    // the bright frame, matching the live turn line and the
                    // footer's running dot which animate only while working.
                    let pulse_dim = self.opts.busy && self.opts.pulse_dim;
                    lines.extend(render_plan_checklist_with_pulse(&p, width, pulse_dim));
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ConversationBlock::Home(p) => {
                    lines.extend(render_home_card(&p, prose_width, self.opts.compact));
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ConversationBlock::TurnSummary(p) => {
                    // §11 receipt: one muted row, neutral finish wording.
                    // "Response finished" describes answer delivery, not task
                    // success — never a bold verdict competing with the answer.
                    let mut spans = vec![
                        Span::styled("      ", theme::metadata_style()),
                        Span::styled(
                            format!("Response finished · {}", format_elapsed_tenths(p.secs)),
                            theme::metadata_style(),
                        ),
                    ];
                    // Tokens per second, from the provider's own count for
                    // this turn. It belongs here and not on the live line:
                    // usage only arrives once the turn is over, and a rate in
                    // characters moved with how verbose the model was being
                    // rather than how fast it was going.
                    let detail = format!(
                        "   {}",
                        turn_stats_detail(p.chars, p.tokens_per_second(), p.tools)
                    );
                    spans.push(Span::styled(detail, theme::metadata_style()));
                    lines.push(Line::from(spans));
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ConversationBlock::Metadata(p) => {
                    // Metadata is a one-line summary — `block_height` budgets
                    // exactly one row for it — and its long content is almost
                    // always a path. Wrapping both overran that budget and cut
                    // the end off the path; eliding keeps it to one line and
                    // keeps the folder name.
                    let fitted = crate::path_display::elide_path(&p.text, width);
                    lines.push(Line::from(Span::styled(fitted, theme::muted())));
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ConversationBlock::Thinking(p) if p.collapsed => {
                    // Spent reasoning: one line saying it happened and how
                    // long it took, rather than a dim paragraph the reader has
                    // already scrolled past.
                    //
                    // Aligned with the answer, not with the deeper indent that
                    // expanded reasoning uses. That indent subordinates a block
                    // of text to the answer around it; on a single line sitting
                    // above the answer it subordinates nothing and just starts
                    // the reply on a different left edge from everything under
                    // it.
                    let indent = " ".repeat(MESSAGE_PADDING);
                    let label = match p.duration_secs {
                        Some(secs) => format!("Thought for {}", format_elapsed_tenths(secs)),
                        None => "Thought".to_string(),
                    };
                    lines.push(Line::from(vec![
                        Span::styled(indent, theme::dim()),
                        Span::styled(label, theme::dim().add_modifier(Modifier::ITALIC)),
                    ]));
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ConversationBlock::Thinking(p) => {
                    // Recedes rather than announces: no glyph, no bold label,
                    // no status word — deeper-indented and dim so it reads as
                    // background reasoning, not another activity item.
                    lines.extend(render_thinking(&p.text, p.duration_secs, width));
                    // A blank closes the reasoning block, the same as every
                    // other major block: the tool/activity rows this reasoning
                    // produced are rail rows and get no separator of their
                    // own, so without one here they would hug the thoughts.
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
            }
        }
        (lines.into_inner(), start_block == 0)
    }
}

pub struct ConversationLinesWidget<'a> {
    pub lines: &'a [HyperlinkLine],
    pub tail_lines: &'a [HyperlinkLine],
    /// Rebuilt every frame and never cached: the live turn line animates, so
    /// caching it by content length would freeze it.
    pub status_lines: &'a [HyperlinkLine],
    /// Whether this terminal renders `OSC 8`. Resolved by the caller so a test
    /// can pin the behaviour instead of inheriting the environment.
    pub hyperlinks: bool,
    pub scroll: u16,
    pub follow: bool,
    pub bottom_padding: u16,
    /// Hold the transcript against the composer once a conversation has
    /// started, instead of letting a short one float at the top of the pane
    /// with the live edge stranded mid-screen.
    pub anchor_bottom: bool,
    /// Stands in for the plan card once it has scrolled above the window.
    pub plan_dock: Option<&'a PlanDock>,
    /// Records each approval/question option row's screen rect this paint, for
    /// mouse hit-testing. `None` in tests.
    pub option_sink: Option<&'a std::cell::RefCell<Vec<(usize, Rect)>>>,
    /// Option index under the pointer; tints that row only.
    pub hover_option: Option<usize>,
}

/// The three slices a transcript frame is painted from, in paint order:
/// settled history, the in-flight preview, and the live turn line.
#[derive(Clone, Copy)]
pub(super) struct TranscriptSlices<'a> {
    pub lines: &'a [HyperlinkLine],
    pub tail_lines: &'a [HyperlinkLine],
    pub status_lines: &'a [HyperlinkLine],
    /// Whether the surrounding terminal renders `OSC 8` at all. Resolved once
    /// per paint rather than once per row, and `false` leaves every row exactly
    /// as it renders today.
    pub hyperlinks: bool,
}

/// Locate the latest visible plan card inside `lines`, and build its dock row.
///
/// The card is found by rendering it standalone and matching its first and
/// last rows against what was produced. This avoids a second full semantic
/// projection solely to locate the plan.
fn plan_dock_for(
    model: &ConversationModel,
    available_width: usize,
    lines: &[HyperlinkLine],
) -> Option<PlanDock> {
    // §3/§8: pin only an unfinished plan belonging to the active turn.
    // A completed plan stays as one compact muted block in history — it
    // never docks above the final answer competing with it.
    let width = available_width.max(4);
    let prose_width = prose_width_for(width);
    let turn_boundaries = model.turn_boundaries();
    let latest_turn = turn_boundaries.last().copied();
    let plan = model
        .items
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, item)| match item {
            ChatItem::PlanChecklist {
                explanation,
                steps,
                evidence,
            } => {
                let turn = turn_boundaries
                    .iter()
                    .copied()
                    .rev()
                    .find(|&start| start <= index);
                let visible = match turn {
                    None => true,
                    Some(start) => Some(start) == latest_turn,
                };
                visible.then(|| PlanChecklistPresentation {
                    explanation: explanation.clone(),
                    steps: steps.clone(),
                    evidence: evidence.clone(),
                })
            }
            _ => None,
        })
        // Only an unfinished plan docks: once every step reports complete,
        // there is no active work to track and the dock would outrank the
        // final answer it should yield to.
        .filter(|plan| {
            plan.steps
                .iter()
                .any(|item| item.status != forge_types::PlanStepStatus::Completed)
        })?;
    // Follow mode renders only a tail window, so a plan far enough back is
    // not in `lines` at all — which is exactly when it most needs docking.
    // Treat "not rendered" as "above the window", not as "no plan".
    // The dock is a settled summary row: no pulse, matching history.
    let card = render_plan_checklist(&plan, width);
    let located = card.first().zip(card.last()).and_then(|(first, last)| {
        let (first, last) = (line_plain(first), line_plain(last));
        let head = lines
            .iter()
            .rposition(|line| line_plain(&line.line) == first)?;
        lines[head..]
            .iter()
            .position(|line| line_plain(&line.line) == last)
            .map(|offset| head + offset + 1)
    });
    let end = located.unwrap_or(0);

    let done = plan
        .steps
        .iter()
        .filter(|item| item.status == forge_types::PlanStepStatus::Completed)
        .count();
    let total = plan.steps.len();
    let current = plan
        .steps
        .iter()
        .find(|item| item.status == forge_types::PlanStepStatus::InProgress)
        .map(|item| item.step.as_str());
    let mut spans = vec![
        Span::styled("\u{2191} ", theme::muted()),
        Span::styled("Plan  ", theme::metadata_style()),
        Span::styled(format!("{done}/{total} done"), theme::muted()),
    ];
    if let Some(step) = current {
        spans.push(Span::styled("  \u{b7}  ", theme::muted()));
        spans.push(Span::styled(
            crate::path_display::elide_middle(step, prose_width.saturating_sub(20).max(8)),
            theme::text(),
        ));
    }
    Some(PlanDock {
        end,
        summary: Line::from(spans).into(),
    })
}

/// A line's text with styling dropped, for comparing two renderings of the
/// same content.
fn line_plain(line: &Line<'static>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

/// A one-row stand-in for the plan card, and where the card it stands for
/// ends.
///
/// The card is worth its height while it is on screen and worth nothing once
/// it has scrolled past — which measurement showed happens about four seconds
/// into a turn. When the card is above the window this row takes the top of
/// the pane instead, so the plan is never simply gone.
#[derive(Debug, Clone)]
pub struct PlanDock {
    /// Index just past the plan card's last line, within the rendered slice.
    pub(super) end: usize,
    pub(super) summary: HyperlinkLine,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render_conversation_lines(
    slices: TranscriptSlices<'_>,
    scroll_from_bottom: u16,
    follow: bool,
    bottom_padding: u16,
    anchor_bottom: bool,
    dock: Option<&PlanDock>,
    area: Rect,
    option_rects: Option<&mut Vec<(usize, Rect)>>,
    hover_option: Option<usize>,
    buf: &mut Buffer,
) {
    theme::fill(area, buf, theme::assistant_message());
    let TranscriptSlices {
        lines,
        tail_lines,
        status_lines,
        hyperlinks,
    } = slices;
    let tail_end = lines.len().saturating_add(tail_lines.len());
    let content_len = tail_end.saturating_add(status_lines.len());
    let total = content_len.saturating_add(bottom_padding as usize);
    let max_scroll = total.saturating_sub(area.height as usize);
    let scroll = if follow {
        max_scroll
    } else {
        max_scroll.saturating_sub((scroll_from_bottom as usize).min(max_scroll))
    };
    let end = scroll.saturating_add(area.height as usize).min(total);
    // Borrowed, not cloned: these lines come from the render cache and are
    // reused every frame. Deep-copying each visible one (and every owned string
    // inside its spans) was pure per-frame waste.
    let blank = HyperlinkLine::default();
    let mut visible = (scroll..end)
        .map(|index| {
            if index < lines.len() {
                &lines[index]
            } else if index < tail_end {
                &tail_lines[index - lines.len()]
            } else if index < content_len {
                &status_lines[index - tail_end]
            } else {
                &blank
            }
        })
        .collect::<Vec<_>>();
    // The plan card has scrolled above the window: its one-row stand-in takes
    // the top of the pane. Measured before this existed, the card was on
    // screen for 8 frames out of 70 and nothing afterwards said a plan
    // existed, which step was running, or how far in it was.
    if let Some(dock) = dock.filter(|dock| dock.end <= scroll) {
        if let Some(first) = visible.first_mut() {
            *first = &dock.summary;
        }
    }
    // Short transcripts painted from the top left the newest line — the one
    // being written — floating in the middle of the pane with a screen of
    // nothing under it. Push them down so the live edge sits where the reader
    // is already looking: just above the composer.
    let area = if anchor_bottom && total < area.height as usize {
        let offset = area.height.saturating_sub(total as u16);
        Rect::new(
            area.x,
            area.y.saturating_add(offset),
            area.width,
            total as u16,
        )
    } else {
        area
    };
    render_visible_conversation_lines(&visible, area, buf, option_rects, hover_option, hyperlinks);
}

/// Paint one row, then attach the destinations its columns hide.
///
/// The marking happens after the row is painted because the escape sequence has
/// no width: it wraps the cell symbols that are already in place, so the row's
/// geometry is settled before any of it is emitted.
fn paint_row(line: &HyperlinkLine, rect: Rect, buf: &mut Buffer, hyperlinks: bool) {
    (&line.line).render(rect, buf);
    if hyperlinks {
        links::mark_buffer_hyperlinks(buf, rect, &line.links);
    }
}

pub(super) fn render_visible_conversation_lines(
    lines: &[&HyperlinkLine],
    area: Rect,
    buf: &mut Buffer,
    mut option_rects: Option<&mut Vec<(usize, Rect)>>,
    hover_option: Option<usize>,
    hyperlinks: bool,
) {
    let mut index = 0;
    let mut y = area.y;
    while index < lines.len() && y < area.bottom() {
        if lines[index]
            .spans
            .first()
            .is_some_and(|span| span.content == DIFF_BLOCK_MARKER)
        {
            let end = lines[index + 1..]
                .iter()
                .position(|line| {
                    line.spans
                        .first()
                        .is_some_and(|span| span.content == DIFF_BLOCK_END_MARKER)
                })
                .map_or(lines.len(), |offset| index + 1 + offset);
            let block_height =
                (end - index + 2).min(area.bottom().saturating_sub(y) as usize) as u16;
            let block_area = Rect::new(area.x, y, area.width, block_height);
            let title = Line::from(lines[index].spans[1..].to_vec());
            let block = Block::default()
                .title(title)
                .borders(Borders::ALL)
                .padding(Padding::horizontal(1))
                .border_style(theme::panel_border())
                .style(theme::panel());
            let inner = block.inner(block_area);
            block.render(block_area, buf);
            // One row per line, same as an unwrapped `Paragraph` over the same
            // slice, but without cloning the lines to build one.
            for (offset, line) in lines[index + 1..end].iter().enumerate() {
                let row = inner.y.saturating_add(offset as u16);
                if row >= inner.bottom() {
                    break;
                }
                paint_row(
                    line,
                    Rect::new(inner.x, row, inner.width, 1),
                    buf,
                    hyperlinks,
                );
            }
            y = y.saturating_add(block_height);
            index = end.saturating_add(1);
        } else {
            let line = lines[index];
            let rect = Rect::new(area.x, y, area.width, 1);
            match option_row_index(line) {
                Some(option) => {
                    if let Some(rects) = option_rects.as_deref_mut() {
                        rects.push((option, rect));
                    }
                    if hover_option == Some(option) {
                        // Hover is a pointer affordance: raised ground, bold
                        // weight and the shared `›` marker on the first row of
                        // the option. The marker cell is reserved, so nothing
                        // shifts and the row's own link columns still point at
                        // the cells they named; selection still outranks hover.
                        hovered_option_line(line).render(rect, buf);
                        if hyperlinks {
                            links::mark_buffer_hyperlinks(buf, rect, &line.links);
                        }
                    } else {
                        paint_row(line, rect, buf, hyperlinks);
                    }
                }
                None => paint_row(line, rect, buf, hyperlinks),
            }
            y = y.saturating_add(1);
            index += 1;
        }
    }
}

impl Widget for ConversationLinesWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let mut collected = self.option_sink.map(|cell| cell.borrow_mut());
        render_conversation_lines(
            TranscriptSlices {
                lines: self.lines,
                tail_lines: self.tail_lines,
                status_lines: self.status_lines,
                hyperlinks: self.hyperlinks,
            },
            self.scroll,
            self.follow,
            self.bottom_padding,
            self.anchor_bottom,
            self.plan_dock,
            area,
            collected.as_deref_mut(),
            self.hover_option,
            buf,
        );
    }
}

/// Detect language from file path, returning language name for syntax highlighting.
pub(super) fn lang_from_path(path: &str) -> Option<&'static str> {
    if path.to_ascii_lowercase().ends_with(".md") {
        return Some("markdown");
    }
    let language = forge_syntax::detect_from_path(path);
    (language != forge_syntax::SyntaxLanguage::Unknown).then(|| language.as_str())
}

fn approval_question(tool: &str) -> &'static str {
    if forge_governance::is_shell_tool(tool) {
        "Forge wants to run a shell command."
    } else {
        "Forge wants to run this tool."
    }
}

/// Below this inner width the card drops everything optional — the reason
/// line and the inline consequences — and keeps only what the decision needs.
/// The sidebar is around twenty columns wide, where each of those wraps to
/// three or four rows and pushes the card's own title off the pane.
const APPROVAL_COMPACT_WIDTH: usize = 40;

/// Fewest columns worth giving an inline consequence. Below this it would be
/// elided down to noise, so it is dropped instead.
const APPROVAL_MIN_HELP_COLUMNS: usize = 14;

/// Tool subjects named under one plan step before the rest are counted.
const PLAN_EVIDENCE_ITEMS: usize = 3;

/// Rows one option's description may spend on the question card.
const QUESTION_DESCRIPTION_LINES: usize = 2;

/// Rows the category explanation may spend when it is the only thing saying
/// why the prompt appeared.
const APPROVAL_REASON_LINES: usize = 3;

/// Rows it may spend once the sandbox's own words are above it. The
/// explanation is then a footnote to evidence the operator can already read,
/// and the five-line version was taller than the command it was about.
const APPROVAL_REASON_LINES_WITH_FAILURE: usize = 1;

/// First sentence of a help string, which is the part that says what happens.
///
/// The rest is qualification — where a rule would be written, the pattern it
/// would match — and belongs to the selected option's full text, not to a
/// one-line summary sitting beside a label.
fn short_consequence(help: &str) -> String {
    match help.split_once(". ") {
        Some((first, _)) => first.to_string(),
        None => help.trim_end_matches('.').to_string(),
    }
}

/// Title in the approval card's top border.
const APPROVAL_TITLE: &str = "Approval needed";

/// Starter prompts on the first screen. Concrete enough to be worth pressing,
/// generic enough to fit any repository.
const HOME_STARTERS: &[&str] = &[
    "Explain what this project does",
    "Check this project's error handling",
    "Add tests for a behavior",
];

/// Width of the label column on the home card.
const HOME_LABEL_WIDTH: usize = 11;

/// The first screen.
///
/// It used to be a version string, a clipped path and an orphaned
/// `· 20 skills`, then four hundred pixels of nothing — no model, no provider,
/// no connection state, and no suggestion of what to type. Every comparable CLI
/// puts at least the model here.
fn render_home_card(p: &HomePresentation, prose_width: usize, compact: bool) -> Vec<Line<'static>> {
    let prose_width = prose_width.min(CARD_MAX_WIDTH);
    let pad = " ".repeat(MESSAGE_PADDING);
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut row = |spans: Vec<Span<'static>>| {
        let mut all = vec![Span::raw(pad.clone())];
        all.extend(spans);
        out.push(Line::from(all));
    };

    let field = |label: &str, value: Vec<Span<'static>>| {
        let mut spans = vec![Span::styled(
            format!("{label:<HOME_LABEL_WIDTH$}"),
            theme::muted(),
        )];
        spans.extend(value);
        spans
    };

    row(vec![Span::styled(
        "FORGE",
        theme::brand().add_modifier(Modifier::BOLD),
    )]);
    if !compact {
        row(vec![]);
    }
    row(field(
        "model",
        vec![Span::styled(
            crate::path_display::elide_middle(
                &p.model,
                prose_width.saturating_sub(HOME_LABEL_WIDTH),
            ),
            theme::text(),
        )],
    ));
    row(field(
        "provider",
        vec![
            Span::styled(
                crate::path_display::elide_middle(
                    &p.provider,
                    prose_width
                        .saturating_sub(HOME_LABEL_WIDTH + 2 + if p.connected { 9 } else { 13 }),
                ),
                theme::text(),
            ),
            Span::raw("  "),
            if p.connected {
                Span::styled("connected", theme::ok())
            } else {
                Span::styled("not connected", theme::warn())
            },
        ],
    ));
    row(field(
        "workspace",
        vec![Span::styled(
            crate::path_display::elide_path(
                &p.workspace,
                prose_width.saturating_sub(HOME_LABEL_WIDTH + 2),
            ),
            theme::text(),
        )],
    ));
    if !compact {
        row(field(
            "skills",
            vec![Span::styled(
                format!("{} loaded", p.skills_loaded),
                theme::text(),
            )],
        ));
    }
    row(vec![]);
    row(vec![Span::styled("Try one of these", theme::muted())]);
    for starter in HOME_STARTERS {
        row(vec![
            Span::styled("    ", theme::accent_style()),
            Span::styled((*starter).to_string(), theme::text_secondary()),
        ]);
    }
    out
}

/// Render the pending-approval prompt as a bordered card.
///
/// It used to be emitted as bare lines in the transcript flow, styled like any
/// other prose, which left the single most consequential prompt in the product
/// with less visual weight than the empty composer below it. The presentation
/// already carried a `focused` flag documented as "accent border vs muted" —
/// there simply was no border for it to colour.
/// The questionnaire's card.
///
/// Built like the approval card and for the same reason: these are the same
/// weight of decision — the agent has stopped and cannot go on until the
/// operator answers — and they should not look like two unrelated things.
///
/// Every option shows its description, not only the selected one. Choosing
/// between three options means comparing them, and a description that appears
/// only under the cursor makes the reader arrow up and down to do it.
pub(super) fn render_question_card(
    p: &QuestionPendingPresentation,
    prose_width: usize,
) -> Vec<Line<'static>> {
    let prose_width = prose_width.min(CARD_MAX_WIDTH);
    let pad = " ".repeat(MESSAGE_PADDING);
    let total = prose_width.saturating_sub(MESSAGE_PADDING).max(12);
    let inner = total.saturating_sub(4);
    let compact = inner < APPROVAL_COMPACT_WIDTH;
    let border = if p.focused {
        theme::waiting_border()
    } else {
        theme::border_muted()
    };

    let title = if p.question_count > 1 {
        format!(
            "{} ({}/{})",
            p.header,
            p.question_index + 1,
            p.question_count
        )
    } else {
        p.header.clone()
    };

    let mut out: Vec<Line<'static>> = vec![{
        let mut spans = vec![Span::raw(pad.clone())];
        spans.extend(card_top_border(total, Some(&title), border).spans);
        Line::from(spans)
    }];
    let mut row = |spans: Vec<Span<'static>>| {
        let mut all = vec![Span::raw(pad.clone())];
        all.extend(card_content_spans(spans, inner, border, None).spans);
        out.push(Line::from(all));
    };

    let selected_count = p.options.iter().filter(|opt| opt.chosen).count();
    for (line_idx, wrapped) in wrap(&p.question, inner).into_iter().enumerate() {
        let mut spans = vec![Span::styled(wrapped.clone(), theme::text())];
        if line_idx == 0 && p.multi_select {
            // The live count rides the header line, right-aligned: the card
            // holds no border to put a title in, and the count belongs next to
            // the question, not buried with the options.
            let label = format!("{selected_count} selected");
            let used = wrapped.chars().count() + label.chars().count();
            if used + 2 <= inner {
                spans.push(Span::raw(" ".repeat(inner - used)));
            } else {
                spans.push(Span::raw("  "));
            }
            spans.push(Span::styled(label, theme::ok()));
        }
        row(spans);
    }
    row(vec![]);

    for (idx, opt) in p.options.iter().enumerate() {
        let selected = idx == p.selected;
        // The marker is its own span. Folding it into the wrapped string let
        // `wrap` trim the leading space off every unselected row, so the
        // options sat two columns left of the one under the cursor and the
        // list did not read as a list. Selected rows carry the shared `>`
        // grammar (DESIGN-004/013).
        let marker = if selected { "> " } else { "  " };
        let style = if selected {
            theme::text().add_modifier(Modifier::BOLD)
        } else {
            theme::text()
        };
        // Multi-select rows use a checkbox in place of the ordinal, so the
        // checked state reads before the label. Single-select keeps the
        // ordinal: 1-9 already answer the question.
        let (tag, tag_style) = if p.multi_select {
            if opt.chosen {
                ("[x] ".to_string(), theme::ok())
            } else {
                (
                    "[ ] ".to_string(),
                    theme::metadata_style().add_modifier(Modifier::BOLD),
                )
            }
        } else {
            (
                format!("{}. ", idx + 1),
                theme::metadata_style().add_modifier(Modifier::BOLD),
            )
        };
        let lead = marker.chars().count() + tag.chars().count();
        for (n, wrapped) in wrap(&opt.label, inner.saturating_sub(lead))
            .into_iter()
            .enumerate()
        {
            let mut spans = vec![Span::styled(
                if n == 0 {
                    marker.to_string()
                } else {
                    " ".repeat(marker.chars().count())
                },
                theme::accent_style(),
            )];
            if n == 0 {
                spans.insert(0, Span::raw(option_row_marker(idx)));
            }
            spans.push(Span::styled(
                if n == 0 {
                    tag.clone()
                } else {
                    " ".repeat(tag.chars().count())
                },
                tag_style,
            ));
            if !p.multi_select && opt.chosen {
                spans.push(Span::styled("selected ", theme::ok()));
            }
            spans.push(Span::styled(wrapped, style));
            row(spans);
        }
        if let Some(desc) = opt.description.as_deref().filter(|d| !d.is_empty()) {
            if !compact {
                // Capped: with every option explaining itself the card grows
                // by the number of options, and a long description on each of
                // five of them pushes the question itself off a short pane.
                // Two lines is enough to distinguish options; the rest is
                // prose the operator did not ask for.
                let wrapped = wrap(desc, inner.saturating_sub(lead));
                let elided = wrapped.len() > QUESTION_DESCRIPTION_LINES;
                for (n, text) in wrapped
                    .into_iter()
                    .take(QUESTION_DESCRIPTION_LINES)
                    .enumerate()
                {
                    let last = n + 1 == QUESTION_DESCRIPTION_LINES;
                    row(vec![Span::styled(
                        format!(
                            "{}{text}{}",
                            " ".repeat(lead),
                            if elided && last { "…" } else { "" }
                        ),
                        theme::metadata_style(),
                    )]);
                }
            }
        }
    }

    row(vec![]);
    let hint = if p.question_count > 1 && p.multi_select {
        crate::hints::QUESTION_TABS_MULTI
    } else if p.question_count > 1 {
        crate::hints::QUESTION_TABS
    } else if p.multi_select {
        crate::hints::QUESTION_MULTI
    } else {
        crate::hints::QUESTION
    };
    row(crate::hints::hint_spans(hint, inner));

    let mut bottom = vec![Span::raw(pad)];
    bottom.extend(card_bottom_border(total, border).spans);
    out.push(Line::from(bottom));
    out
}

fn render_approval_card(p: &ApprovalPendingPresentation, prose_width: usize) -> Vec<Line<'static>> {
    // Prose runs the width of the pane; the rail-prefixed prompt does not
    // follow it there either.
    let prose_width = prose_width.min(CARD_MAX_WIDTH);
    let pad = " ".repeat(MESSAGE_PADDING);
    // A row spends `pad` + `│` + ` ` + inner — no right wall, so the rail
    // gets back the column a boxed border used to spend closing itself.
    let inner = prose_width.saturating_sub(MESSAGE_PADDING + 2).max(8);
    let compact = inner < APPROVAL_COMPACT_WIDTH;
    let border = if p.focused {
        theme::waiting_border()
    } else {
        theme::border_muted()
    };

    let mut out: Vec<Line<'static>> = Vec::new();
    let mut row = |spans: Vec<Span<'static>>| {
        let mut all = vec![Span::raw(pad.clone()), Span::raw("  ")];
        // Clip, don't wrap further. Content with no break opportunity — a
        // path, a long single-token command — comes back from `wrap` wider
        // than asked for, and without this it would run straight out past
        // the pane instead of stopping at it.
        let mut used = 0usize;
        for span in spans {
            let w = span.width();
            if used + w <= inner {
                used += w;
                all.push(span);
            } else {
                let room = inner - used;
                if room > 0 {
                    let clipped: String = if span.width() > room && room > 1 {
                        format!(
                            "{}…",
                            span.content.chars().take(room - 1).collect::<String>()
                        )
                    } else {
                        span.content.chars().take(room).collect()
                    };
                    all.push(Span::styled(clipped, span.style));
                }
                break;
            }
        }
        out.push(Line::from(all));
    };

    row(vec![Span::styled(
        APPROVAL_TITLE,
        border.add_modifier(Modifier::BOLD),
    )]);

    let section = |label: &'static str| {
        vec![Span::styled(
            label,
            theme::accent_style().add_modifier(Modifier::BOLD),
        )]
    };

    let question = p
        .question
        .as_deref()
        .unwrap_or_else(|| approval_question(&p.tool));
    row(vec![]);
    row(section("Approval summary"));
    for wrapped in wrap(question, inner) {
        row(vec![Span::styled(wrapped, theme::text())]);
    }
    // What the sandbox actually said about this command. The category
    // explanation below reads identically for every command in that category,
    // so the evidence for this one leads.
    if let Some(failure) = p.failure.as_deref().filter(|f| !f.is_empty()) {
        row(vec![]);
        row(vec![Span::styled(
            "Sandbox blocked the command",
            theme::warn().add_modifier(Modifier::BOLD),
        )]);
        for (n, failure_line) in failure.lines().enumerate() {
            let lead = if n == 0 {
                "The sandbox refused it: "
            } else {
                ""
            };
            // The lead shares the row, so it has to come out of the width the
            // text is wrapped to — the reason row below used to leave it out,
            // which pushed the first line past the border and clipped it.
            for (m, wrapped) in wrap(failure_line, inner.saturating_sub(lead.len()))
                .into_iter()
                .enumerate()
            {
                row(vec![
                    Span::styled(
                        if m == 0 { lead } else { "" }.to_string(),
                        theme::metadata_style(),
                    ),
                    Span::styled(wrapped, theme::warn()),
                ]);
            }
        }
    }
    // Why this call was gated. Without it the prompt reads as arbitrary: the
    // reason was already on the payload and simply never shown. Capped, and
    // dropped entirely once the failure above has said the same thing more
    // specifically — five lines of policy is not worth the height.
    let reason_lines = if p.failure.is_some() {
        APPROVAL_REASON_LINES_WITH_FAILURE
    } else {
        APPROVAL_REASON_LINES
    };
    if let Some(reason) = p
        .reason
        .as_deref()
        .filter(|r| !r.is_empty())
        .filter(|_| !compact)
    {
        const LEAD: &str = "Asked because ";
        let wrapped = wrap(reason, inner.saturating_sub(LEAD.len()));
        let elided = wrapped.len() > reason_lines;
        for (n, text) in wrapped.into_iter().take(reason_lines).enumerate() {
            let lead = if n == 0 { LEAD } else { "" };
            let last = n + 1 == reason_lines;
            row(vec![
                Span::styled(lead.to_string(), theme::metadata_style()),
                Span::styled(
                    if elided && last {
                        format!("{text}…")
                    } else {
                        text
                    },
                    theme::muted(),
                ),
            ]);
        }
    }
    row(vec![]);
    row(section("Command to run"));

    let command_lines: Vec<&str> = p.command.lines().collect();
    if command_lines.is_empty() {
        row(vec![Span::styled("(empty command)", theme::muted())]);
    } else {
        for command_line in command_lines {
            for wrapped in wrap(command_line, inner.saturating_sub(2)) {
                row(vec![Span::styled(
                    format!(" {wrapped} "),
                    theme::chat_code_block(),
                )]);
            }
        }
    }

    // A working directory has no spaces to wrap on, so `wrap` returned it
    // whole and it ran straight out through the card's right border. Elide it
    // on separators instead, which also keeps the folder name.
    row(vec![]);
    row(section("Working directory"));
    let cwd_line = approval_location_line(
        &crate::path_display::elide_path(&p.cwd, inner.saturating_sub(3)),
        &p.env_delta,
    );
    for wrapped in wrap(&cwd_line, inner) {
        row(vec![Span::styled(wrapped, theme::muted())]);
    }
    row(vec![]);
    row(section("What would you like to do?"));

    for (idx, opt) in p.options.iter().enumerate() {
        let selected = idx == p.selected;
        // Shared `>` focus grammar (DESIGN-004/013): shape, not color,
        // marks the option about to happen.
        let (marker, style) = if selected {
            ("> ", theme::text().add_modifier(Modifier::BOLD))
        } else {
            ("  ", theme::muted())
        };
        let help = opt.help.as_deref().unwrap_or("").trim();

        // The selected option gets its consequence in full, on its own rows —
        // it is the one about to happen. The others get a short form on the
        // same row as the label, so every option explains itself without the
        // card growing three rows taller than the pane it has to fit in.
        // The label always gets the full width. Reserving a fixed column for
        // the consequence made long labels wrap for no reason, which reads far
        // worse than an option with no inline note.
        let key_w = opt
            .key
            .as_deref()
            .filter(|k| !k.is_empty())
            .map(|k| k.chars().count() + 1)
            .unwrap_or(0);
        let label_lines = wrap(&opt.label, inner.saturating_sub(2 + key_w));
        let label_rows = label_lines.len();
        for (n, wrapped) in label_lines.into_iter().enumerate() {
            let lead = if n == 0 { marker } else { "  " };
            let mut spans = vec![Span::styled(lead.to_string(), theme::accent_style())];
            if n == 0 {
                spans.insert(0, Span::raw(option_row_marker(idx)));
            }
            // The key leads the row it triggers, so the mapping is visible
            // without reading the hint line and counting. In front rather than
            // after the label: trailing, it ate into the room the consequence
            // needs and elided it down to nonsense.
            if let Some(k) = opt.key.as_deref().filter(|k| !k.is_empty()) {
                let text = if n == 0 {
                    format!("{k} ")
                } else {
                    " ".repeat(k.chars().count() + 1)
                };
                spans.push(Span::styled(
                    text,
                    theme::metadata_style().add_modifier(Modifier::BOLD),
                ));
            }
            spans.push(Span::styled(wrapped, style));
            let last = n + 1 == label_rows;
            if !selected && last && !help.is_empty() && !compact {
                let used: usize = spans.iter().map(Span::width).sum();
                let room = inner.saturating_sub(used + 2);
                if room >= APPROVAL_MIN_HELP_COLUMNS {
                    spans.push(Span::raw("  "));
                    spans.push(Span::styled(
                        crate::path_display::elide_middle(&short_consequence(help), room),
                        theme::metadata_style(),
                    ));
                }
            }
            row(spans);
        }
        if selected && !help.is_empty() {
            for wrapped in wrap(help, inner.saturating_sub(4)) {
                row(vec![Span::styled(
                    format!("    {wrapped}"),
                    theme::metadata_style(),
                )]);
            }
        }
    }

    row(vec![]);
    // Built as spans, not wrapped text: `wrap` collapses runs of spaces, which
    // would flatten the gaps that separate one key/verb pair from the next.
    row(crate::hints::hint_spans(crate::hints::APPROVAL, inner));

    out
}

fn approval_location_line(cwd: &str, env_delta: &str) -> String {
    match env_delta {
        "" | "inherited" => cwd.to_string(),
        other => format!("{cwd}  ·  env {other}"),
    }
}

const DIFF_BLOCK_MARKER: &str = "\u{200b}";

const DIFF_BLOCK_END_MARKER: &str = "\u{200c}";

/// Zero-width marker prefixed to an approval/question option's first row,
/// repeated `index + 1` times so the paint pass can recover the option index
/// and record its screen `Rect` for mouse click/hover. Same trick as the diff
/// markers: zero width, so it never changes layout.
const OPTION_ROW_MARKER: char = '\u{2061}';

/// First row of a hovered approval/question option: raised ground, a weight
/// step and the shared `›` marker in the reserved gutter. A row already
/// carrying the selection marker (`>`) or a checkbox keeps it — selection
/// outranks hover and the cell width never changes.
fn hovered_option_line(line: &Line<'static>) -> Line<'static> {
    let mut hovered = line.clone();
    if let Some(gutter) = hovered.spans.get_mut(1) {
        if gutter.content.as_ref() == "  " {
            gutter.content = "› ".into();
            gutter.style = theme::accent_style().add_modifier(Modifier::BOLD);
        }
    }
    hovered.style(theme::surface_hover().add_modifier(Modifier::BOLD))
}

fn option_row_marker(index: usize) -> String {
    std::iter::repeat_n(OPTION_ROW_MARKER, index + 1).collect()
}

fn option_row_index(line: &Line<'_>) -> Option<usize> {
    let content = line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .find(|content| !content.is_empty() && content.chars().all(|c| c == OPTION_ROW_MARKER))?;
    Some(content.chars().count() - 1)
}

const INDENT_UNIT: &str = "  ";

const MESSAGE_PADDING: usize = 2;

/// How wide the answer's text is allowed to run.
///
/// The pane, less a constant gutter — deliberately uncapped. A fixed measure
/// of 72 left roughly two thirds of a wide pane empty while the text wrapped
/// every few words, and most of an agent's answer is list items of ten to
/// twenty-five words: at 72 columns each wraps to three lines, and given the
/// room each is a single line instead. The gutter stays a constant two
/// columns rather than a share of the width, so it reads as a margin at every
/// size instead of growing into leftover space.
fn prose_width_for(width: usize) -> usize {
    // §10: 96-column reading measure within the pane. Code, tool commands
    // and tabular data bypass this and keep full content width at render.
    message_content_width(width).clamp(4, 96)
}

fn message_content_width(width: usize) -> usize {
    width.saturating_sub(MESSAGE_PADDING)
}

/// Widest a *card* may be drawn: the approval, question, home and plan cards.
///
/// Not a reading measure — those cards are mostly short labels and a command,
/// and a three-word command inside a two-hundred-column border reads as a
/// mistake. Prose has no ceiling (see `prose_width`); this is only about how
/// wide a box should be allowed to get around small content.
const CARD_MAX_WIDTH: usize = 80;

/// Pane widths below this drop the rail and indent (flat mode).
const RAIL_MIN_WIDTH: usize = 50;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct NumberedDiffLine {
    pub(super) old: Option<usize>,
    pub(super) new: Option<usize>,
    pub(super) marker: char,
    pub(super) content: String,
    pub(super) header: bool,
    /// Index into the raw patch lines this row came from. `diff --git`,
    /// `---` and `+++` preambles are skipped, so rendered position and raw
    /// position diverge — search hits index raw lines (see `recompute_matches`
    /// in `diff_view.rs`) and need the mapping to survive numbering.
    pub(super) raw: usize,
}

pub(super) fn number_diff_lines(lines: &[String]) -> Vec<NumberedDiffLine> {
    let mut numbered = Vec::new();
    let mut old_line = None;
    let mut new_line = None;

    for (raw, line) in lines.iter().enumerate() {
        if line.starts_with("diff --git ") || line.starts_with("--- ") || line.starts_with("+++ ") {
            continue;
        }
        if line.starts_with("@@") {
            let mut fields = line.split_whitespace();
            let _ = fields.next();
            old_line = fields.next().and_then(|field| parse_hunk_start(field, '-'));
            new_line = fields.next().and_then(|field| parse_hunk_start(field, '+'));
            numbered.push(NumberedDiffLine {
                old: None,
                new: None,
                marker: ' ',
                content: line.clone(),
                header: true,
                raw,
            });
            continue;
        }
        if line.starts_with("\\ No newline") {
            numbered.push(NumberedDiffLine {
                old: None,
                new: None,
                marker: ' ',
                content: line.clone(),
                header: true,
                raw,
            });
            continue;
        }

        let (marker, content) = match line.chars().next() {
            Some(marker @ ('+' | '-' | ' ')) => (marker, line[marker.len_utf8()..].to_string()),
            _ => (' ', line.clone()),
        };
        let (old, new) = match marker {
            '-' => {
                let old = old_line;
                old_line = old_line.map(|line| line + 1);
                (old, None)
            }
            '+' => {
                let new = new_line;
                new_line = new_line.map(|line| line + 1);
                (None, new)
            }
            _ => {
                let old = old_line;
                let new = new_line;
                old_line = old_line.map(|line| line + 1);
                new_line = new_line.map(|line| line + 1);
                (old, new)
            }
        };
        numbered.push(NumberedDiffLine {
            old,
            new,
            marker,
            content,
            header: false,
            raw,
        });
    }
    numbered
}

/// Expand/collapse hint on a tool row.
///
/// Spelled `Ctrl+O` — a chord is written without spaces around the plus — and
/// unbracketed. It is rendered dim, beside a tool name at full text weight, so
/// it stops competing with the label it sits next to.
fn activity_detail_label(_expanded: bool) -> &'static str {
    ""
}

/// Collapsed-line rendering for command-execution activity groups (see
/// `activity_entry_from_tool` and the `ChatItem::ActivityGroup` case in
/// `semantic_blocks_from_items`, both of which set `count_label` to the
/// raw `"$ command"` text for validation/command entries).
///
/// Returns `Some((truncated_command, output_line_count))` when `count_label`
/// is a command line that exceeds [`COMMAND_LINE_MAX_CHARS`]; `None` leaves
/// short commands and non-command summaries (file counts, etc.) untouched so
/// the caller falls back to rendering `count_label` as-is.
fn collapsed_command_summary(count_label: &str, items: &[String]) -> Option<(String, usize)> {
    if count_label.chars().count() <= COMMAND_LINE_MAX_CHARS {
        return None;
    }
    let command = count_label.strip_prefix("$ ")?;
    let segment = first_command_segment(command);
    let mut truncated: String = segment.chars().take(COMMAND_LINE_MAX_CHARS).collect();
    if segment.chars().count() > COMMAND_LINE_MAX_CHARS {
        truncated.push('…');
    }
    let output_lines: usize = items.iter().map(|item| item.lines().count()).sum();
    Some((format!("$ {truncated}"), output_lines))
}

fn parse_hunk_start(value: &str, marker: char) -> Option<usize> {
    value.strip_prefix(marker)?.split(',').next()?.parse().ok()
}

/// Matches the truncation length already used for long single-line summaries
/// elsewhere in this file (see the `wrote ·` write/edit summary above).
const COMMAND_LINE_MAX_CHARS: usize = 80;

/// First command/pipe segment of a (possibly chained) shell command line,
/// splitting at the earliest `;`, `&&`, or `|`.
fn first_command_segment(command: &str) -> &str {
    let mut end = command.len();
    for sep in [";", "&&", "|"] {
        if let Some(idx) = command.find(sep) {
            end = end.min(idx);
        }
    }
    command[..end].trim_end()
}

/// A verification command's verdict: the command, what the runner concluded,
/// and the runner's own words as evidence.
///
/// Sibling of [`render_numbered_diff`]. The headline is right-aligned into the
/// same column the tool cards use, so a column of runs scans vertically.
pub(super) fn render_verification_card(
    p: &forge_transcript::VerificationBlockPresentation,
    width: usize,
) -> Vec<Line<'static>> {
    let (glyph_style, headline_style) = match p.verdict {
        Verdict::Passed { .. } => (theme::ok(), theme::ok()),
        Verdict::Failed { .. } => (theme::danger(), theme::danger()),
        // An unparsed result is neither good news nor bad; it is a gap in the
        // evidence, and it must not borrow the colour of either.
        Verdict::Unparsed { .. } => (theme::warn(), theme::muted()),
    };
    let headline = p.verdict.headline();
    let mut out = Vec::new();

    // INDENT_UNIT (2) + ASCII marker (3) + space + command.
    let left_w = 2 + 3 + 1 + p.command.chars().count();
    let gap = width
        .saturating_sub(left_w + headline.chars().count() + 2)
        .max(1);
    out.push(Line::from(vec![
        Span::raw(INDENT_UNIT),
        Span::styled(p.verdict.glyph().to_string(), glyph_style),
        Span::raw(" "),
        Span::styled(
            p.command.clone(),
            theme::text().add_modifier(Modifier::BOLD),
        ),
        Span::raw(" ".repeat(gap)),
        Span::styled(headline, headline_style),
    ]));

    let last = p.evidence.len().saturating_sub(1);
    for (i, line) in p.evidence.iter().enumerate() {
        let branch = if i == last { "  └ " } else { "  │ " };
        let room = width.saturating_sub(branch.chars().count() + 4);
        let text: String = line.chars().take(room).collect();
        out.push(Line::from(vec![
            Span::raw(INDENT_UNIT),
            Span::styled(branch.to_string(), theme::border_muted()),
            Span::styled(text, theme::muted()),
        ]));
    }

    // Only when there is more to see than what is already shown.
    if !p.expanded {
        let remaining = p.detail.lines().count().saturating_sub(p.evidence.len());
        if remaining > 0 {
            out.push(Line::from(vec![
                Span::raw(INDENT_UNIT),
                Span::styled(format!("    ... {remaining} more lines"), theme::dim()),
            ]));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    use ratatui::text::Line;

    #[test]
    fn file_language_detection_uses_shared_registry() {
        for path in [
            "main.cjs",
            "api.pyi",
            "style.scss",
            "style.sass",
            "style.less",
            "view.tsx",
            "App.java",
            "main.cpp",
            "Dockerfile",
            "Makefile",
        ] {
            assert_eq!(
                lang_from_path(path),
                Some(forge_syntax::detect_from_path(path).as_str()),
                "{path}"
            );
        }
        assert_eq!(lang_from_path("README.md"), Some("markdown"));
        assert_eq!(lang_from_path("notes.txt"), None);
    }

    /// Text of every rendered line, for asserting on content rather than styling.
    /// The visible text of one row. Tests read rows for their text, and a row
    /// is either a plain `Line` or a `HyperlinkLine` depending on whether the
    /// renderer that produced it had a destination to carry.
    trait RowText {
        fn row_text(&self) -> String;
    }

    impl RowText for Line<'static> {
        fn row_text(&self) -> String {
            self.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        }
    }

    impl RowText for HyperlinkLine {
        fn row_text(&self) -> String {
            self.line.row_text()
        }
    }

    /// `find`/`filter` hand out a reference to the reference they were given.
    impl<R: RowText + ?Sized> RowText for &R {
        fn row_text(&self) -> String {
            (**self).row_text()
        }
    }

    fn lines_text<R: RowText>(lines: &[R]) -> String {
        lines
            .iter()
            .map(RowText::row_text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn streaming_thinking_materializes_only_the_requested_tail() {
        let mut cache = StreamPreviewCache::default();
        // Cover both settled paragraphs and one long, still-unsettled block.
        for thinking in [
            "Reasoning paragraph.\n\n".repeat(100),
            "reasoning ".repeat(1000),
        ] {
            let full = render_streaming_preview(
                &thinking,
                "",
                None,
                40,
                usize::MAX,
                &mut cache,
                Density::Compact,
            );
            let tail =
                render_streaming_preview(&thinking, "", None, 40, 12, &mut cache, Density::Compact);
            assert!(!tail.is_empty());
            assert!(tail.len() <= 13, "reasoning exceeded its viewport window");
            assert!(full.ends_with(&tail));
            // Scrolling back must recover the cached lines, not discard history.
            assert_eq!(
                full,
                render_streaming_preview(
                    &thinking,
                    "",
                    None,
                    40,
                    usize::MAX,
                    &mut cache,
                    Density::Compact,
                )
            );
        }
    }

    /// Only the visible tail is materialised, which is what keeps a rebuild
    /// from costing more as the answer grows.
    #[test]
    fn the_stream_cache_materialises_only_the_window() {
        let mut body = String::new();
        for i in 0..80 {
            body.push_str(&format!("Paragraph number {i} of the answer.\n\n"));
        }
        // A live tail, or `keep_from_end` and `keep_from_end - tail.len()` are
        // the same number and the windowing arithmetic goes untested.
        body.push_str("A trailing paragraph still being written");
        let mut cache = StreamMarkdownCache::default();
        let windowed = cache.render(&body, 60, 10, Density::Compact);
        let whole = cache.render(&body, 60, usize::MAX, Density::Compact);

        assert!(windowed.len() <= 10, "got {} lines", windowed.len());
        assert!(
            whole.len() > windowed.len(),
            "the window must actually bite"
        );

        let tail_of_whole = lines_text(&whole[whole.len() - windowed.len()..]);
        assert_eq!(
            lines_text(&windowed),
            tail_of_whole,
            "the window must be the tail of the full render, not a different render"
        );
    }
}
