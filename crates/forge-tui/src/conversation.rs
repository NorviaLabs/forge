//! Drawing a transcript: turning `forge_transcript`'s projection into
//! ratatui lines.
//!
//! Everything here produces `Line<'static>`; nothing here decides *what*
//! the transcript shows. That half is `forge-transcript`, which this module
//! re-exports so `crate::conversation::` keeps naming both.

pub use forge_transcript::*;

use crate::links::{self, HyperlinkLine};
use crate::markdown::{
    preserve_line_breaks, render_markdown, render_markdown_links, render_markdown_open_with,
    render_markdown_with_density_links, Density,
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
        ConversationBlock::ActivityGroup(_)
            | ConversationBlock::ActiveProgress(_)
            | ConversationBlock::AgentRoster(_)
            // Receipts are a trail too: one blank before the first, none
            // between one receipt and the next.
            | ConversationBlock::AgentReceipt(_)
    )
}

/// Shared "· N chars · R tok/s · N tools" suffix, used by both the live
/// `TurnSummary` line.
fn turn_stats_detail(chars: usize, tokens_per_second: Option<f64>, tools: usize) -> String {
    let mut detail = format!("· {} chars", compact_count(chars));
    if let Some(rate) = tokens_per_second {
        detail.push_str(&format!(" · {rate:.0} tok/s"));
    }
    if tools > 0 {
        let unit = if tools == 1 { "tool" } else { "tools" };
        detail.push_str(&format!(" · {tools} {unit}"));
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
            let padding = " ".repeat(width.saturating_sub(Span::raw(&text).width()));
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
            "{old:>number_width$} {new:>number_width$} │ {} ",
            line.marker
        );
        let row_width = Span::raw(&gutter).width() + Span::raw(line.content.as_str()).width();
        let mut spans = vec![
            Span::styled(
                format!("{old:>number_width$} {new:>number_width$} │ "),
                theme::metadata_style(),
            ),
            Span::styled(format!("{} ", line.marker), line_style),
        ];

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
        .map(|line| Span::raw(line).width().div_ceil(width).max(1))
        .sum::<usize>()
        .max(1)
}

fn estimate_block_lines(block: &ConversationBlock, width: usize, prose_width: usize) -> usize {
    let body = match block {
        ConversationBlock::UserMessage(p) => {
            estimate_wrapped_lines(&p.text, width.saturating_sub(MESSAGE_PADDING)).saturating_add(1)
        }
        ConversationBlock::AssistantAnswer(p) => estimate_wrapped_lines(&p.text, prose_width)
            .saturating_add(usize::from(!p.text.trim().is_empty())),
        ConversationBlock::Thinking(p) if p.collapsed => 1,
        ConversationBlock::Thinking(p) => estimate_wrapped_lines(&p.text, prose_width),
        ConversationBlock::CodeBlock(p) => estimate_wrapped_lines(&p.text, width),
        ConversationBlock::DiffBlock(p) => p.lines.len().saturating_add(2),
        ConversationBlock::VerificationBlock(p) => p.evidence.len().saturating_add(1),
        ConversationBlock::Callout(p) => estimate_wrapped_lines(&p.text, width).saturating_add(1),
        ConversationBlock::PlanChecklist(p) => p.steps.len().saturating_add(3),
        ConversationBlock::ActivityGroup(p) => {
            if p.expanded {
                let detail_width = width.saturating_sub(ACTIVITY_DETAIL_INDENT.len());
                let commands = p
                    .subcommands
                    .iter()
                    .map(|command| estimate_wrapped_lines(command, detail_width))
                    .sum::<usize>();
                let output = p
                    .items
                    .iter()
                    .map(|item| estimate_wrapped_lines(item, detail_width))
                    .sum::<usize>();
                1 + commands + output.min(7) + usize::from(output > 7)
            } else {
                1
            }
        }
        ConversationBlock::ActiveProgress(_)
        | ConversationBlock::Metadata(_)
        | ConversationBlock::TurnSummary(_)
        | ConversationBlock::AgentReceipt(_) => 1,
        ConversationBlock::AgentRoster(p) => {
            if p.expanded() {
                1 + p.rows.len().min(AGENT_ROSTER_MAX_ROWS)
                    + usize::from(p.rows.len() > AGENT_ROSTER_MAX_ROWS)
            } else {
                1
            }
        }
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

/// The renderer-owned author label above an answer, shared by the settled
/// transcript and the live preview so the two paint the same row.
fn assistant_label_line() -> Line<'static> {
    Line::from(Span::styled(
        format!("{}Forge", " ".repeat(MESSAGE_PADDING)),
        theme::text().add_modifier(Modifier::BOLD),
    ))
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
    // Reasoning arrives one step per line; keep those lines.
    let text = preserve_line_breaks(text);
    let full_text = match duration_secs {
        Some(secs) => format!("{text} · {}", format_elapsed_tenths(secs)),
        None => text,
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
            &preserve_line_breaks(thinking),
            width.saturating_sub(INDENT_UNIT.chars().count() * 2),
            keep_from_end,
            &suffix,
            Density::Compact,
        );
        lines.extend(style_thinking_lines(thinking_lines));
    }
    if !text.is_empty() {
        // Same rhythm as the settled transcript: one blank between blocks,
        // the author label on the first token, no trailing blank of its own.
        // Settling then repaints the same rows instead of inserting a label
        // above the answer and shifting everything over it.
        if !lines.is_empty() {
            lines.push(Line::from(""));
        }
        lines.push(assistant_label_line());
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
        // The disclosure hint is written once, on the most recent collapsed
        // group, rather than on every tool row of every turn.
        let last_activity_group = blocks
            .iter()
            .rposition(|b| matches!(b, ConversationBlock::ActivityGroup(p) if !p.expanded));
        // A full-width rule opens every turn boundary (every UserMessage
        // after the first block in the transcript) — independent of whether
        // that turn has a plan checklist. The rule is cushioned by one blank
        // row on each side so turns visibly separate.
        //
        // Vertical rhythm is owned here and nowhere else: a block emits no
        // trailing blank of its own. One blank row precedes every non-rail
        // block, rail rows (tool activity) stay tight against each other, and
        // one blank closes the transcript. Blocks that each added their own
        // blank doubled up wherever two kinds of content met.
        let mut seen_any_block = start_block > 0;
        let mut prev_railed = false;
        for (block_index, block) in blocks.into_iter().enumerate().skip(start_block) {
            let is_turn_start = matches!(block, ConversationBlock::UserMessage(_));
            if is_turn_start {
                turn_index += 1;
            }
            let in_latest_turn = total_turns == 0 || turn_index == total_turns;
            let railed = is_railed_block(&block);
            if !is_turn_start && gap && !lines.is_empty() && !(railed && prev_railed) {
                ensure_blank_line(&mut lines);
            }
            prev_railed = railed;
            if is_turn_start && seen_any_block {
                // A turn boundary is one rule with one row of breathing room
                // on each side.
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
                }
                ConversationBlock::AssistantAnswer(p) => {
                    // §5: one renderer-owned Forge label per final-response
                    // region — never one per streamed fragment. The label
                    // lands with the first answer token, so settling changes
                    // nothing above the answer: an empty reserved row read as
                    // a stray double blank while the answer streamed.
                    if !p.text.trim().is_empty() {
                        lines.push(assistant_label_line());
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
                    let marker = match p.outcome {
                        ActivityOutcome::Success => "✓ ",
                        ActivityOutcome::Failure | ActivityOutcome::TimedOut => "✗ ",
                        ActivityOutcome::Blocked
                        | ActivityOutcome::Denied
                        | ActivityOutcome::Warning => "! ",
                        ActivityOutcome::Cancelled => "− ",
                        ActivityOutcome::Neutral => "",
                    };
                    spans.push(Span::styled(marker, label_style));
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
                    } else if !p.expanded {
                        let suffix_width = if width >= 80 {
                            activity_detail_label(false).chars().count()
                        } else {
                            0
                        } + if p.subcommands.len() > 1 {
                            format!(" · +{} more", p.subcommands.len() - 1)
                                .chars()
                                .count()
                        } else {
                            0
                        };
                        let used_width = spans.iter().map(Span::width).sum::<usize>();
                        let preview = crate::path_display::elide_middle(
                            &p.subcommands[0],
                            width.saturating_sub(used_width + suffix_width),
                        );
                        spans.push(Span::styled(preview, theme::metadata_style()));
                        if p.subcommands.len() > 1 {
                            spans.push(Span::styled(
                                format!(" · +{} more", p.subcommands.len() - 1),
                                theme::dim(),
                            ));
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
                    // The hint names a key, not a row: once, on the newest
                    // collapsed group, is enough to teach it.
                    if width >= 80 && last_activity_group == Some(block_index) {
                        spans.push(Span::styled(
                            activity_detail_label(p.expanded),
                            theme::dim(),
                        ));
                    }
                    let line = Line::from(spans);
                    lines.push(line);
                    // Disclosed detail nests under the group's label, not on
                    // the prose edge: at the prose indent a failed command's
                    // output was indistinguishable from the answer below it.
                    let detail_width = width.saturating_sub(ACTIVITY_DETAIL_INDENT.len());
                    for subcommand in p.subcommands.iter().filter(|_| p.expanded) {
                        for wrapped in wrap(subcommand, detail_width) {
                            let sub_line = Line::from(Span::styled(
                                format!("{ACTIVITY_DETAIL_INDENT}{wrapped}"),
                                theme::muted(),
                            ));
                            lines.push(sub_line);
                        }
                    }
                    if p.expanded {
                        let rendered_items: Vec<String> = p
                            .items
                            .iter()
                            .flat_map(|item| wrap(item, detail_width).into_iter())
                            .collect();
                        let visible = rendered_items.len();
                        let shown = rendered_items.iter().take(7);
                        for wrapped in shown {
                            let item_line = Line::from(Span::styled(
                                format!("{ACTIVITY_DETAIL_INDENT}{wrapped}"),
                                theme::muted(),
                            ));
                            lines.push(item_line);
                        }
                        if visible > 7 {
                            lines.push(Line::from(Span::styled(
                                format!("{ACTIVITY_DETAIL_INDENT}... {} more lines", visible - 7),
                                theme::dim(),
                            )));
                        }
                    }
                }
                ConversationBlock::ApprovalPending(p) => {
                    lines.extend(render_approval_card(&p, prose_width));
                }
                ConversationBlock::QuestionPending(p) => {
                    lines.extend(render_question_card(&p, prose_width));
                }
                ConversationBlock::Callout(p) => {
                    // The kind leads as a bracketed tag, not only as colour:
                    // the same `[✓]`/`[!]` shape grammar the lifecycle uses,
                    // so a callout still says what it is with colour stripped.
                    let (tag, st) = match p.kind {
                        BannerKind::Info => ("[i]", theme::info()),
                        BannerKind::Warn => ("[!]", theme::warn()),
                        BannerKind::Error => ("[✗]", theme::error_callout()),
                        BannerKind::Ok => ("[✓]", theme::ok()),
                    };
                    // On the prose edge like every other block: a callout
                    // flush against the pane divider read as chrome, not as a
                    // line of the conversation.
                    let indent = " ".repeat(MESSAGE_PADDING);
                    for l in wrap(&format!("{tag} {}", p.text), message_content_width(width)) {
                        lines.push(Line::from(Span::styled(format!("{indent}{l}"), st)));
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
                }
                ConversationBlock::DiffBlock(p) => {
                    lines.push(diff_title_line(&p.path, &p.lines));
                    // The box is inset by the prose padding on both sides and
                    // draws a border plus one cell of padding inside that.
                    let inner_width = diff_box_inner_width(width);
                    if !p.rationale.is_empty() {
                        // Inside the card's own border+padding, so the
                        // rationale shares the diff content's left edge rather
                        // than sitting two columns in from it.
                        for l in wrap(&p.rationale, inner_width).into_iter().take(2) {
                            lines.push(Line::from(Span::styled(
                                l,
                                theme::muted().add_modifier(Modifier::ITALIC),
                            )));
                        }
                        // A blank keeps the rationale from reading as the first
                        // line of the diff.
                        lines.push(Line::from(""));
                    }
                    lines.extend(render_numbered_diff(&p.path, &p.lines, inner_width));
                    lines.push(Line::from(DIFF_BLOCK_END_MARKER));
                }
                ConversationBlock::VerificationBlock(p) => {
                    lines.extend(render_verification_card(&p, width));
                }
                ConversationBlock::PlanChecklist(p) => {
                    // Pulse only while a turn runs: settled history renders
                    // the bright frame, matching the live turn line and the
                    // footer's running dot which animate only while working.
                    let pulse_dim = self.opts.busy && self.opts.pulse_dim;
                    lines.extend(render_plan_checklist_with_pulse(&p, width, pulse_dim));
                }
                ConversationBlock::Home(p) => {
                    lines.extend(render_home_card(&p, prose_width, self.opts.compact));
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
                        " {}",
                        turn_stats_detail(p.chars, p.tokens_per_second(), p.tools)
                    );
                    spans.push(Span::styled(detail, theme::metadata_style()));
                    lines.push(Line::from(spans));
                }
                ConversationBlock::Metadata(p) => {
                    // Metadata is a one-line summary — `block_height` budgets
                    // exactly one row for it — and its long content is almost
                    // always a path. Wrapping both overran that budget and cut
                    // the end off the path; eliding keeps it to one line and
                    // keeps the folder name.
                    let fitted = crate::path_display::elide_path(&p.text, width);
                    lines.push(Line::from(Span::styled(fitted, theme::muted())));
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
                }
                ConversationBlock::Thinking(p) => {
                    // Recedes rather than announces: no glyph, no bold label,
                    // no status word — deeper-indented and dim so it reads as
                    // background reasoning, not another activity item.
                    lines.extend(render_thinking(&p.text, p.duration_secs, width));
                }
                ConversationBlock::AgentRoster(p) => {
                    lines.extend(render_agent_roster(&p, width, in_latest_turn));
                }
                ConversationBlock::AgentReceipt(p) => {
                    lines.push(render_agent_receipt(&p, width));
                }
            }
        }
        // The one trailing blank the transcript owns, so whatever follows it
        // (the live preview, the turn line, the composer) stands off the last
        // block without that block having to know.
        if gap && !lines.is_empty() {
            ensure_blank_line(&mut lines);
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
            // `index` is the title row and `end` the zero-width end marker, so
            // the content is `end - index - 1` rows. Plus two borders that is
            // `end - index + 1`; the old `+ 2` left one blank panel row inside
            // the box above the bottom edge.
            let block_height =
                (end - index + 1).min(area.bottom().saturating_sub(y) as usize) as u16;
            // Inset to the prose edge: flush against the pane divider the box
            // was the one block that started on a different left edge.
            let inset = DIFF_BOX_INSET.min(area.width / 4);
            let block_area = Rect::new(
                area.x + inset,
                y,
                area.width.saturating_sub(2 * inset),
                block_height,
            );
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

/// Title led by the approval card's first row.
const APPROVAL_TITLE: &str = "Approval needed";

/// Starter prompts on the first screen. Concrete enough to be worth trying,
/// generic enough to fit any repository.
const HOME_STARTERS: &[&str] = &[
    "Explain what this project does",
    "Check this project's error handling",
    "Add tests for a behavior",
];

/// Width of the label column on the home card.
const HOME_LABEL_WIDTH: usize = 10;

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

    if !compact {
        row(vec![Span::styled(
            "FORGE",
            theme::brand().add_modifier(Modifier::BOLD),
        )]);
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
                        .saturating_sub(HOME_LABEL_WIDTH + 1 + if p.connected { 9 } else { 13 }),
                ),
                theme::text(),
            ),
            Span::raw(" "),
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
    if !compact {
        row(vec![]);
    }
    let starter_width = prose_width.saturating_sub(4).max(1);
    let starters = if compact && HOME_STARTERS.iter().any(|s| s.len() > starter_width) {
        &HOME_STARTERS[..1]
    } else {
        HOME_STARTERS
    };
    row(vec![Span::styled(
        if starters.len() == 1 {
            "Try this"
        } else {
            "Try one of these"
        },
        theme::muted(),
    )]);
    for starter in starters {
        for (index, line) in wrap(starter, starter_width).into_iter().enumerate() {
            row(vec![
                Span::styled(
                    if index == 0 { "• " } else { "  " },
                    theme::response_marker(),
                ),
                Span::styled(line, theme::text_secondary()),
            ]);
        }
    }
    out
}

/// The approval prompt's card.
///
/// It used to be emitted as bare lines in the transcript flow, styled like any
/// other prose, which left the single most consequential prompt in the product
/// with less visual weight than the empty composer below it. The presentation's
/// `focused` flag now leads the title row rather than a border.
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
    // Borderless: a row is `pad` + content, so the interior is the pane's
    // width less the pad alone. The four columns a boxed border and its
    // padding used to reserve left the card's right edge four columns short.
    let inner = prose_width.saturating_sub(MESSAGE_PADDING).max(12);
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

    let mut out: Vec<Line<'static>> = Vec::new();
    let mut row = |spans: Vec<Span<'static>>| {
        let mut all = vec![Span::raw(pad.clone())];
        all.extend(card_content_spans(spans, inner, border, None).spans);
        out.push(Line::from(all));
    };

    // No border carries the title, so the header — with its `(n/m)` progress
    // when there is more than one question — leads as a focused row.
    let title_style = if p.focused {
        theme::waiting_border().add_modifier(Modifier::BOLD)
    } else {
        theme::metadata_style().add_modifier(Modifier::BOLD)
    };
    row(vec![Span::styled(title, title_style)]);
    row(vec![]);

    let selected_count = p.options.iter().filter(|opt| opt.chosen).count();
    for (line_idx, wrapped) in wrap(&p.question, inner).into_iter().enumerate() {
        let mut spans = vec![Span::styled(wrapped.clone(), theme::text())];
        if line_idx == 0 && p.multi_select {
            // The live count rides the question's first line, right-aligned:
            // it belongs next to the question it answers, not on the header
            // with the progress or buried with the options.
            let label = format!("{selected_count} selected");
            let label_w = crate::path_display::width(&label);
            let used = crate::path_display::width(&wrapped) + label_w;
            if used + 2 <= inner {
                spans.push(Span::raw(" ".repeat(inner - used)));
                spans.push(Span::styled(label, theme::ok()));
            } else {
                // No room beside a full first line: the count takes its own
                // row, still right-aligned, rather than running off the card.
                row(spans);
                spans = vec![
                    Span::raw(" ".repeat(inner.saturating_sub(label_w))),
                    Span::styled(label, theme::ok()),
                ];
            }
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

    // A child's request leads with who is asking: the card is the same shape
    // as the session's own approval, and the name is what tells them apart.
    let title = match p.asker.as_deref() {
        Some(asker) => format!("{asker} needs approval"),
        None => APPROVAL_TITLE.to_string(),
    };
    row(vec![Span::styled(
        title,
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
    // A child's request is the child's: "fix-tests wants to run a shell
    // command", not Forge's.
    let question = match p.asker.as_deref() {
        Some(asker) if question.starts_with("Forge ") => question.replacen("Forge", asker, 1),
        _ => question.to_string(),
    };
    // The title row already says this is an approval; a second "Approval
    // summary" label under it only repeats the word.
    row(vec![]);
    for wrapped in wrap(&question, inner) {
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
    let hints = match (p.asker.is_some(), p.focused) {
        (true, true) => crate::hints::CHILD_APPROVAL,
        (true, false) => crate::hints::CHILD_APPROVAL_UNFOCUSED,
        (false, _) => crate::hints::APPROVAL,
    };
    row(crate::hints::hint_spans(hints, inner));

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

/// Indent of a disclosed activity group's commands and output: under the
/// group's label (marker, bar slot), not on the prose edge.
const ACTIVITY_DETAIL_INDENT: &str = "      ";

/// Child rows an open roster shows before deferring to `/tasks`.
const AGENT_ROSTER_MAX_ROWS: usize = 6;

/// Glyph that marks a child agent, shared with the footer's agents chip.
const AGENT_GLYPH: &str = "◆";

/// Marker and style for a child's state: the same shape grammar the activity
/// rows use (`✓ ✗ ! −`), so a roster reads as part of the tool trail.
fn agent_state_marker(state: &AgentRowState, in_latest_turn: bool) -> (&'static str, Style) {
    match state {
        AgentRowState::Running => (AGENT_GLYPH, theme::accent_style()),
        AgentRowState::Waiting { .. } => ("!", theme::warn().add_modifier(Modifier::BOLD)),
        AgentRowState::Finished if in_latest_turn => ("✓", theme::tool_success_style()),
        AgentRowState::Finished => ("✓", theme::text_secondary()),
        AgentRowState::Failed { .. } => ("✗", theme::danger().add_modifier(Modifier::BOLD)),
        AgentRowState::Cancelled => ("−", theme::muted()),
    }
}

/// The roster: a rail block with one row per child, standing where the
/// children were spawned.
///
/// ```text
///   ▾   ◆ Agents  3 running · 1 waiting
///       ◆ explore      Writer · running 1m12s
///       ! fix-tests    Writer · waiting on approval: bash rm -rf target   a/d decide · → read
/// ```
///
/// Under 80 columns the mode and the actions go; marker, name and state stay.
/// A finished roster folds to its header, the way a spent tool group does.
fn render_agent_roster(
    p: &AgentRosterPresentation,
    width: usize,
    in_latest_turn: bool,
) -> Vec<Line<'static>> {
    let expanded = p.expanded();
    let running = p
        .rows
        .iter()
        .filter(|r| r.state == AgentRowState::Running)
        .count();
    let waiting = p
        .rows
        .iter()
        .filter(|r| matches!(r.state, AgentRowState::Waiting { .. }))
        .count();
    let failed = p
        .rows
        .iter()
        .filter(|r| matches!(r.state, AgentRowState::Failed { .. }))
        .count();
    let done = p
        .rows
        .iter()
        .filter(|r| matches!(r.state, AgentRowState::Finished))
        .count();
    let mut counts: Vec<String> = Vec::new();
    for (n, word) in [
        (waiting, "waiting"),
        (failed, "failed"),
        (running, "running"),
        (done, "finished"),
    ] {
        if n > 0 {
            counts.push(format!("{n} {word}"));
        }
    }
    let header_style = if waiting > 0 || failed > 0 {
        theme::warn().add_modifier(Modifier::BOLD)
    } else if in_latest_turn {
        theme::accent_style().add_modifier(Modifier::BOLD)
    } else {
        theme::text_secondary().add_modifier(Modifier::BOLD)
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(
            if expanded { "  ▾ " } else { "  ▸ " },
            theme::metadata_style(),
        ),
        Span::styled("  ", theme::metadata_style()),
        Span::styled(format!("{AGENT_GLYPH} "), header_style),
        Span::styled(
            if p.rows.len() == 1 { "Agent" } else { "Agents" },
            header_style,
        ),
        Span::styled(
            // The counts are the one part of the header that can outgrow a
            // narrow pane; they give way from the right, the rarest first.
            crate::path_display::truncate_end(
                &format!("  {}", counts.join(" · ")),
                width.saturating_sub(ACTIVITY_DETAIL_INDENT.len() + 2 + 6),
            ),
            theme::metadata_style(),
        ),
    ])];
    // The verbs the conversation offers act on the marked row; they are named
    // once, on the header, and only when there is a marked row to act on.
    if expanded && width >= 80 && p.selected.is_some() {
        lines[0]
            .spans
            .push(Span::styled("   → read · i insert", theme::dim()));
    }
    if !expanded {
        return lines;
    }
    let narrow = width < 80;
    let name_width = p
        .rows
        .iter()
        .map(|r| r.label.chars().count())
        .max()
        .unwrap_or(0)
        .clamp(4, 24);
    for row in p.rows.iter().take(AGENT_ROSTER_MAX_ROWS) {
        let (marker, style) = agent_state_marker(&row.state, in_latest_turn);
        // The shared `›` cursor grammar: shape, not colour, says which child
        // the conversation's keys will reach.
        let indent = if p.selected == Some(row.task_id) {
            "    › "
        } else {
            ACTIVITY_DETAIL_INDENT
        };
        let mut spans = vec![
            Span::styled(indent.to_string(), theme::accent_style()),
            Span::styled(format!("{marker} "), style),
            Span::styled(
                format!(
                    "{:<name_width$}",
                    crate::path_display::elide_middle(&row.label, name_width)
                ),
                style,
            ),
            Span::styled("  ", theme::metadata_style()),
        ];
        if !narrow {
            if let Some(mode) = row.mode.as_deref().filter(|m| !m.is_empty()) {
                spans.push(Span::styled(format!("{mode} · "), theme::metadata_style()));
            }
        }
        let state_text = match &row.state {
            AgentRowState::Running => format!("running {}", row.elapsed),
            AgentRowState::Waiting { request } => {
                if request.is_empty() || narrow {
                    row.state.word().to_string()
                } else {
                    format!("{}: {request}", row.state.word())
                }
            }
            AgentRowState::Finished => format!("finished {}", row.elapsed),
            AgentRowState::Failed { error } => {
                if error.is_empty() || narrow {
                    format!("failed after {}", row.elapsed)
                } else {
                    format!("failed after {}: {error}", row.elapsed)
                }
            }
            AgentRowState::Cancelled => format!("was cancelled after {}", row.elapsed),
        };
        let actions = match (&row.state, narrow) {
            (AgentRowState::Waiting { .. }, false) => "   a/d decide",
            _ => "",
        };
        let used: usize = spans.iter().map(Span::width).sum::<usize>() + actions.chars().count();
        // Cut at the end, never the middle: the state word and the start of
        // the request or error are what the row is for.
        let state_text = crate::path_display::truncate_end(&state_text, width.saturating_sub(used));
        spans.push(Span::styled(
            state_text,
            if matches!(row.state, AgentRowState::Waiting { .. }) {
                theme::warn()
            } else {
                theme::metadata_style()
            },
        ));
        if !actions.is_empty() {
            spans.push(Span::styled(actions.to_string(), theme::dim()));
        }
        lines.push(Line::from(spans));
    }
    if p.rows.len() > AGENT_ROSTER_MAX_ROWS {
        lines.push(Line::from(Span::styled(
            format!(
                "{ACTIVITY_DETAIL_INDENT}· +{} more · /tasks",
                p.rows.len() - AGENT_ROSTER_MAX_ROWS
            ),
            theme::dim(),
        )));
    }
    lines
}

/// One receipt row at the end of a turn, on the turn summary's indent:
///
/// ```text
///       ✓ explore finished in 1m12s · on forge/explore-7 · → read · i insert
///       ✗ fix-tests failed after 0m48s: exit status 101 · → read
///       ✓ fix-tests · allowed once
/// ```
fn render_agent_receipt(p: &AgentReceiptPresentation, width: usize) -> Line<'static> {
    let (marker, style, verb) = match &p.kind {
        AgentReceiptKind::Finished => (
            "✓",
            theme::tool_success_style(),
            format!("finished in {}", p.elapsed),
        ),
        AgentReceiptKind::Failed { error } => (
            "✗",
            theme::danger().add_modifier(Modifier::BOLD),
            if error.is_empty() {
                format!("failed after {}", p.elapsed)
            } else {
                format!("failed after {}: {error}", p.elapsed)
            },
        ),
        AgentReceiptKind::Cancelled => (
            "−",
            theme::muted(),
            format!("was cancelled after {}", p.elapsed),
        ),
        AgentReceiptKind::Decided { allowed: true } => (
            "✓",
            theme::tool_success_style(),
            "· allowed once".to_string(),
        ),
        AgentReceiptKind::Decided { allowed: false } => {
            ("−", theme::muted(), "· not run".to_string())
        }
    };
    // No key hints here: the conversation's `→`/`i` act on the roster's
    // marked child, not on whichever receipt the eye lands on, and a hint
    // that is sometimes wrong is worse than none. The Tasks view has them all.
    //
    // The row is cut from the right, in order of what it can spare: the
    // branch keeps its last segment, the summary keeps its opening words, and
    // the outcome itself is the last thing to go.
    let lead = format!("      {marker} ");
    let head = crate::path_display::truncate_end(
        &format!("{} {verb}", p.label),
        width.saturating_sub(lead.chars().count()),
    );
    let mut used = lead.chars().count() + head.chars().count();
    let mut spans = vec![
        Span::styled(lead, style),
        Span::styled(head, theme::metadata_style()),
    ];
    let branch = p.branch.as_deref().filter(|b| !b.is_empty()).map(|b| {
        format!(
            "on {}",
            crate::path_display::elide_middle(b, RECEIPT_BRANCH_MAX)
        )
    });
    let branch_cells = branch.as_ref().map_or(0, |b| b.chars().count() + 3);
    if let Some(summary) = p
        .summary
        .as_deref()
        .map(|s| s.lines().next().unwrap_or("").trim())
        .filter(|s| !s.is_empty())
    {
        let room = width.saturating_sub(used + 3 + branch_cells);
        if room >= RECEIPT_SUMMARY_MIN {
            let summary = crate::path_display::truncate_end(summary, room);
            used += summary.chars().count() + 3;
            spans.push(Span::styled(format!(" · {summary}"), theme::dim()));
        }
    }
    if let Some(branch) = branch {
        if width >= used + branch_cells {
            spans.push(Span::styled(format!(" · {branch}"), theme::dim()));
        }
    }
    Line::from(spans)
}

/// Widest a branch name is drawn on a receipt before its middle goes.
const RECEIPT_BRANCH_MAX: usize = 32;
/// Fewer cells than this and a summary is dropped rather than cut to a stub.
const RECEIPT_SUMMARY_MIN: usize = 12;

/// Horizontal inset of the diff box from the pane's edges, each side.
const DIFF_BOX_INSET: u16 = MESSAGE_PADDING as u16;

/// Columns available to a diff row inside the box: the pane less the inset on
/// each side, less the border and one cell of padding on each side.
fn diff_box_inner_width(width: usize) -> usize {
    width.saturating_sub(2 * DIFF_BOX_INSET as usize + 4)
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
fn activity_detail_label(expanded: bool) -> &'static str {
    if expanded {
        ""
    } else {
        " · Ctrl+O details"
    }
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
    let left_w = 2 + 3 + 1 + crate::path_display::width(&p.command);
    let gap = width
        .saturating_sub(left_w + crate::path_display::width(&headline) + 2)
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
        let room = width.saturating_sub(crate::path_display::width(branch) + 4);
        let text = crate::path_display::head(line, room);
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

    /// Cards built from model text stay inside their width with wide text:
    /// unbroken CJK wraps instead of running off the edge, and right-aligned
    /// labels are placed by cells.
    #[test]
    fn cards_keep_wide_text_inside_their_width() {
        use forge_transcript::{
            QuestionPendingPresentation, Verdict, VerificationBlockPresentation,
        };
        let width = 40;
        let question = render_question_card(
            &QuestionPendingPresentation {
                header: "質問".into(),
                question: "東京都庁舎".repeat(12),
                options: vec![],
                selected: 0,
                multi_select: true,
                question_index: 0,
                question_count: 1,
                focused: true,
            },
            width,
        );
        let verification = render_verification_card(
            &VerificationBlockPresentation {
                command: "テスト実行".into(),
                verdict: Verdict::Passed {
                    headline: "成功".into(),
                },
                evidence: vec!["結果".repeat(40)],
                expanded: false,
                detail: String::new(),
            },
            width,
        );
        for line in question.iter().chain(verification.iter()) {
            assert!(line.width() <= width, "{} cells: {line:?}", line.width());
        }
        let text: String = question
            .iter()
            .flat_map(|line| line.spans.iter().map(|span| span.content.to_string()))
            .collect();
        assert_eq!(text.matches('東').count(), 12, "question text was clipped");
        assert!(text.contains("selected"));
    }

    #[test]
    fn wrapped_line_estimate_counts_terminal_cells_not_chars() {
        // Six CJK characters occupy twelve cells: two rows at width 10.
        assert_eq!(estimate_wrapped_lines("漢字漢字漢字", 10), 2);
        assert_eq!(estimate_wrapped_lines("abcdef", 10), 1);
    }

    #[test]
    fn activity_rows_disclose_commands_and_failures() {
        let tool = |name: &str, command: &str, state, outcome| ChatItem::ToolCard {
            name: name.into(),
            summary: command.into(),
            detail: "diagnostic output".into(),
            state,
            duration: None,
            subcommand: Some(command.into()),
            outcome,
        };
        let mut model = ConversationModel {
            items: vec![
                tool(
                    "shell",
                    "echo first",
                    ToolCardState::Done,
                    forge_types::ExecutionOutcome::Success,
                ),
                tool(
                    "shell",
                    "echo second",
                    ToolCardState::Done,
                    forge_types::ExecutionOutcome::Success,
                ),
            ],
            turn_summaries: Vec::new(),
            turn_summary_base: 0,
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts::default(),
        };
        for width in [40, 80, 120] {
            let rows = model.lines_for_width(width);
            let text = lines_text(&rows);
            eprintln!("collapsed ({width} columns):\n{text}");
            assert!(rows.iter().all(|row| row.line.width() <= width), "{text}");
            assert!(text.contains("echo first"), "{text}");
            assert!(text.contains("+1 more"), "{text}");
            assert!(!text.contains("echo second"), "{text}");
            assert!(!text.contains("diagnostic output"), "{text}");
        }
        model.opts.tool_expanded = true;
        let text = lines_text(&model.lines_for_width(80));
        assert!(text.contains("echo second"), "{text}");
        assert!(text.contains("diagnostic output"), "{text}");
        model.opts.tool_expanded = false;
        for name in ["custom_tool", "read_file", "shell"] {
            model.items = vec![tool(
                name,
                "failed operation",
                ToolCardState::Error,
                forge_types::ExecutionOutcome::Failed { exit_code: Some(1) },
            )];
            let text = lines_text(&model.lines_for_width(80));
            assert!(text.contains("diagnostic output"), "{text}");
            assert!(text.contains("✗"), "{text}");
            eprintln!("failure ({name}):\n{text}");
        }
    }

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

    fn single_item_model(item: ChatItem) -> ConversationModel {
        ConversationModel {
            items: vec![item],
            turn_summaries: Vec::new(),
            turn_summary_base: 0,
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts::default(),
        }
    }

    /// Settling a streamed answer must not move it: the label row is reserved
    /// while the answer streams, so the first line of prose lands on the same
    /// row before and after (the caret would otherwise jump).
    #[test]
    fn settling_an_answer_does_not_reflow_it() {
        let text = "Here is the plan.\n\n- one\n- two";
        let first_prose = |rows: &[HyperlinkLine]| {
            rows.iter()
                .position(|row| row.row_text().contains("Here is the plan"))
                .expect("answer prose")
        };
        let streaming = single_item_model(ChatItem::StreamingAssistant { text: text.into() })
            .lines_for_width(80);
        let settled =
            single_item_model(ChatItem::Assistant { text: text.into() }).lines_for_width(80);
        assert_eq!(
            first_prose(&streaming),
            first_prose(&settled),
            "answer shifted when it settled:\nstreaming\n{}\nsettled\n{}",
            lines_text(&streaming),
            lines_text(&settled)
        );
    }

    /// A callout's kind must survive with colour stripped, so it leads with the
    /// shared `[✓]`/`[!]` shape grammar rather than hue alone.
    #[test]
    fn callouts_name_their_kind_without_colour() {
        for (kind, tag) in [
            (BannerKind::Info, "[i]"),
            (BannerKind::Warn, "[!]"),
            (BannerKind::Error, "[✗]"),
            (BannerKind::Ok, "[✓]"),
        ] {
            let text = lines_text(
                &single_item_model(ChatItem::ActivitySummary {
                    label: "did the thing".into(),
                    action: None,
                    kind,
                })
                .lines_for_width(80),
            );
            assert!(text.contains(tag), "{kind:?} lost its tag:\n{text}");
        }
    }

    /// The borderless question card leads with its header, and its `(n/m)`
    /// progress when there is more than one question.
    #[test]
    fn question_card_leads_with_its_header_and_progress() {
        use forge_transcript::QuestionPendingPresentation;
        let card = render_question_card(
            &QuestionPendingPresentation {
                header: "Choose a target".into(),
                question: "Where should this deploy?".into(),
                options: vec![],
                selected: 0,
                multi_select: false,
                question_index: 0,
                question_count: 3,
                focused: false,
            },
            72,
        );
        let text = lines_text(&card);
        assert!(text.contains("Choose a target"), "{text}");
        assert!(text.contains("(1/3)"), "{text}");
        assert!(card.iter().all(|line| line.width() <= 72), "{text}");
    }

    fn model(items: Vec<ChatItem>, opts: ConversationViewOpts) -> ConversationModel {
        ConversationModel {
            items: forge_transcript::group_routine_activity(items),
            turn_summaries: Vec::new(),
            turn_summary_base: 0,
            scroll: 0,
            follow: true,
            opts,
        }
    }

    fn tool(name: &str, summary: &str, outcome: forge_types::ExecutionOutcome) -> ChatItem {
        ChatItem::ToolCard {
            name: name.into(),
            summary: summary.into(),
            detail: "stdout line 1\nstdout line 2".into(),
            state: ToolCardState::from(&outcome),
            duration: None,
            subcommand: Some(summary.into()),
            outcome,
        }
    }

    /// A turn that mixes every kind of content: thinking, a tool trail with a
    /// failure, a plan, a diff, a callout and an answer.
    fn mixed_turn(answer: ChatItem) -> Vec<ChatItem> {
        use forge_types::{ExecutionOutcome, PlanItem, PlanStepStatus};
        vec![
            ChatItem::User {
                text: "Fix the flaky retry test.".into(),
            },
            ChatItem::Thinking {
                text: "Read retry.rs first.".into(),
                duration_secs: Some(4.1),
            },
            tool("read_file", "crates/a.rs", ExecutionOutcome::Success),
            tool("read_file", "crates/b.rs", ExecutionOutcome::Success),
            tool(
                "shell",
                "cargo test -p forge-core retry",
                ExecutionOutcome::Failed { exit_code: Some(1) },
            ),
            ChatItem::PlanChecklist {
                explanation: None,
                steps: vec![PlanItem {
                    step: "Inject a clock".into(),
                    status: PlanStepStatus::Completed,
                }],
                evidence: vec![vec![]],
            },
            ChatItem::DiffCard {
                path: "crates/a.rs".into(),
                lines: vec!["@@ -1 +1 @@".into(), "-a".into(), "+b".into()],
                rationale: String::new(),
            },
            ChatItem::Banner {
                text: "Context at 62%".into(),
                kind: BannerKind::Warn,
            },
            answer,
        ]
    }

    fn rows(model: &ConversationModel, width: usize) -> Vec<String> {
        model
            .lines_for_width(width)
            .iter()
            .map(|l| l.row_text().trim_end().to_string())
            .collect()
    }

    /// Spacing is owned by the loop: never two blank rows in a row, one
    /// blank before every non-rail block, rail rows tight, one blank closing
    /// the transcript. Checked over a mixed turn both settled and streaming.
    #[test]
    fn vertical_rhythm_is_single_blank_everywhere() {
        for answer in [
            ChatItem::Assistant {
                text: "Done.\n\n- one\n- two".into(),
            },
            ChatItem::StreamingAssistant {
                text: "Done.\n\n- one\n- tw".into(),
            },
        ] {
            let m = model(mixed_turn(answer), ConversationViewOpts::default());
            let rows = rows(&m, 100);
            let text = rows.join("\n");
            assert!(!text.contains("\n\n\n"), "double blank:\n{text}");
            assert_eq!(rows.last().map(String::as_str), Some(""), "{text}");
            // The tool trail is tight, then one blank, then the next block.
            let first_tool = rows.iter().position(|r| r.contains("▸")).unwrap();
            assert!(!rows[first_tool + 1].is_empty(), "{text}");
            // Prose follows the callout after exactly one blank.
            let callout = rows.iter().position(|r| r.contains("[!]")).unwrap();
            assert!(rows[callout + 1].is_empty(), "{text}");
            assert!(rows[callout + 2].contains("Forge"), "{text}");
        }
    }

    /// The author label lands with the first streamed token, in the model
    /// path and in the live preview alike, so settling repaints the same rows.
    #[test]
    fn label_is_present_while_streaming_in_both_paths() {
        let streaming = single_item_model(ChatItem::StreamingAssistant {
            text: "Here".into(),
        })
        .lines_for_width(80);
        assert!(lines_text(&streaming).contains("Forge"));

        let mut cache = StreamPreviewCache::default();
        let live = render_streaming_preview(
            "thinking",
            "Here",
            None,
            80,
            usize::MAX,
            &mut cache,
            Density::Airy,
        );
        let live_rows: Vec<String> = live
            .iter()
            .map(|l| l.row_text().trim_end().into())
            .collect();
        let settled = model(
            vec![
                ChatItem::Thinking {
                    text: "thinking".into(),
                    duration_secs: None,
                },
                ChatItem::Assistant {
                    text: "Here".into(),
                },
            ],
            ConversationViewOpts::default(),
        );
        let settled_rows: Vec<String> = rows(&settled, 80)
            .into_iter()
            .map(|r| r.trim_end_matches(STREAM_CARET).trim_end().to_string())
            .collect();
        let settled_rows = &settled_rows[..settled_rows.len() - 1]; // drop the owned trailing blank
        assert_eq!(
            live_rows
                .iter()
                .map(|r| r.trim_end_matches(STREAM_CARET).trim_end().to_string())
                .collect::<Vec<_>>(),
            settled_rows,
            "live preview and settled transcript lay out differently"
        );
    }

    /// Reasoning keeps its line breaks: one step per line in the source is
    /// one row per step on screen, settled and live, while a paragraph break
    /// still reads as a paragraph break.
    #[test]
    fn thinking_keeps_its_line_breaks() {
        let text = "First step.\nSecond step.\n\nNew paragraph.";
        let settled: Vec<String> = render_thinking(text, None, 80)
            .iter()
            .map(|l| l.row_text().trim().to_string())
            .collect();
        assert_eq!(
            settled,
            ["First step.", "Second step.", "", "New paragraph."],
            "{settled:#?}"
        );

        let mut cache = StreamPreviewCache::default();
        let live: Vec<String> =
            render_streaming_preview(text, "", None, 80, usize::MAX, &mut cache, Density::Airy)
                .iter()
                .map(|l| l.row_text().trim().to_string())
                .collect();
        assert_eq!(
            live,
            ["First step.", "Second step.", "", "New paragraph."],
            "{live:#?}"
        );
    }

    /// Callouts share the prose edge instead of hugging the pane divider.
    #[test]
    fn callouts_sit_on_the_prose_edge() {
        let rows = rows(
            &single_item_model(ChatItem::Banner {
                text: "heads up".into(),
                kind: BannerKind::Info,
            }),
            80,
        );
        assert_eq!(
            rows[0],
            format!("{}[i] heads up", " ".repeat(MESSAGE_PADDING))
        );
    }

    /// Disclosed command output nests under the group label rather than
    /// sharing the answer's left edge.
    #[test]
    fn expanded_activity_detail_nests_under_its_label() {
        let m = model(
            mixed_turn(ChatItem::Assistant {
                text: "Done.".into(),
            }),
            ConversationViewOpts {
                tool_expanded: true,
                ..Default::default()
            },
        );
        let rows = rows(&m, 100);
        let output = rows
            .iter()
            .find(|r| r.contains("stdout line 1"))
            .expect("expanded output");
        assert!(output.starts_with(ACTIVITY_DETAIL_INDENT), "{output:?}");
        assert!(!output.starts_with(&" ".repeat(ACTIVITY_DETAIL_INDENT.len() + 1)));
    }

    /// The disclosure hint teaches a key once, on the newest group.
    #[test]
    fn disclosure_hint_appears_once() {
        let m = model(
            mixed_turn(ChatItem::Assistant {
                text: "Done.".into(),
            }),
            ConversationViewOpts::default(),
        );
        let rows = rows(&m, 100);
        let hinted: Vec<_> = rows.iter().filter(|r| r.contains("Ctrl+O")).collect();
        assert_eq!(hinted.len(), 1, "{rows:#?}");
        // The newest *collapsed* group carries it; the failed group after it
        // is already disclosed and has nothing to hint at.
        let collapsed: Vec<_> = rows.iter().filter(|r| r.contains('▸')).collect();
        assert!(collapsed.last().unwrap().contains("Ctrl+O"), "{rows:#?}");
    }

    /// The caret must not borrow the fence gutter's glyph.
    #[test]
    fn stream_caret_is_not_the_code_gutter() {
        assert_ne!(STREAM_CARET, '▌');
        let mut cache = StreamPreviewCache::default();
        let live = render_streaming_preview(
            "",
            "```rust\nlet x",
            None,
            80,
            usize::MAX,
            &mut cache,
            Density::Airy,
        );
        let code = live
            .iter()
            .find(|l| l.row_text().contains("let x"))
            .expect("code row");
        let text = code.row_text();
        assert!(text.trim_start().starts_with('▌'), "{text:?}");
        assert!(text.contains(&format!("let x{STREAM_CARET}")), "{text:?}");
        // The caret keeps its live colour even with the chip after it.
        let caret = code
            .spans
            .iter()
            .find(|s| s.content.as_ref() == STREAM_CARET.to_string())
            .expect("caret span");
        assert_eq!(caret.style.fg, theme::text().fg);
    }

    /// The diff box is inset to the prose edge on both sides.
    #[test]
    fn diff_box_is_inset_from_the_pane_edges() {
        use ratatui::buffer::Buffer;
        let m = single_item_model(ChatItem::DiffCard {
            path: "a.rs".into(),
            lines: vec!["@@ -1 +1 @@".into(), "-a".into(), "+b".into()],
            rationale: String::new(),
        });
        let lines = m.lines_for_width(40);
        let refs: Vec<&HyperlinkLine> = lines.iter().collect();
        let area = Rect::new(0, 0, 40, 10);
        let mut buf = Buffer::empty(area);
        render_visible_conversation_lines(&refs, area, &mut buf, None, None, false);
        let row0: String = (0..40).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert!(row0.starts_with("  ┌"), "{row0:?}");
        assert!(row0.trim_end().ends_with("┐"), "{row0:?}");
        assert_eq!(row0.trim_end().chars().count(), 38, "{row0:?}");
    }

    fn agent_row(label: &str, id: u64, state: AgentRowState) -> AgentRosterRow {
        AgentRosterRow {
            task_id: id,
            label: label.into(),
            mode: Some("Writer".into()),
            state,
            elapsed: "1m12s".into(),
            child_session_id: None,
            turn_ordinal: None,
        }
    }

    /// Every roster row carries its state as a word, the marked row carries
    /// the `›` cursor, and the verbs appear once on the header, only when
    /// there is a marked row to act on.
    #[test]
    fn roster_rows_say_their_state_in_words() {
        let roster = AgentRosterPresentation {
            rows: vec![
                agent_row("explore", 1, AgentRowState::Running),
                agent_row(
                    "fix-tests",
                    2,
                    AgentRowState::Waiting {
                        request: "bash rm -rf target".into(),
                    },
                ),
                agent_row(
                    "docs",
                    3,
                    AgentRowState::Failed {
                        error: "exit status 101".into(),
                    },
                ),
                agent_row("review", 4, AgentRowState::Finished),
            ],
            selected: Some(2),
        };
        let rendered = rows(
            &single_item_model(ChatItem::AgentRoster(roster.clone())),
            100,
        );
        let text = rendered.join("\n");
        assert!(
            text.contains("◆ Agents  1 waiting · 1 failed · 1 running · 1 finished"),
            "{text}"
        );
        assert!(text.contains("→ read · i insert"), "{text}");
        assert!(text.contains("◆ explore"), "{text}");
        assert!(text.contains("running 1m12s"), "{text}");
        assert!(text.contains("› ! fix-tests"), "{text}");
        assert!(
            text.contains("waiting on approval: bash rm -rf target"),
            "{text}"
        );
        assert!(text.contains("a/d decide"), "{text}");
        assert!(text.contains("✗ docs"), "{text}");
        assert!(
            text.contains("failed after 1m12s: exit status 101"),
            "{text}"
        );
        assert!(text.contains("✓ review"), "{text}");
        assert!(text.contains("finished 1m12s"), "{text}");
        assert!(rendered.iter().all(|r| r.chars().count() <= 100), "{text}");

        // Narrow: mode and verbs go, marker, name and state stay.
        let narrow = rows(
            &single_item_model(ChatItem::AgentRoster(roster.clone())),
            60,
        )
        .join("\n");
        assert!(!narrow.contains("Writer"), "{narrow}");
        assert!(!narrow.contains("→ read"), "{narrow}");
        assert!(narrow.contains("fix-tests"), "{narrow}");
        assert!(narrow.contains("waiting on approval"), "{narrow}");

        // Nothing marked: no verbs promised.
        let unmarked = AgentRosterPresentation {
            selected: None,
            ..roster
        };
        let text = rows(&single_item_model(ChatItem::AgentRoster(unmarked)), 100).join("\n");
        assert!(!text.contains("→ read"), "{text}");
        assert!(!text.contains('›'), "{text}");
    }

    /// A roster of finished children folds to its header; one over the row
    /// cap defers the rest to the Tasks view.
    #[test]
    fn roster_folds_when_done_and_caps_its_rows() {
        let done = AgentRosterPresentation {
            rows: vec![
                agent_row("a", 1, AgentRowState::Finished),
                agent_row("b", 2, AgentRowState::Cancelled),
            ],
            selected: None,
        };
        let rows_done = rows(&single_item_model(ChatItem::AgentRoster(done)), 100);
        assert_eq!(rows_done.iter().filter(|r| !r.is_empty()).count(), 1);
        assert!(rows_done[0].contains("▸"), "{rows_done:?}");

        let many = AgentRosterPresentation {
            rows: (0..8)
                .map(|i| agent_row(&format!("agent-{i}"), i, AgentRowState::Running))
                .collect(),
            selected: None,
        };
        let text = rows(&single_item_model(ChatItem::AgentRoster(many)), 100).join("\n");
        assert!(text.contains("agent-5"), "{text}");
        assert!(!text.contains("agent-6"), "{text}");
        assert!(text.contains("+2 more · /tasks"), "{text}");
    }

    /// One receipt per terminal outcome and per decision, each with its
    /// marker and its word.
    #[test]
    fn receipts_name_the_outcome() {
        let receipt = |kind: AgentReceiptKind| {
            ChatItem::AgentReceipt(AgentReceiptPresentation {
                task_id: 1,
                label: "explore".into(),
                kind,
                elapsed: "1m12s".into(),
                summary: Some("Found the race in retry.rs".into()),
                branch: Some("forge/explore-7".into()),
                turn_ordinal: None,
            })
        };
        let text = |kind| rows(&single_item_model(receipt(kind)), 100)[0].clone();
        assert!(
            text(AgentReceiptKind::Finished).contains(
                "✓ explore finished in 1m12s · Found the race in retry.rs · on forge/explore-7"
            ),
            "{}",
            text(AgentReceiptKind::Finished)
        );
        assert!(text(AgentReceiptKind::Failed {
            error: "exit status 101".into()
        })
        .contains("✗ explore failed after 1m12s: exit status 101"));
        assert!(text(AgentReceiptKind::Cancelled).contains("− explore was cancelled after 1m12s"));
        assert!(
            text(AgentReceiptKind::Decided { allowed: true }).contains("✓ explore · allowed once")
        );
        assert!(text(AgentReceiptKind::Decided { allowed: false }).contains("− explore · not run"));
        // No key hints: the conversation's keys act on the roster's marked
        // child, not on a receipt.
        assert!(!text(AgentReceiptKind::Finished).contains("→ read"));
    }

    /// A child's card leads with who is asking and hints the child's keys;
    /// unfocused it says how to reach it.
    #[test]
    fn child_card_names_the_asker_and_its_keys() {
        let card = |focused: bool| ApprovalPendingPresentation {
            tool: "bash".into(),
            command: "rm -rf target".into(),
            cwd: "/tmp/wt".into(),
            env_delta: String::new(),
            question: None,
            reason: None,
            failure: None,
            options: vec![
                ApprovalMenuRow {
                    label: "Don't run".into(),
                    detail: None,
                    help: None,
                    key: Some("d".into()),
                },
                ApprovalMenuRow {
                    label: "Allow once".into(),
                    detail: None,
                    help: None,
                    key: Some("a".into()),
                },
            ],
            selected: 0,
            focused,
            asker: Some("fix-tests".into()),
        };
        let focused = lines_text(&render_approval_card(&card(true), 80));
        assert!(focused.contains("fix-tests needs approval"), "{focused}");
        assert!(focused.contains("rm -rf target"), "{focused}");
        assert!(focused.contains("a allow once"), "{focused}");
        assert!(focused.contains("d don't run"), "{focused}");
        let passive = lines_text(&render_approval_card(&card(false), 80));
        assert!(passive.contains("Tab to this card"), "{passive}");
        assert!(!passive.contains("Esc leave"), "{passive}");
    }

    /// A child moving from running to finished changes its own row and
    /// nothing above it: the answer before the roster keeps its row.
    #[test]
    fn roster_updates_in_place_without_reflowing_the_answer_above() {
        let items = |state: AgentRowState| {
            vec![
                ChatItem::User { text: "go".into() },
                ChatItem::Assistant {
                    text: "Spawning a helper.".into(),
                },
                ChatItem::AgentRoster(AgentRosterPresentation {
                    rows: vec![agent_row("explore", 1, state)],
                    selected: Some(1),
                }),
                ChatItem::Assistant {
                    text: "Meanwhile, here is the plan.".into(),
                },
            ]
        };
        let position = |state: AgentRowState| {
            rows(&model(items(state), ConversationViewOpts::default()), 100)
                .iter()
                .position(|r| r.contains("Spawning a helper"))
                .unwrap()
        };
        assert_eq!(
            position(AgentRowState::Running),
            position(AgentRowState::Finished)
        );
        // Below the roster, the one settle is the fold: a finished roster is
        // its header alone, so what follows moves up by exactly the rows the
        // child row took, once, the way a turn settles elsewhere.
        let below = |state: AgentRowState| {
            rows(&model(items(state), ConversationViewOpts::default()), 100)
                .iter()
                .position(|r| r.contains("here is the plan"))
                .unwrap()
        };
        assert_eq!(
            below(AgentRowState::Running),
            below(AgentRowState::Finished) + 1
        );
    }
}
