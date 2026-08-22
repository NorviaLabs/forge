//! Drawing a transcript: turning `forge_transcript`'s projection into
//! ratatui lines.
//!
//! Everything here produces `Line<'static>`; nothing here decides *what*
//! the transcript shows. That half is `forge-transcript`, which this module
//! re-exports so `crate::conversation::` keeps naming both.

pub use forge_transcript::*;

use crate::markdown::render_markdown;
use crate::status_glyph::{status_glyph, Status};
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

/// Round a count for display: `842`, `1.2k`, `48k`.
pub(super) fn compact_count(n: usize) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=9_999 => format!("{:.1}k", n as f64 / 1_000.0),
        _ => format!("{}k", n / 1_000),
    }
}

/// Add a blank separator line unless the last line is already blank.
pub(super) fn ensure_blank_line(lines: &mut Vec<Line<'static>>) {
    let last_blank = lines
        .last()
        .is_none_or(|l| l.spans.iter().all(|s| s.content.is_empty()));
    if !last_blank {
        lines.push(Line::from(""));
    }
}

/// A bordered card's top edge: `┌─ {title} ───┐` when `title` is set
/// (Approval), or a plain `┌────┐` when it isn't (Plan). `total_width` is the
/// full rendered line width (the card's content width plus its 2 side
/// borders and 2 padding columns).
pub(super) fn card_top_border(
    total_width: usize,
    title: Option<&str>,
    border: Style,
) -> Line<'static> {
    match title {
        Some(title) => {
            let fill = total_width
                .saturating_sub(5)
                .saturating_sub(title.chars().count());
            Line::from(vec![Span::styled(
                format!("┌─ {title} {}┐", "─".repeat(fill)),
                border,
            )])
        }
        None => Line::from(vec![Span::styled(
            format!("┌{}┐", "─".repeat(total_width.saturating_sub(2))),
            border,
        )]),
    }
}

/// A bordered card's bottom edge: `└────┘`.
pub(super) fn card_bottom_border(total_width: usize, border: Style) -> Line<'static> {
    Line::from(vec![Span::styled(
        format!("└{}┘", "─".repeat(total_width.saturating_sub(2))),
        border,
    )])
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
    Line::from(vec![
        Span::styled("│ ", border_style),
        Span::styled(format!("{content}{pad}"), content_style),
        Span::styled(" │", border_style),
    ])
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
    let mut line_spans = vec![Span::styled("│ ", border_style)];
    line_spans.append(&mut spans);
    line_spans.push(Span::styled(
        pad,
        fill.map_or(Style::default(), |bg| Style::default().bg(bg)),
    ));
    line_spans.push(Span::styled(" │", border_style));
    Line::from(line_spans)
}

/// Prepend a rail glyph in the given style to a rendered line.
pub(super) fn prefix_line_with(line: &mut Line<'static>, glyph_style: Style) {
    let mut spans = vec![Span::styled(RAIL_GLYPH, glyph_style), Span::raw(" ")];
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

pub(super) fn render_plan_checklist(
    plan: &PlanChecklistPresentation,
    width: usize,
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
                Span::styled(l, theme::muted().add_modifier(Modifier::ITALIC)),
            ]));
        }
    }
    // A bordered, unfilled card — no "Plan" caption. The border itself
    // signals "this is a checklist"; [✓]/[►]/[ ] checkboxes signal step status.
    let longest_content = plan
        .steps
        .iter()
        .map(|item| item.step.chars().count() + 4) // checkbox + space
        .chain(explanation.map(|e| e.chars().count()))
        .max()
        .unwrap_or(0);
    let available_interior = width.saturating_sub(4);
    let inner_w = longest_content.min(CARD_MAX_WIDTH).min(available_interior);
    let border = theme::accent_style();
    lines.push(card_top_border(inner_w + 4, None, border));
    for (idx, item) in plan.steps.iter().enumerate() {
        let (marker, style) = match item.status {
            PlanStepStatus::Completed => ("[✓]", theme::ok()),
            PlanStepStatus::InProgress => ("[►]", theme::warn()),
            PlanStepStatus::Pending => ("[ ]", theme::muted()),
        };
        let body_width = inner_w.saturating_sub(4).max(4);
        let mut wrapped = wrap(&item.step, body_width).into_iter();
        if let Some(first) = wrapped.next() {
            lines.push(card_content_spans(
                vec![
                    Span::styled(format!("{marker} "), style),
                    Span::styled(first, theme::text()),
                ],
                inner_w,
                border,
                None,
            ));
        }
        for cont in wrapped {
            lines.push(card_content_spans(
                vec![Span::raw("    "), Span::styled(cont, theme::text())],
                inner_w,
                border,
                None,
            ));
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
                lines.push(card_content_spans(
                    vec![
                        Span::raw("    "),
                        Span::styled(wrapped, theme::metadata_style()),
                    ],
                    inner_w,
                    border,
                    None,
                ));
            }
        }
    }
    lines.push(card_bottom_border(inner_w + 4, border));
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
            estimate_wrapped_lines(&p.text, width.saturating_sub(2))
        }
        ConversationBlock::AssistantAnswer(p) => estimate_wrapped_lines(&p.text, prose_width),
        ConversationBlock::Thinking(p) if p.collapsed => 1,
        ConversationBlock::Thinking(p) => estimate_wrapped_lines(&p.text, prose_width),
        ConversationBlock::CodeBlock(p) => estimate_wrapped_lines(&p.text, width),
        ConversationBlock::DiffBlock(p) => p.lines.len().saturating_add(2),
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
        ConversationBlock::Home(p) => render_home_card(p, prose_width).len(),
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
    /// The exact text the cached lines came from. Compared by content rather
    /// than length so a boundary that moves backwards rebuilds instead of
    /// silently reusing the wrong lines.
    prefix: String,
    open_lines: Vec<Line<'static>>,
}

impl StreamMarkdownCache {
    /// Lines for `text`, materialising at most `keep_from_end` of them.
    ///
    /// Caching the parse stops the answer being re-read, but copying every
    /// cached line into the output is O(total lines) on its own, so a long turn
    /// stays quadratic. Only the tail is ever on screen, so only the tail is
    /// built — the same windowing `lines_for_width_from_end` already applies to
    /// the transcript, moved inside a single block.
    fn render(&mut self, text: &str, width: usize, keep_from_end: usize) -> Vec<Line<'static>> {
        let cut = crate::markdown::settled_prefix_len(text);
        let settled = &text[..cut];
        // Grow the cache rather than rebuild it. Re-rendering the whole settled
        // prefix on every boundary advance is O(n) per advance, which is the
        // quadratic this cache exists to remove. Appending is sound for the
        // same reason the split is: each advance lands on a top-level block
        // boundary, where the renderer's state is its initial state.
        if self.width != width || !settled.starts_with(&self.prefix) {
            self.width = width;
            self.prefix.clear();
            self.open_lines.clear();
        }
        if settled.len() > self.prefix.len() {
            let fresh = &settled[self.prefix.len()..];
            self.open_lines
                .extend(crate::markdown::render_markdown_open(fresh, width));
            self.prefix.push_str(fresh);
        }
        let mut tail = crate::markdown::render_markdown_open(&text[cut..], width);
        crate::markdown::fade_streaming_tail(&mut tail);
        let from_prefix = keep_from_end.saturating_sub(tail.len());
        let skip = self.open_lines.len().saturating_sub(from_prefix);
        crate::markdown::render_markdown_join(&self.open_lines[skip..], tail)
    }
}

/// Drawing a [`ConversationModel`].
///
/// An extension trait rather than an inherent impl, because Rust requires
/// inherent impls to live with their type and the model now lives in
/// `forge-transcript`. Callers need this trait in scope.
pub trait ConversationRender {
    /// Render at the transcript's default width.
    fn lines(&self) -> Vec<Line<'static>>;
    /// Render wrapped to `available_width` columns.
    fn lines_for_width(&self, available_width: usize) -> Vec<Line<'static>>;
    /// Render only the last `keep_from_end` estimated lines, walking blocks
    /// from the tail. Follow-mode frames use this so a long transcript does
    /// not rebuild off-screen history.
    fn lines_for_width_from_end(
        &self,
        available_width: usize,
        keep_from_end: usize,
    ) -> Vec<Line<'static>>;
    /// As [`Self::lines_for_width_from_end`], reusing `cache` for the settled
    /// prefix of a streaming answer. Only the live preview passes one.
    fn lines_for_width_from_end_cached(
        &self,
        available_width: usize,
        keep_from_end: usize,
        cache: &mut StreamMarkdownCache,
    ) -> Vec<Line<'static>>;
    /// Shared body of the two above. Not called directly.
    fn render_lines(
        &self,
        available_width: usize,
        keep_from_end: usize,
        stream_cache: Option<&mut StreamMarkdownCache>,
    ) -> Vec<Line<'static>>;
    /// As [`Self::lines_for_width_from_end`], also reporting where the plan
    /// card sits so it can be docked once it scrolls away.
    fn lines_and_plan_dock(
        &self,
        available_width: usize,
        keep_from_end: usize,
    ) -> (Vec<Line<'static>>, Option<PlanDock>);
}

impl ConversationRender for ConversationModel {
    fn lines(&self) -> Vec<Line<'static>> {
        self.lines_for_width(if self.opts.compact { 88 } else { 100 })
    }

    /// Build display lines for the actual conversation viewport. Prose gets a
    /// readable cap; code and structured blocks keep the full pane width.
    fn lines_for_width(&self, available_width: usize) -> Vec<Line<'static>> {
        self.lines_for_width_from_end(available_width, usize::MAX)
    }

    fn lines_for_width_from_end(
        &self,
        available_width: usize,
        keep_from_end: usize,
    ) -> Vec<Line<'static>> {
        self.render_lines(available_width, keep_from_end, None)
    }

    fn lines_for_width_from_end_cached(
        &self,
        available_width: usize,
        keep_from_end: usize,
        cache: &mut StreamMarkdownCache,
    ) -> Vec<Line<'static>> {
        self.render_lines(available_width, keep_from_end, Some(cache))
    }

    fn lines_and_plan_dock(
        &self,
        available_width: usize,
        keep_from_end: usize,
    ) -> (Vec<Line<'static>>, Option<PlanDock>) {
        let lines = self.render_lines(available_width, keep_from_end, None);
        let dock = plan_dock_for(self, available_width, keep_from_end, &lines);
        (lines, dock)
    }

    fn render_lines(
        &self,
        available_width: usize,
        keep_from_end: usize,
        mut stream_cache: Option<&mut StreamMarkdownCache>,
    ) -> Vec<Line<'static>> {
        let width = available_width.max(4);
        let prose_width = prose_width_for(width);
        let mut lines = Vec::new();
        let gap = !self.opts.compact;
        let rail = width >= RAIL_MIN_WIDTH;
        let blocks = self.semantic_blocks();
        let start_block = start_block_for_tail(&blocks, width, prose_width, keep_from_end);
        // A full-width rule opens every turn boundary (every UserMessage
        // after the first block in the transcript) — independent of whether
        // that turn has a plan checklist. Compact tool rows stay tight
        // against each other; major blocks get a blank separator.
        let mut seen_any_block = start_block > 0;
        for block in blocks.into_iter().skip(start_block) {
            let is_turn_start = matches!(block, ConversationBlock::UserMessage(_));
            let railed = is_railed_block(&block);
            if !railed && gap && !lines.is_empty() {
                // Major blocks read as boundaries: separate them from the
                // preceding tool trail with a single blank line.
                ensure_blank_line(&mut lines);
            }
            if is_turn_start && seen_any_block {
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
                    let theme_id = crate::theme::active();
                    let prefix_width = MESSAGE_PADDING;
                    let user_lines = user_message_gutter::render_user_message_lines(
                        &p.text,
                        width.saturating_sub(prefix_width),
                        &theme_id,
                        false,
                        wrap,
                    );
                    for line in user_lines.into_iter() {
                        // No leading marker — just an indent matching
                        // assistant messages' own left padding, with the
                        // highlighted background carried all the way to the
                        // edge so the block reads as one seamless bar.
                        let mut spans = vec![Span::styled(
                            " ".repeat(prefix_width),
                            theme::text().bg(theme::accent_soft_bg()),
                        )];
                        spans.extend(line.spans.into_iter().map(|mut span| {
                            span.style = span.style.bg(theme::accent_soft_bg());
                            span
                        }));
                        let content_width = spans.iter().map(Span::width).sum::<usize>();
                        if content_width < width {
                            spans.push(Span::styled(
                                " ".repeat(width - content_width),
                                theme::text().bg(theme::accent_soft_bg()),
                            ));
                        }
                        lines.push(Line::from(spans));
                    }
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ConversationBlock::AssistantAnswer(p) => {
                    let parts = match stream_cache.as_deref_mut() {
                        Some(cache) if p.streaming => {
                            cache.render(&p.text, prose_width, keep_from_end)
                        }
                        _ => render_markdown(&p.text, prose_width),
                    };
                    for line in parts {
                        let mut spans = vec![Span::raw(" ".repeat(MESSAGE_PADDING))];
                        spans.extend(line.spans);
                        let used = spans.iter().map(Span::width).sum::<usize>();
                        if used < width {
                            spans.push(Span::raw(" ".repeat(width - used)));
                        }
                        lines.push(Line::from(spans).style(theme::assistant_answer_style()));
                    }
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ConversationBlock::ActiveProgress(p) => {
                    let label = format!("{} · {}", p.label, p.summary);
                    let prefix = if p.id == "stream" { "▍ " } else { "● " };
                    let mut line = Line::from(vec![
                        Span::styled(prefix, theme::progress_style()),
                        Span::styled(label, theme::text().add_modifier(Modifier::BOLD)),
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
                    let (prefix, separator) = match p.outcome {
                        ActivityOutcome::Success => (status_glyph(Status::Success), " "),
                        ActivityOutcome::Failure => (status_glyph(Status::Error), " "),
                        ActivityOutcome::Blocked => (Span::styled("⏸", theme::warn()), " "),
                        ActivityOutcome::Warning => (status_glyph(Status::Warning), ""),
                        ActivityOutcome::Neutral => (Span::styled("●", theme::muted()), " "),
                        ActivityOutcome::Denied => {
                            (Span::styled("⊘", theme::tool_denied_style()), " ")
                        }
                        ActivityOutcome::Cancelled => (Span::styled("■", theme::muted()), " "),
                        ActivityOutcome::TimedOut => {
                            (Span::styled("⧖", theme::tool_timeout_style()), " ")
                        }
                    };
                    let mut spans = vec![
                        prefix,
                        Span::raw(separator),
                        Span::styled(p.label, theme::text().add_modifier(Modifier::BOLD)),
                        Span::styled("  ", theme::metadata_style()),
                    ];
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
                    spans.push(Span::styled(
                        activity_detail_label(p.expanded),
                        theme::dim(),
                    ));
                    let mut line = Line::from(spans);
                    if rail {
                        prefix_line_rail(&mut line);
                    }
                    lines.push(line);
                    let rail_extra = if rail { RAIL_EXTRA } else { 0 };
                    for (index, subcommand) in p.subcommands.iter().enumerate() {
                        let last = index + 1 == p.subcommands.len();
                        let glyph = if last { "└─" } else { "├─" };
                        let sub_width = width.saturating_sub(5 + rail_extra);
                        for (lineno, wrapped) in wrap(subcommand, sub_width).into_iter().enumerate()
                        {
                            let head = if lineno == 0 { glyph } else { "│" };
                            let mut sub_line = Line::from(Span::styled(
                                format!("{INDENT_UNIT}{head} {wrapped}"),
                                theme::muted(),
                            ));
                            if rail {
                                prefix_line_rail(&mut sub_line);
                            }
                            lines.push(sub_line);
                        }
                    }
                    if p.expanded {
                        for item in p.items {
                            for wrapped in wrap(&item, width.saturating_sub(2 + rail_extra)) {
                                let mut item_line = Line::from(Span::styled(
                                    format!("{INDENT_UNIT}{wrapped}"),
                                    theme::muted(),
                                ));
                                if rail {
                                    prefix_line_rail(&mut item_line);
                                }
                                lines.push(item_line);
                            }
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
                        lines.push(Line::from(Span::styled(format!("▸ {l}"), st)));
                    }
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ConversationBlock::CodeBlock(p) => {
                    // `render_markdown` already renders a fenced block with
                    // its rail and syntax colours. Styling the returned lines
                    // again painted a second ground over the top of it.
                    for line in render_markdown(&p.text, width) {
                        lines.push(line);
                    }
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ConversationBlock::DiffBlock(p) => {
                    lines.push(diff_title_line(&p.path, &p.lines));
                    if !p.rationale.is_empty() {
                        for l in wrap(&p.rationale, width.saturating_sub(6))
                            .into_iter()
                            .take(2)
                        {
                            lines.push(Line::from(vec![
                                Span::styled(INDENT_UNIT, theme::info()),
                                Span::styled(l, theme::muted().add_modifier(Modifier::ITALIC)),
                            ]));
                        }
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
                ConversationBlock::PlanChecklist(p) => {
                    lines.extend(render_plan_checklist(&p, width));
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ConversationBlock::Home(p) => {
                    lines.extend(render_home_card(&p, prose_width));
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ConversationBlock::TurnSummary(p) => {
                    // A turn used to end by simply stopping: the answer ran
                    // out and only the footer recorded that anything had
                    // concluded. This is the bottom edge, and the cost.
                    let mut spans = vec![
                        Span::styled("  ✓  ", theme::ok()),
                        Span::styled(
                            format!("Answered in {}", format_elapsed_tenths(p.secs)),
                            theme::text().add_modifier(Modifier::BOLD),
                        ),
                    ];
                    let mut detail = format!("   ·  {} chars", compact_count(p.chars));
                    if p.tools > 0 {
                        let unit = if p.tools == 1 { "tool" } else { "tools" };
                        detail.push_str(&format!("  ·  {} {unit}", p.tools));
                    }
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
                    let indent = INDENT_UNIT.repeat(2);
                    let content_width = width.saturating_sub(indent.chars().count());
                    let full_text = match p.duration_secs {
                        Some(secs) => format!("{} · {}", p.text, format_elapsed_tenths(secs)),
                        None => p.text.clone(),
                    };
                    for line in render_markdown(&full_text, content_width) {
                        let mut spans = vec![Span::styled(
                            indent.clone(),
                            theme::dim().add_modifier(Modifier::ITALIC),
                        )];
                        spans.extend(line.spans.into_iter().map(|mut span| {
                            span.style.fg = theme::dim().fg;
                            span.style = span.style.add_modifier(Modifier::ITALIC);
                            span
                        }));
                        lines.push(Line::from(spans));
                    }
                    // Deliberately no trailing blank: the tool call this
                    // reasoning produced should hug it, and the next major
                    // block opens with its own separator anyway.
                }
            }
        }
        lines
    }
}

#[cfg(test)]
pub struct ConversationWidget<'a> {
    pub model: &'a ConversationModel,
}

pub struct ConversationLinesWidget<'a> {
    pub lines: &'a [Line<'static>],
    pub tail_lines: &'a [Line<'static>],
    /// Rebuilt every frame and never cached: the live turn line animates, so
    /// caching it by content length would freeze it.
    pub status_lines: &'a [Line<'static>],
    pub scroll: u16,
    pub follow: bool,
    pub bottom_padding: u16,
    /// Hold the transcript against the composer once a conversation has
    /// started, instead of letting a short one float at the top of the pane
    /// with the live edge stranded mid-screen.
    pub anchor_bottom: bool,
    /// Stands in for the plan card once it has scrolled above the window.
    pub plan_dock: Option<&'a PlanDock>,
}

/// The three slices a transcript frame is painted from, in paint order:
/// settled history, the in-flight preview, and the live turn line.
#[derive(Clone, Copy)]
pub(super) struct TranscriptSlices<'a> {
    pub lines: &'a [Line<'static>],
    pub tail_lines: &'a [Line<'static>],
    pub status_lines: &'a [Line<'static>],
}

/// Locate the plan card inside `lines`, and build the row that stands in
/// for it.
///
/// The card is found by rendering it standalone and matching its first and
/// last rows against what was produced, rather than by threading an index
/// out of the block loop — the loop feeds three cached entry points and a
/// tail window, and an index would have to be kept correct through all of
/// them. Matching costs a scan of the rendered lines, and only when a plan
/// exists at all.
fn plan_dock_for(
    model: &ConversationModel,
    available_width: usize,
    keep_from_end: usize,
    lines: &[Line<'static>],
) -> Option<PlanDock> {
    let width = available_width.max(4);
    let blocks = model.semantic_blocks();
    let prose_width = prose_width_for(width);
    let start = start_block_for_tail(&blocks, width, prose_width, keep_from_end);
    let (index, plan) = blocks
        .iter()
        .enumerate()
        .rev()
        .find_map(|(i, block)| match block {
            ConversationBlock::PlanChecklist(p) => Some((i, p)),
            _ => None,
        })?;
    // Follow mode renders only a tail window, so a plan far enough back is
    // not in `lines` at all — which is exactly when it most needs docking.
    // Treat "not rendered" as "above the window", not as "no plan".
    let end = if index < start {
        0
    } else {
        let card = render_plan_checklist(plan, width);
        let located = card.first().zip(card.last()).and_then(|(first, last)| {
            let (first, last) = (line_plain(first), line_plain(last));
            let head = lines.iter().rposition(|line| line_plain(line) == first)?;
            lines[head..]
                .iter()
                .position(|line| line_plain(line) == last)
                .map(|offset| head + offset + 1)
        });
        located.unwrap_or(0)
    };

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
        Span::styled("\u{2191} ", theme::accent_style()),
        Span::styled("Plan  ", theme::metadata_style()),
        Span::styled(
            "\u{2593}".repeat(done) + &"\u{2591}".repeat(total.saturating_sub(done)),
            theme::accent_style(),
        ),
        Span::styled(format!("  {done}/{total}"), theme::muted()),
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
        summary: Line::from(spans),
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
    pub(super) summary: Line<'static>,
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
    buf: &mut Buffer,
) {
    theme::fill(area, buf, theme::assistant_message());
    let TranscriptSlices {
        lines,
        tail_lines,
        status_lines,
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
    let blank = Line::from("");
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
    render_visible_conversation_lines(&visible, area, buf);
}

pub(super) fn render_visible_conversation_lines(
    lines: &[&Line<'static>],
    area: Rect,
    buf: &mut Buffer,
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
                .border_style(theme::inactive_panel_border())
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
                (*line).render(Rect::new(inner.x, row, inner.width, 1), buf);
            }
            y = y.saturating_add(block_height);
            index = end.saturating_add(1);
        } else {
            lines[index].render(Rect::new(area.x, y, area.width, 1), buf);
            y = y.saturating_add(1);
            index += 1;
        }
    }
}

impl Widget for ConversationLinesWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        render_conversation_lines(
            TranscriptSlices {
                lines: self.lines,
                tail_lines: self.tail_lines,
                status_lines: self.status_lines,
            },
            self.scroll,
            self.follow,
            self.bottom_padding,
            self.anchor_bottom,
            self.plan_dock,
            area,
            buf,
        );
    }
}

#[cfg(test)]
impl Widget for ConversationWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // The transcript owns the main area; hierarchy comes from spacing and
        // semantic markers rather than a permanent frame.
        let inset_x = 2.min(area.width);
        let inset_y = 1.min(area.height);
        let area = Rect {
            x: area.x.saturating_add(inset_x),
            y: area.y.saturating_add(inset_y),
            width: area.width.saturating_sub(inset_x),
            height: area.height.saturating_sub(inset_y),
        };
        let lines = self.model.lines_for_width(area.width as usize);
        render_conversation_lines(
            TranscriptSlices {
                lines: &lines,
                tail_lines: &[],
                status_lines: &[],
            },
            self.model.scroll,
            self.model.follow,
            0,
            false,
            None,
            area,
            buf,
        );
    }
}

/// Detect language from file path, returning language name for syntax highlighting.
pub(super) fn lang_from_path(path: &str) -> Option<&'static str> {
    let path_lower = path.to_lowercase();
    let filename = path_lower
        .rsplit('/')
        .next()
        .unwrap_or(&path_lower)
        .rsplit('\\')
        .next()
        .unwrap_or(&path_lower);

    let ext = filename.rsplit('.').next()?;
    match ext {
        "rs" => Some("rust"),
        "ts" | "tsx" => Some("typescript"),
        "js" | "jsx" | "mjs" => Some("javascript"),
        "py" => Some("python"),
        "go" => Some("go"),
        "json" => Some("json"),
        "html" | "htm" => Some("html"),
        "css" => Some("css"),
        "sh" | "bash" | "zsh" => Some("bash"),
        "md" => Some("markdown"),
        "toml" | "yaml" | "yml" => Some("yaml"),
        "txt" | "log" => None,
        _ => None,
    }
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
    "Find the bugs in the file I have open",
    "Write tests for every public function",
];

/// Width of the label column on the home card.
const HOME_LABEL_WIDTH: usize = 11;

/// The first screen.
///
/// It used to be a version string, a clipped path and an orphaned
/// `· 20 skills`, then four hundred pixels of nothing — no model, no provider,
/// no connection state, and no suggestion of what to type. Every comparable CLI
/// puts at least the model here.
fn render_home_card(p: &HomePresentation, prose_width: usize) -> Vec<Line<'static>> {
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
    row(vec![]);
    row(field(
        "model",
        vec![Span::styled(p.model.clone(), theme::text())],
    ));
    row(field(
        "provider",
        vec![
            Span::styled(p.provider.clone(), theme::text()),
            Span::raw("  "),
            if p.connected {
                Span::styled("● connected", theme::ok())
            } else {
                Span::styled("● not connected", theme::warn())
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
    row(field(
        "skills",
        vec![Span::styled(
            format!("{} loaded", p.skills_loaded),
            theme::text(),
        )],
    ));
    row(vec![]);
    row(vec![Span::styled("Try one of these", theme::muted())]);
    for starter in HOME_STARTERS {
        row(vec![
            Span::styled("  → ", theme::accent_style()),
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

    for wrapped in wrap(&p.question, inner) {
        row(vec![Span::styled(wrapped, theme::text())]);
    }
    row(vec![]);

    for (idx, opt) in p.options.iter().enumerate() {
        let selected = idx == p.selected;
        // The marker is its own span. Folding it into the wrapped string let
        // `wrap` trim the leading space off every unselected row, so the
        // options sat two columns left of the one under the cursor and the
        // list did not read as a list.
        let marker = if selected { "\u{276f} " } else { "  " };
        let style = if selected {
            theme::text().add_modifier(Modifier::BOLD)
        } else {
            theme::text()
        };
        // The digit already answers the question — `handle_question_menu_key`
        // has accepted 1-9 all along, with nothing on screen to say so.
        let ordinal = format!("{}. ", idx + 1);
        let chosen = if opt.chosen { "● " } else { "" };
        let lead = marker.chars().count() + ordinal.chars().count() + chosen.chars().count();
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
            spans.push(Span::styled(
                if n == 0 {
                    ordinal.clone()
                } else {
                    " ".repeat(ordinal.chars().count())
                },
                theme::metadata_style().add_modifier(Modifier::BOLD),
            ));
            if !chosen.is_empty() {
                spans.push(Span::styled(
                    if n == 0 {
                        chosen.to_string()
                    } else {
                        " ".repeat(chosen.chars().count())
                    },
                    theme::ok(),
                ));
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
    let hint = if p.question_count > 1 {
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
    // Prose runs the width of the pane; a card does not follow it there.
    let prose_width = prose_width.min(CARD_MAX_WIDTH);
    let pad = " ".repeat(MESSAGE_PADDING);
    // A row spends `pad` + `│` + ` ` + inner + `│`, so the card is
    // `MESSAGE_PADDING + 3` columns wider than its content.
    let inner = prose_width.saturating_sub(MESSAGE_PADDING + 3).max(8);
    let compact = inner < APPROVAL_COMPACT_WIDTH;
    let border = if p.focused {
        theme::waiting_border()
    } else {
        theme::border_muted()
    };

    let mut out: Vec<Line<'static>> = Vec::new();
    let mut row = |spans: Vec<Span<'static>>| {
        let mut all = vec![
            Span::raw(pad.clone()),
            Span::styled("│", border),
            Span::raw(" "),
        ];
        // Clip, don't just pad. Content with no break opportunity — a path, a
        // long single-token command — comes back from `wrap` wider than asked
        // for, and without this it runs straight out through the right border.
        let mut used = 0usize;
        for span in spans {
            let w = span.width();
            if used + w <= inner {
                used += w;
                all.push(span);
            } else {
                let room = inner - used;
                if room > 0 {
                    let clipped: String = span.content.chars().take(room).collect();
                    used += clipped.chars().count();
                    all.push(Span::styled(clipped, span.style));
                }
                break;
            }
        }
        if used < inner {
            all.push(Span::raw(" ".repeat(inner - used)));
        }
        all.push(Span::styled("│", border));
        out.push(Line::from(all));
    };

    let question = p
        .question
        .as_deref()
        .unwrap_or_else(|| approval_question(&p.tool));
    row(vec![]);
    for wrapped in wrap(question, inner) {
        row(vec![Span::styled(wrapped, theme::text())]);
    }
    // What the sandbox actually said about this command. The category
    // explanation below reads identically for every command in that category,
    // so the evidence for this one leads.
    if let Some(failure) = p.failure.as_deref().filter(|f| !f.is_empty()) {
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
    let cwd_line = approval_location_line(
        &crate::path_display::elide_path(&p.cwd, inner.saturating_sub(3)),
        &p.env_delta,
    );
    for wrapped in wrap(&cwd_line, inner) {
        row(vec![Span::styled(wrapped, theme::muted())]);
    }
    row(vec![]);

    for (idx, opt) in p.options.iter().enumerate() {
        let selected = idx == p.selected;
        let (marker, style) = if selected {
            ("\u{276f} ", theme::text().add_modifier(Modifier::BOLD))
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
    row(vec![]);

    // A content row is `│ ` + inner + `│` = inner + 3 columns. The top border
    // spends `╭─ ` + title + ` ` + fill + `╮`, so the corners land on the same
    // columns as the walls.
    let title_fill = (inner + 3).saturating_sub(APPROVAL_TITLE.chars().count() + 5);
    let top = Line::from(vec![
        Span::raw(pad.clone()),
        Span::styled("╭─ ", border),
        Span::styled(APPROVAL_TITLE, border.add_modifier(Modifier::BOLD)),
        Span::styled(format!(" {}╮", "─".repeat(title_fill)), border),
    ]);
    let bottom = Line::from(vec![
        Span::raw(pad),
        Span::styled(format!("╰{}╯", "─".repeat(inner + 1)), border),
    ]);
    out.insert(0, top);
    out.push(bottom);
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
    width.saturating_sub(MESSAGE_PADDING * 2).max(4)
}

/// Widest a *card* may be drawn: the approval, question, home and plan cards.
///
/// Not a reading measure — those cards are mostly short labels and a command,
/// and a three-word command inside a two-hundred-column border reads as a
/// mistake. Prose has no ceiling (see `prose_width`); this is only about how
/// wide a box should be allowed to get around small content.
const CARD_MAX_WIDTH: usize = 80;

/// Subtle left rail grouping tool calls and progress under the current turn.
const RAIL_GLYPH: &str = "│";

/// Pane widths below this drop the rail and indent (flat mode).
const RAIL_MIN_WIDTH: usize = 50;

/// Columns the rail unit (`│ `) consumes from wrapped content.
const RAIL_EXTRA: usize = 2;

#[derive(Debug, PartialEq, Eq)]
struct NumberedDiffLine {
    old: Option<usize>,
    new: Option<usize>,
    marker: char,
    content: String,
    header: bool,
}

fn number_diff_lines(lines: &[String]) -> Vec<NumberedDiffLine> {
    let mut numbered = Vec::new();
    let mut old_line = None;
    let mut new_line = None;

    for line in lines {
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
        "  Ctrl+O to collapse"
    } else {
        "  Ctrl+O"
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

#[cfg(test)]
mod tests {
    use super::*;
    use forge_types::{ExecutionOutcome, Message, MessageRole, TaskLifecycle, ToolCall};
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Modifier;
    use ratatui::text::Line;
    use ratatui::Terminal;

    #[test]
    fn roles_map_to_items() {
        let msgs = vec![
            Message {
                outcome: Default::default(),
                role: MessageRole::System,
                content: "You are Forge, a coding agent. Use tools when needed.".into(),
                tool_call_id: None,
                name: None,
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
                attachments: Vec::new(),
            },
            Message {
                outcome: Default::default(),
                role: MessageRole::User,
                content: "hi".into(),
                tool_call_id: None,
                name: None,
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
                attachments: Vec::new(),
            },
            Message {
                outcome: Default::default(),
                role: MessageRole::Assistant,
                content: "yo".into(),
                tool_call_id: None,
                name: None,
                thinking: Some("**ponder**".into()),
                thinking_duration_secs: Some(2.4),
                tool_calls: vec![],
                attachments: Vec::new(),
            },
            Message {
                outcome: Default::default(),
                role: MessageRole::Tool,
                content: "ok body".into(),
                tool_call_id: Some("1".into()),
                name: Some("read_file".into()),
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
                attachments: Vec::new(),
            },
        ];
        let m = ConversationModel::from_messages(
            &msgs,
            &[],
            TaskLifecycle::Working,
            ConversationViewOpts::default(),
        );
        // System prompts stay hidden; completed reasoning remains visible before the answer.
        assert!(matches!(m.items[0], ChatItem::User { .. }));
        assert!(matches!(m.items[1], ChatItem::Thinking { .. }));
        assert!(matches!(m.items[2], ChatItem::Assistant { .. }));
        // Intent change: routine tool cards group from the *first* card now
        // (see `flush_activity_group`), so a lone `read_file` is an
        // ActivityGroup rather than a standalone ToolCard. Grouping at the
        // semantic layer was always true — the `semantic_blocks` assertion
        // below predates this — and what moved is the item layer, which is
        // what stops the block shrinking when a sibling completes.
        assert!(m
            .items
            .iter()
            .any(|i| matches!(i, ChatItem::ActivityGroup { .. })));
        // Full system prompt must not appear in rendered lines
        let rendered: String = m
            .lines()
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !rendered.contains("You are Forge, a coding agent"),
            "system prompt leaked into UI:\n{rendered}"
        );
        assert!(
            !rendered.contains("FORGE"),
            "brand splash removed from chat:\n{rendered}"
        );
    }

    /// The welcome pane's workspace line is budgeted one row, so a long path
    /// must elide to fit rather than wrap or run off the pane edge.
    #[test]
    fn the_home_workspace_line_elides_to_one_row() {
        let long =
            "/private/tmp/claude-501/-Users-someone-Projects-forge/ac5a5dcf-403d/scratchpad/lab";
        let m = ConversationModel::from_messages(
            &[],
            &[],
            TaskLifecycle::Ready,
            ConversationViewOpts::default(),
        )
        .with_home(
            long.to_string(),
            20,
            "gpt-5.6-sol".into(),
            "OpenAI".into(),
            true,
        );

        let rendered: Vec<String> = m
            .lines_for_width(60)
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        let workspace: Vec<&String> = rendered
            .iter()
            .filter(|l| l.contains("workspace"))
            .collect();
        assert_eq!(workspace.len(), 1, "must stay on one row: {rendered:?}");
        let line = workspace[0];
        assert!(line.contains('\u{2026}'), "expected an elision: {line}");
        assert!(line.contains("lab"), "workspace name must survive: {line}");
        assert!(
            !line.contains("ac5a5dcf-403d/scratchpad"),
            "the middle should be the part that goes: {line}"
        );
    }

    /// The first screen must name the model, the provider and its connection
    /// state, and offer something to type. It used to carry none of them.
    #[test]
    fn the_home_card_names_the_model_and_offers_a_way_in() {
        let m = ConversationModel::from_messages(
            &[],
            &[],
            TaskLifecycle::Ready,
            ConversationViewOpts::default(),
        )
        .with_home(
            "~/demo".into(),
            20,
            "gpt-5.6-sol".into(),
            "OpenAI".into(),
            true,
        );
        let text: String = m
            .lines_for_width(80)
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(text.contains("FORGE"), "{text}");
        assert!(text.contains("gpt-5.6-sol"), "{text}");
        assert!(text.contains("OpenAI"), "{text}");
        assert!(text.contains("connected"), "{text}");
        assert!(text.contains("~/demo"), "{text}");
        assert!(text.contains("20 loaded"), "{text}");
        assert!(text.contains(HOME_STARTERS[0]), "{text}");
    }

    #[test]
    fn expansion_state_does_not_mutate_transcript_data() {
        let collapsed = ConversationModel {
            items: vec![ChatItem::ToolCard {
                name: "bash".into(),
                summary: "$ cargo test · failed".into(),
                detail: "status 101".into(),
                state: ToolCardState::Error,
                duration: None,
                subcommand: None,
                outcome: forge_types::ExecutionOutcome::Failed { exit_code: None },
            }],
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts::default(),
        };
        let expanded = ConversationModel {
            opts: ConversationViewOpts {
                tool_expanded: true,
                ..Default::default()
            },
            ..collapsed.clone()
        };

        assert_eq!(collapsed.items, expanded.items);
        assert_ne!(collapsed.lines(), expanded.lines());
    }

    #[test]
    fn completed_thinking_remains_visible_in_lines() {
        let msgs = vec![Message {
            outcome: Default::default(),
            role: MessageRole::Assistant,
            content: "ans".into(),
            tool_call_id: None,
            name: None,
            thinking: Some("long thinking text here that should collapse".into()),
            thinking_duration_secs: Some(3.1),
            tool_calls: vec![],
            attachments: Vec::new(),
        }];
        let m = ConversationModel::from_messages(
            &msgs,
            &[],
            TaskLifecycle::Working,
            ConversationViewOpts {
                ..Default::default()
            },
        );
        let text: String = m
            .lines()
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("long thinking"),
            "completed thinking body should remain visible, got:\n{text}"
        );
    }

    #[test]
    fn thinking_renders_markdown_emphasis_without_literal_delimiters() {
        let model = ConversationModel {
            items: vec![ChatItem::Thinking {
                text: "**Inspecting** the parser".into(),
                duration_secs: None,
            }],
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts::default(),
        };

        let lines = model.lines_for_width(80);
        let text = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(text.contains("Inspecting the parser"), "got {text:?}");
        assert!(!text.contains("**"), "markdown delimiters leaked: {text:?}");

        let emphasis = lines
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content.contains("Inspecting"))
            .expect("emphasized thinking span present");
        assert!(emphasis.style.add_modifier.contains(Modifier::BOLD));
        assert!(emphasis.style.add_modifier.contains(Modifier::ITALIC));
        assert_eq!(
            emphasis.style.fg,
            Some(theme::palette(forge_config::DEFAULT_THEME_ID).dim)
        );
    }

    #[test]
    fn thinking_headings_on_separate_paragraphs_render_without_asterisks() {
        let model = ConversationModel {
            items: vec![ChatItem::Thinking {
                text: "**Designing SessionTemp temporary directory management**\n\n**Planning safe session temp directory creation**".into(),
                duration_secs: None,
            }],
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts::default(),
        };

        let lines = model.lines_for_width(80);
        let text = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(!text.contains('*'), "heading markers leaked: {text:?}");
        let designing = lines
            .iter()
            .position(|line| line_text(line).contains("Designing SessionTemp"))
            .expect("first heading");
        let planning = lines
            .iter()
            .position(|line| line_text(line).contains("Planning safe session"))
            .expect("second heading");
        assert!(
            planning > designing,
            "headings should be on separate lines, got:\n{text}"
        );
    }

    #[test]
    fn thinking_renders_dim_italic_indented_and_precedes_the_answer() {
        let msgs = vec![Message {
            outcome: Default::default(),
            role: MessageRole::Assistant,
            content: "final answer".into(),
            tool_call_id: None,
            name: None,
            thinking: Some("reasoning text".into()),
            thinking_duration_secs: Some(2.4),
            tool_calls: vec![],
            attachments: Vec::new(),
        }];
        let m = ConversationModel::from_messages(
            &msgs,
            &[],
            TaskLifecycle::Ready,
            ConversationViewOpts::default(),
        );
        let lines = m.lines_for_width(80);

        let thinking_idx = lines
            .iter()
            .position(|l| line_text(l).contains("reasoning text"))
            .expect("thinking line present");
        let answer_idx = lines
            .iter()
            .position(|l| line_text(l).contains("final answer"))
            .expect("answer line present");
        assert!(
            thinking_idx < answer_idx,
            "thinking must render before the answer, got thinking@{thinking_idx} answer@{answer_idx}: {lines:?}"
        );

        let thinking_line = &lines[thinking_idx];
        let thinking_text = line_text(thinking_line);
        assert!(
            thinking_text.starts_with(&INDENT_UNIT.repeat(2)),
            "thinking should be indented past normal content, got {thinking_text:?}"
        );
        let dark = theme::palette(forge_config::DEFAULT_THEME_ID);
        let thinking_spans = thinking_line
            .spans
            .iter()
            .filter(|span| !span.content.trim().is_empty())
            .collect::<Vec<_>>();
        assert!(
            thinking_spans
                .iter()
                .all(|span| span.style.fg == Some(dark.dim)),
            "thinking should use the dim token"
        );
        assert!(
            thinking_spans
                .iter()
                .all(|span| span.style.add_modifier.contains(Modifier::ITALIC)),
            "thinking should be italic"
        );
        assert!(
            thinking_spans
                .iter()
                .all(|span| !span.style.add_modifier.contains(Modifier::BOLD)),
            "thinking should not be bold — no label, unlike tool activity"
        );
        assert!(
            thinking_text.contains("2.4s"),
            "duration should still be shown, got {thinking_text:?}"
        );
    }

    /// The reply used to start on a different left edge from the answer it
    /// introduces: 4 columns for the collapsed reasoning, 2 for everything
    /// under it.
    #[test]
    fn collapsed_reasoning_shares_the_answer_left_edge() {
        // Only spent reasoning collapses — the newest block stays expanded —
        // so the turn needs a later step for the first one to fold.
        let think = |content: &str, thinking: &str| Message {
            outcome: Default::default(),
            role: MessageRole::Assistant,
            content: content.into(),
            tool_call_id: None,
            name: None,
            thinking: Some(thinking.into()),
            thinking_duration_secs: Some(185.0),
            tool_calls: vec![],
            attachments: Vec::new(),
        };
        let msgs = vec![
            think("The answer itself.", "some private reasoning"),
            think("And a later step.", "more reasoning"),
        ];
        let model = ConversationModel::from_messages(
            &msgs,
            &[],
            TaskLifecycle::Completed,
            ConversationViewOpts::default(),
        );

        let rendered: Vec<String> = model.lines_for_width(100).iter().map(line_text).collect();
        let edge = |needle: &str| {
            rendered
                .iter()
                .find(|row| row.contains(needle))
                .map(|row| row.len() - row.trim_start().len())
                .unwrap_or_else(|| panic!("{needle:?} missing from {rendered:#?}"))
        };

        assert_eq!(
            edge("Thought for"),
            edge("The answer itself."),
            "reasoning and answer should start in the same column: {rendered:#?}"
        );
    }

    /// A wide pane is used. 119 characters wrapped to two lines under the old
    /// 72-column measure while two thirds of the pane sat empty.
    #[test]
    fn a_wide_viewport_gives_its_width_to_the_answer() {
        let content = std::iter::repeat_n("word", 24)
            .collect::<Vec<_>>()
            .join(" ");
        let msgs = vec![Message {
            outcome: Default::default(),
            role: MessageRole::Assistant,
            content,
            tool_call_id: None,
            name: None,
            thinking: None,
            thinking_duration_secs: None,
            tool_calls: vec![],
            attachments: Vec::new(),
        }];
        let model = ConversationModel::from_messages(
            &msgs,
            &[],
            TaskLifecycle::Working,
            ConversationViewOpts::default(),
        );

        let answer_lines = model
            .lines_for_width(140)
            .iter()
            .filter(|line| line.spans.iter().any(|span| span.content.contains("word")))
            .count();
        assert_eq!(answer_lines, 1);

        // Narrow panes are unaffected: there the pane, not a measure, was
        // always the limit.
        let narrow = model
            .lines_for_width(48)
            .iter()
            .filter(|line| line.spans.iter().any(|span| span.content.contains("word")))
            .count();
        assert!(narrow >= 3, "a 48-column pane should still wrap: {narrow}");
    }

    #[test]
    fn active_thinking_renders_above_empty_answer() {
        let msgs = vec![Message {
            outcome: Default::default(),
            role: MessageRole::Assistant,
            content: String::new(),
            tool_call_id: None,
            name: None,
            thinking: Some(
                "one two three four five six seven eight nine ten eleven twelve thirteen".into(),
            ),
            thinking_duration_secs: None,
            tool_calls: vec![],
            attachments: Vec::new(),
        }];
        let model = ConversationModel::from_messages(
            &msgs,
            &[],
            TaskLifecycle::Working,
            ConversationViewOpts::default(),
        );

        let rendered_text = rendered_text(&model);
        assert!(
            rendered_text.contains("one two three"),
            "active reasoning should appear at the top of the turn: {rendered_text}"
        );
        assert!(matches!(
            model.items.first(),
            Some(ChatItem::Thinking { .. })
        ));
    }

    #[test]
    fn assistant_output_stays_visible_below_thinking() {
        let msgs = vec![Message {
            outcome: Default::default(),
            role: MessageRole::Assistant,
            content: "ans".into(),
            tool_call_id: None,
            name: None,
            thinking: Some("this is a very long active thinking message that should wrap into multiple lines in the conversation pane".into()),
            thinking_duration_secs: None,
            tool_calls: vec![],
            attachments: Vec::new(),
        }];
        let m = ConversationModel::from_messages(
            &msgs,
            &[],
            TaskLifecycle::Working,
            ConversationViewOpts::default(),
        );
        let rendered_lines: Vec<String> = m
            .lines()
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        let thinking_idx = rendered_lines
            .iter()
            .position(|line| line.contains("very long active thinking"))
            .expect("thinking visible");
        let answer_idx = rendered_lines
            .iter()
            .position(|line| line.contains("ans"))
            .expect("answer visible");
        assert!(
            thinking_idx < answer_idx,
            "thinking must appear above the answer, got:\n{}",
            rendered_lines.join("\n")
        );
    }

    #[test]
    fn user_messages_render_left_aligned_with_indent() {
        const WIDTH: usize = 100;
        let msgs = vec![Message {
            outcome: Default::default(),
            role: MessageRole::User,
            content: "hello world".into(),
            tool_call_id: None,
            name: None,
            thinking: None,
            thinking_duration_secs: None,
            tool_calls: vec![],
            attachments: Vec::new(),
        }];
        let m = ConversationModel::from_messages(
            &msgs,
            &[],
            TaskLifecycle::Working,
            ConversationViewOpts::default(),
        );
        let lines = m.lines_for_width(WIDTH);
        let rendered_lines = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        let rendered = rendered_lines.join("\n");
        assert_eq!(rendered_lines[0].trim_end(), "  hello world", "{rendered}");
        let dark = theme::palette(forge_config::DEFAULT_THEME_ID);
        let first = &lines[0];
        // No leading marker — a plain indent, background carried to the edge.
        assert_eq!(first.spans[0].content.as_ref(), "  ");
        assert_eq!(first.spans[0].style.bg, Some(dark.accent_soft));
        assert_eq!(first.spans[1].content.as_ref(), "hello world");
        assert_eq!(first.spans[1].style.fg, Some(dark.text));
        assert_eq!(first.spans[1].style.bg, Some(dark.accent_soft));
        assert!(!rendered.contains('|'), "{rendered}");
        assert!(!rendered.contains('›'), "{rendered}");
        assert!(!rendered.contains(" │"), "{rendered}");
        assert!(rendered.contains("hello world"), "{rendered}");
    }

    #[test]
    fn wrapped_user_message_keeps_indent_and_background_on_every_row() {
        const WIDTH: usize = 20;
        let msgs = vec![Message {
            outcome: Default::default(),
            role: MessageRole::User,
            content: "one two three four five six seven".into(),
            tool_call_id: None,
            name: None,
            thinking: None,
            thinking_duration_secs: None,
            tool_calls: vec![],
            attachments: Vec::new(),
        }];
        let m = ConversationModel::from_messages(
            &msgs,
            &[],
            TaskLifecycle::Working,
            ConversationViewOpts::default(),
        );
        let lines = m.lines_for_width(WIDTH);
        let dark = theme::palette(forge_config::DEFAULT_THEME_ID);
        let user_rows: Vec<&Line<'static>> = lines
            .iter()
            .take_while(|line| {
                line.spans
                    .first()
                    .is_some_and(|s| s.style.bg == Some(dark.accent_soft))
            })
            .collect();
        assert!(
            user_rows.len() > 1,
            "message should wrap to more than one row at width {WIDTH}: {lines:?}"
        );
        for row in &user_rows {
            assert_eq!(row.spans[0].content.as_ref(), "  ");
            assert_eq!(row.spans[0].style.bg, Some(dark.accent_soft));
        }
    }

    #[test]
    fn empty_transcript_renders_without_initial_marker() {
        let model = ConversationModel::from_messages(
            &[],
            &[],
            TaskLifecycle::Working,
            ConversationViewOpts::default(),
        );
        let rendered = model
            .lines()
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.is_empty(), "{rendered}");
        assert!(!rendered.contains('▸'), "{rendered}");
    }

    #[test]
    fn long_assistant_responses_use_no_repeated_product_heading() {
        let msgs = vec![Message {
            outcome: Default::default(),
            role: MessageRole::Assistant,
            content: "line one\nline two\nline three\nline four".into(),
            tool_call_id: None,
            name: None,
            thinking: None,
            thinking_duration_secs: None,
            tool_calls: vec![],
            attachments: Vec::new(),
        }];
        let model = ConversationModel::from_messages(
            &msgs,
            &[],
            TaskLifecycle::Working,
            ConversationViewOpts::default(),
        );
        let rendered = model
            .lines()
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert!(!rendered.iter().any(|line| line.as_str() == "Forge"));
        assert_eq!(rendered.iter().filter(|line| line.contains('─')).count(), 0);
    }

    #[test]
    fn builtin_tool_outputs_are_compact_until_expanded() {
        let calls = [
            (
                "read",
                "read_file",
                serde_json::json!({"path": "src/lib.rs"}),
            ),
            (
                "bash",
                "bash",
                serde_json::json!({"command": "cargo test --quiet"}),
            ),
            ("find", "glob", serde_json::json!({"query": "*.rs"})),
            ("grep", "grep", serde_json::json!({"pattern": "ToolCard"})),
            (
                "git",
                "git",
                serde_json::json!({"subcommand": "status", "args": ["--short"]}),
            ),
            (
                "web",
                "web_search",
                serde_json::json!({"query": "ratatui diff rendering"}),
            ),
        ]
        .map(|(id, name, arguments)| ToolCall {
            id: id.into(),
            name: name.into(),
            arguments,
        });
        let mut messages = vec![Message {
            outcome: Default::default(),
            role: MessageRole::Assistant,
            content: String::new(),
            tool_call_id: None,
            name: None,
            thinking: None,
            thinking_duration_secs: None,
            tool_calls: calls.to_vec(),
            attachments: Vec::new(),
        }];
        let outputs = [
            ("read", "read_file", "pub fn noisy() {\n- old\n+ new\n}"),
            ("bash", "bash", "running tests\nfeature-a\n+ experimental"),
            ("find", "glob", "src/lib.rs\nsrc/main.rs"),
            (
                "grep",
                "grep",
                "src/lib.rs:10:ToolCard\nsrc/app.rs:20:ToolCard",
            ),
            ("git", "git", " M src/lib.rs\n M src/app.rs"),
            (
                "web",
                "web_search",
                "## Web search: ratatui diff rendering\n\n1. **Ratatui**\n   - URL: https://ratatui.rs\n   - Snippet: Widgets\n\n```json\n[]\n```",
            ),
        ];
        let output_count = outputs.len();
        messages.extend(outputs.map(|(id, name, content)| Message {
            outcome: Default::default(),
            role: MessageRole::Tool,
            content: content.into(),
            tool_call_id: Some(id.into()),
            name: Some(name.into()),
            thinking: None,
            thinking_duration_secs: None,
            tool_calls: vec![],
            attachments: Vec::new(),
        }));

        let model = ConversationModel::from_messages(
            &messages,
            &[],
            TaskLifecycle::Working,
            ConversationViewOpts::default(),
        );
        assert_eq!(output_count, outputs.len());
        assert!(model.items.iter().any(|item| matches!(
            item,
            ChatItem::ActivityGroup {
                category: ActivityCategory::Exploring,
                ..
            }
        )));
        assert!(
            !model
                .items
                .iter()
                .any(|item| matches!(item, ChatItem::DiffCard { .. })),
            "ordinary multiline output must not be classified as a diff"
        );

        let blocks = model.semantic_blocks();
        assert!(blocks.iter().any(|block| matches!(
            block,
            ConversationBlock::ActivityGroup(group)
                if !group.items.is_empty() && !group.label.is_empty()
        )));

        let expanded = ConversationModel::from_messages(
            &messages,
            &[],
            TaskLifecycle::Working,
            ConversationViewOpts {
                tool_expanded: true,
                ..ConversationViewOpts::default()
            },
        )
        .semantic_blocks();
        assert!(expanded.iter().any(|block| matches!(
            block,
            ConversationBlock::ActivityGroup(group)
                if group.items.iter().any(|item| item.contains("https://ratatui.rs"))
        )));
    }

    #[test]
    fn failed_validation_command_does_not_render_as_validation_completed() {
        let model = ConversationModel {
            items: vec![ChatItem::ToolCard {
                name: "bash".into(),
                summary: "$ cargo test · failed · exit code 101".into(),
                detail: "status 101".into(),
                state: ToolCardState::Error,
                duration: None,
                subcommand: None,
                outcome: ExecutionOutcome::Failed {
                    exit_code: Some(101),
                },
            }],
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts::default(),
        };
        let rendered = rendered_text(&model);
        assert!(
            !rendered.contains("Validation completed"),
            "a failed validation command must never render as completed:\n{rendered}"
        );
        assert!(
            rendered.contains("Tests failed") || rendered.contains("failed"),
            "the failure must be visible in the rendered transcript:\n{rendered}"
        );
    }

    #[test]
    fn activity_outcome_icon_matrix() {
        let cases = [
            (ActivityOutcome::Success, false),
            (ActivityOutcome::Failure, false),
            (ActivityOutcome::Blocked, false),
            (ActivityOutcome::Warning, false),
            (ActivityOutcome::Neutral, false),
            (ActivityOutcome::Denied, false),
            (ActivityOutcome::Cancelled, false),
            (ActivityOutcome::TimedOut, false),
        ];
        for (outcome, _) in cases {
            let model = ConversationModel {
                items: vec![ChatItem::ActivityGroup {
                    category: ActivityCategory::Validating,
                    summary: "summary".into(),
                    detail: "detail".into(),
                    state: ToolCardState::Done,
                    outcome: match outcome {
                        ActivityOutcome::Denied => ExecutionOutcome::Denied {
                            reason: "denied".into(),
                        },
                        ActivityOutcome::Cancelled => ExecutionOutcome::Cancelled,
                        ActivityOutcome::TimedOut => ExecutionOutcome::TimedOut,
                        ActivityOutcome::Failure => ExecutionOutcome::Failed { exit_code: None },
                        _ => ExecutionOutcome::Success,
                    },
                }],
                scroll: 0,
                follow: true,
                opts: ConversationViewOpts::default(),
            };
            // Rendering must not panic for any outcome variant.
            let _ = rendered_text(&model);
        }
    }

    #[test]
    fn inline_code_in_body_text_is_tinted_not_given_an_interactive_accent() {
        let lines = render_markdown("plain text with `inline code` in it", 80);
        let code_span = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .find(|span| span.content.as_ref() == "inline")
            .expect("inline code token present");
        // Code is marked by its ground, not by a hue: the accent means focus
        // and info means a callout, so neither may leak into body content.
        assert_eq!(code_span.style.bg, theme::inline_code().bg);
        assert_ne!(code_span.style.fg, Some(theme::accent_color()));
        assert_ne!(code_span.style.fg, Some(theme::info_color()));
    }

    #[test]
    fn long_command_subcommand_wraps_with_connector() {
        let long_command = "cargo build --workspace --all-features --jobs 8 && cargo doc --no-deps; ls -la; git status --short";
        let model = bash_tool_card(long_command, "5", false);
        let text = rendered_text(&model);

        assert!(text.contains("└─ $ cargo build"), "{text}");
        // A command too wide for the pane wraps with the connector carried
        // down, so the full invocation stays visible instead of truncating.
        assert!(text.contains("--short"), "{text}");
        assert!(
            text.contains("│"),
            "wrapped continuation must carry the connector:\n{text}"
        );
        assert!(text.contains("5 output lines"), "{text}");
        assert!(text.contains("Ctrl+O"), "{text}");
    }

    #[test]
    fn long_command_entry_expands_full_text_via_ctrl_o() {
        let long_command = "cargo test --workspace --all-features -- --test-threads=1 --nocapture; git diff --check; git status --short";
        let model = bash_tool_card(long_command, "5", true);
        let text = rendered_text(&model);

        // Wrapping the expanded detail can reflow incidental whitespace, so
        // check for the command's distinct pieces rather than byte-exact
        // equality with the original string.
        assert!(text.contains("--test-threads=1 --nocapture"), "{text}");
        assert!(text.contains("git status --short"), "{text}");
        assert!(text.contains("Ctrl+O to collapse"), "{text}");
    }

    #[test]
    fn short_command_entries_are_unaffected() {
        let short_command = "cargo test -p forge-tui";
        let model = bash_tool_card(short_command, "3", false);
        let text = rendered_text(&model);

        assert!(text.contains(&format!("$ {short_command}")), "{text}");
        assert!(!text.contains('…'), "{text}");
    }

    #[test]
    fn tool_card_with_subcommand_renders_connector_line() {
        let model = ConversationModel {
            items: vec![ChatItem::ToolCard {
                name: "git".into(),
                summary: "git status --short · 12 output lines".into(),
                detail: " M crates/forge-tui/src/conversation.rs".into(),
                state: ToolCardState::Done,
                duration: None,
                subcommand: Some("git status --short".into()),
                outcome: forge_types::ExecutionOutcome::Success,
            }],
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts::default(),
        };
        let text = rendered_text(&model);

        assert!(
            text.contains("└─ git status --short · 12 output lines"),
            "invocation must render on its own connector line:\n{text}"
        );
        assert!(
            !text.contains("1 item"),
            "count label must move off the label line:\n{text}"
        );
    }

    #[test]
    fn tool_card_without_subcommand_keeps_single_line() {
        // Render-only tools (apply_patch, MCP) keep the old single-line form.
        let model = ConversationModel {
            items: vec![ChatItem::ToolCard {
                name: "apply_patch".into(),
                summary: "patch applies cleanly".into(),
                detail: "*** Begin Patch".into(),
                state: ToolCardState::Done,
                duration: None,
                subcommand: None,
                outcome: forge_types::ExecutionOutcome::Success,
            }],
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts::default(),
        };
        let text = rendered_text(&model);
        assert!(!text.contains('└'), "{text}");
        assert!(text.contains("Implemented changes"), "{text}");
        assert!(
            !text.contains("patch applies cleanly"),
            "summary stays expanded-only:\n{text}"
        );
    }

    #[test]
    fn write_tool_subcommand_uses_path_when_summary_is_output_preview() {
        let model = ConversationModel {
            items: vec![ChatItem::ToolCard {
                name: "write_file".into(),
                summary: "wrote · +1 -0 src/foo.rs".into(),
                detail: "wrote src/foo.rs".into(),
                state: ToolCardState::Done,
                duration: None,
                subcommand: Some("src/foo.rs".into()),
                outcome: forge_types::ExecutionOutcome::Success,
            }],
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts::default(),
        };
        let text = rendered_text(&model);
        assert!(
            text.contains("└─ src/foo.rs"),
            "write tools should surface the path, not the output preview:\n{text}"
        );
    }

    #[test]
    fn grouped_activity_renders_running_completed_and_details() {
        let running = ConversationModel::from_messages(
            &[],
            &[],
            TaskLifecycle::Working,
            ConversationViewOpts::default(),
        )
        .with_running_tool("read_file");
        let running_text = rendered_text(&running);
        assert!(running_text.contains("● Exploring repository"));
        assert!(running_text.contains("Reading via read_file"));

        let completed = ConversationModel {
            items: group_routine_activity(vec![
                ChatItem::ToolCard {
                    name: "write_file".into(),
                    summary: "wrote · src/lib.rs".into(),
                    detail: "src/lib.rs".into(),
                    state: ToolCardState::Done,
                    duration: None,
                    subcommand: None,
                    outcome: forge_types::ExecutionOutcome::Success,
                },
                ChatItem::ToolCard {
                    name: "apply_patch".into(),
                    summary: "wrote · src/app.rs".into(),
                    detail: "src/app.rs".into(),
                    state: ToolCardState::Done,
                    duration: None,
                    subcommand: None,
                    outcome: forge_types::ExecutionOutcome::Success,
                },
            ]),
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts {
                tool_expanded: true,
                ..Default::default()
            },
        };
        let blocks = completed.semantic_blocks();
        assert!(blocks.iter().any(|block| matches!(
            block,
            ConversationBlock::ActivityGroup(group)
                if !group.items.is_empty() && group.count_label.contains("2")
        )));
        assert_eq!(activity_detail_label(true), "  Ctrl+O to collapse");
        assert_eq!(activity_detail_label(false), "  Ctrl+O");
    }

    #[test]
    fn update_plan_tool_messages_render_as_checklist() {
        let msgs = vec![
            Message {
                outcome: Default::default(),
                role: MessageRole::Assistant,
                content: String::new(),
                tool_call_id: None,
                name: None,
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![ToolCall {
                    id: "plan-1".into(),
                    name: "update_plan".into(),
                    arguments: serde_json::json!({
                        "explanation": "Next steps",
                        "plan": [
                            {"step": "Inspect code", "status": "completed"},
                            {"step": "Implement tool", "status": "in_progress"},
                            {"step": "Add tests", "status": "pending"}
                        ]
                    }),
                }],
                attachments: Vec::new(),
            },
            Message {
                outcome: Default::default(),
                role: MessageRole::Tool,
                content: "Plan updated".into(),
                tool_call_id: Some("plan-1".into()),
                name: Some("update_plan".into()),
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
                attachments: Vec::new(),
            },
        ];
        let m = ConversationModel::from_messages(
            &msgs,
            &[],
            TaskLifecycle::Ready,
            ConversationViewOpts::default(),
        );
        assert!(
            matches!(
                &m.items[..],
                [ChatItem::PlanChecklist {
                    explanation: Some(exp),
                    steps,
                    ..
                }] if exp == "Next steps" && steps.len() == 3
            ),
            "expected plan checklist item, got {:?}",
            m.items
        );
        let text = m
            .lines_for_width(80)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        // No spelled-out "Plan" caption by design — the bordered card and
        // [✓]/[►]/[ ] checkboxes carry the meaning instead.
        assert!(text.contains("Next steps"), "{text}");
        assert!(text.contains("Inspect code"), "{text}");
        assert!(text.contains("Implement tool"), "{text}");
        assert!(text.contains("Add tests"), "{text}");
        assert!(matches!(
            m.semantic_blocks().as_slice(),
            [ConversationBlock::PlanChecklist(_)]
        ));
    }

    /// Measured before this existed: the plan card was on screen for 8 frames
    /// out of 70, and after that nothing said a plan existed at all.
    #[test]
    fn the_plan_docks_once_its_card_has_scrolled_away() {
        let mut items = vec![ChatItem::PlanChecklist {
            explanation: None,
            steps: vec![
                forge_types::PlanItem {
                    step: "Inspect the theme tokens".into(),
                    status: forge_types::PlanStepStatus::Completed,
                },
                forge_types::PlanItem {
                    step: "Add the dark palette".into(),
                    status: forge_types::PlanStepStatus::InProgress,
                },
            ],
            evidence: Vec::new(),
        }];
        // Enough answer under it to push the card off any reasonable pane.
        for n in 0..40 {
            items.push(ChatItem::Assistant {
                text: format!("line {n}"),
            });
        }
        let model = ConversationModel {
            items,
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts::default(),
        };

        let (lines, dock) = model.lines_and_plan_dock(80, usize::MAX);
        let dock = dock.expect("a plan in the transcript should be dockable");

        // The dock stands for a card that really is up there.
        assert!(dock.end <= lines.len(), "{} vs {}", dock.end, lines.len());
        let summary = line_text(&dock.summary);
        assert!(summary.contains("1/2"), "{summary}");
        assert!(
            summary.contains("Add the dark palette"),
            "the running step should be named: {summary}"
        );

        // Scrolled past the card, the top row is the summary. Still on it,
        // the card speaks for itself and the row is left alone.
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 10));
        render_conversation_lines(
            TranscriptSlices {
                lines: &lines,
                tail_lines: &[],
                status_lines: &[],
            },
            0,
            true,
            0,
            false,
            Some(&dock),
            Rect::new(0, 0, 80, 10),
            &mut buf,
        );
        let top: String = (0..80)
            .map(|x| buf[(x, 0)].symbol().to_string())
            .collect::<String>();
        assert!(
            top.contains("Plan") && top.contains("1/2"),
            "docked row missing from the top of the pane: {top:?}"
        );
    }

    #[test]
    fn plan_checklist_card_is_bordered_with_no_background_fill() {
        let plan = PlanChecklistPresentation {
            explanation: Some("Next steps".into()),
            steps: vec![forge_types::PlanItem {
                step: "Inspect code".into(),
                status: forge_types::PlanStepStatus::Completed,
            }],
            evidence: Vec::new(),
        };
        let lines = render_plan_checklist(&plan, 80);
        // The explanation ("Next steps") renders as an unboxed intro line
        // above the card — only the step list itself is bordered.
        let top = lines
            .iter()
            .find(|l| line_text(l).starts_with('┌'))
            .expect("top border present");
        assert!(
            !line_text(top).contains("Plan"),
            "no spelled-out caption in the border, got {:?}",
            line_text(top)
        );
        assert!(
            lines
                .iter()
                .rev()
                .find(|l| !line_text(l).is_empty())
                .is_some_and(|l| line_text(l).starts_with('└')),
            "plan card should close with a bottom border"
        );
        let content_row = lines
            .iter()
            .find(|l| line_text(l).contains("Inspect code"))
            .expect("step content row present");
        for span in &content_row.spans {
            assert_eq!(
                span.style.bg, None,
                "plan card content must have no background fill — canvas shows through, got {span:?}"
            );
        }
    }

    #[test]
    fn diff_rows_have_backgrounds_and_syntax_highlighting() {
        let diff = ["@@ -1 +1 @@", "-fn old() {}", "+fn new() {}"].map(str::to_string);

        let rendered = render_numbered_diff("src/lib.rs", &diff, 40);
        let removed = &rendered[1];
        let added = &rendered[2];

        let dark = theme::palette(forge_config::DEFAULT_THEME_ID);
        assert!(removed
            .spans
            .iter()
            .all(|span| span.style.bg == Some(dark.diff_remove)));
        assert!(added
            .spans
            .iter()
            .all(|span| span.style.bg == Some(dark.diff_add)));
        assert!(
            added.spans.len() > 3,
            "Rust tokens should be separate spans"
        );
        assert_eq!(
            added.spans.iter().map(|span| span.width()).sum::<usize>(),
            40
        );
    }

    #[test]
    fn diff_pane_borders_multi_hunk_content_without_overflowing() {
        let diff = [
            "@@ -1 +1 @@",
            "-old",
            "+new",
            "@@ -10 +10 @@",
            "-before",
            "+after",
            "@@ -20 +20 @@",
            "+this line is deliberately longer than one hundred characters so a narrow diff pane clips it instead of breaking its layout",
        ]
        .map(str::to_string);
        let lines = vec![diff_title_line("src/lib.rs", &diff)]
            .into_iter()
            .chain(render_numbered_diff("src/lib.rs", &diff, 38))
            .chain(std::iter::once(Line::from(DIFF_BLOCK_END_MARKER)))
            .collect::<Vec<_>>();
        let area = Rect::new(0, 0, 40, 12);
        let mut buf = Buffer::empty(area);

        let lines = lines.iter().collect::<Vec<_>>();
        render_visible_conversation_lines(&lines, area, &mut buf);

        assert_eq!(buf[(0, 0)].symbol(), "┌");
        assert_eq!(buf[(39, 0)].symbol(), "┐");
        assert!(buf[(2, 0)].symbol().contains("s"));
        assert_eq!(buf[(0, 10)].symbol(), "└");
        assert_eq!(buf[(39, 10)].symbol(), "┘");
    }

    #[test]
    fn diff_hunk_headers_align_with_file_header() {
        let diff = ["@@ -1 +1 @@", "-old", "+new"].map(str::to_string);
        let title = lines_text(&[diff_title_line("src/lib.rs", &diff)]);
        let hunk = lines_text(&render_numbered_diff("src/lib.rs", &diff, 40));

        assert!(title.starts_with("\u{200b} src/lib.rs"));
        assert!(hunk.starts_with("@@ -1 +1 @@"));
    }

    /// Text of every rendered line, for asserting on content rather than styling.
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

    /// A streaming answer arrives without its closing fence for as long as the
    /// model is still writing the block. The body must still render: the opening
    /// fence is already on screen, so dropping it shows an empty code block.
    #[test]
    fn unterminated_fence_still_renders_its_code() {
        let streaming = "Here is the function:\n\n```rust\npub fn alpha() -> usize { 41 }\npub fn beta() -> usize { 42 }";

        let text = lines_text(&render_markdown(streaming, 80));

        assert!(
            text.contains("rust"),
            "the language should render as a chip:\n{text}"
        );
        assert!(
            text.contains("alpha"),
            "partial code should render:\n{text}"
        );
        assert!(text.contains("beta"), "partial code should render:\n{text}");
        assert!(
            !text.contains("```"),
            "the source fence must not reach rendered output:\n{text}"
        );
    }

    /// The partial block is highlighted, not dumped as plain text, so it does not
    /// visibly restyle itself when the closing fence finally arrives.
    #[test]
    fn unterminated_fence_is_highlighted_like_a_closed_one() {
        let open = "```rust\npub fn alpha() -> usize { 41 }";
        let closed = "```rust\npub fn alpha() -> usize { 41 }\n```";

        let open_lines = render_markdown(open, 80);
        let closed_lines = render_markdown(closed, 80);

        // Find the code line in each rendering and compare span structure.
        let code_of = |lines: &[Line<'static>]| {
            lines
                .iter()
                .find(|l| lines_text(std::slice::from_ref(*l)).contains("alpha"))
                .expect("code line present")
                .clone()
        };
        let open_code = code_of(&open_lines);
        let closed_code = code_of(&closed_lines);

        assert!(
            open_code.spans.len() > 1,
            "partial code should be tokenised, not one plain span: {:?}",
            open_code.spans
        );
        assert_eq!(
            open_code.spans.len(),
            closed_code.spans.len(),
            "partial and closed blocks should highlight identically"
        );
    }

    /// Regression guard for the original defect shape: a message whose only
    /// content is an unterminated block must not render as an empty block.
    #[test]
    fn unterminated_fence_is_not_an_empty_block() {
        let text = lines_text(&render_markdown("```rust\nlet x = 1;", 80));

        assert!(
            text.contains("let x = 1;"),
            "an unterminated block must not render empty:\n{text}"
        );
    }

    #[test]
    fn empty_shows_blank_conversation() {
        let m = ConversationModel::from_messages(
            &[],
            &[],
            TaskLifecycle::Working,
            ConversationViewOpts::default(),
        );
        assert!(m.items.is_empty());
        assert!(m.lines().is_empty());
    }

    #[test]
    fn conversation_widget_applies_padding_offsets() {
        let m = ConversationModel::from_messages(
            &[],
            &[],
            TaskLifecycle::Working,
            ConversationViewOpts::default(),
        )
        .with_brand("forge 0.8.0")
        .with_home("workspace".into(), 2, "m".into(), "Mock".into(), true);
        let area = Rect::new(0, 0, 40, 8);
        let backend = TestBackend::new(area.width, area.height);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|frame| {
            frame.render_widget(ConversationWidget { model: &m }, area);
        })
        .unwrap();
        let buf = term.backend().buffer();
        assert_eq!(buf[(0, 0)].symbol(), " ");
        assert_eq!(buf[(1, 0)].symbol(), " ");
        assert_ne!(buf[(0, 1)].symbol(), "F");
    }

    #[test]
    fn stream_wait_status_is_not_rendered_inline() {
        let mut m = ConversationModel::from_messages(
            &[],
            &[],
            TaskLifecycle::Working,
            ConversationViewOpts {
                busy: true,
                stream_wait: Some((StreamWaitPhase::Thinking, 1.2)),
                ..Default::default()
            },
        );
        let text: String = m
            .lines()
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!text.contains("Thinking..."), "{text}");
        assert!(!text.contains("1.2s"), "{text}");

        m.opts.stream_wait = Some((StreamWaitPhase::Waiting, 0.3));
        let text: String = m
            .lines()
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!text.contains("Working..."), "{text}");
        assert!(!text.contains("0.3s"), "{text}");
    }

    #[test]
    fn streaming_assistant_has_live_label_and_cursor() {
        let m = ConversationModel::from_messages(
            &[],
            &[],
            TaskLifecycle::Working,
            ConversationViewOpts {
                busy: true,
                ..Default::default()
            },
        )
        .with_streaming_assistant("partial response");
        let text = m
            .lines()
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        // Streaming final-answer deltas render as answer text, not progress.
        assert!(!text.contains("Current progress"));
        assert!(text.contains("partial response▌"));
    }

    #[test]
    fn running_tool_card_shows_intent_without_arguments() {
        let m = ConversationModel::from_messages(
            &[],
            &[],
            TaskLifecycle::Working,
            ConversationViewOpts::default(),
        )
        .with_running_tool("read_file");
        let text = m
            .lines()
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(text.contains("● Exploring repository"));
        assert!(text.contains("Reading via read_file"));
    }

    /// The card is a box: every content row must start and end on the same
    /// columns as the corners, or the border visibly steps in and out.
    #[test]
    fn the_approval_card_border_is_square() {
        let p = ApprovalPendingPresentation {
            tool: "bash".into(),
            command: "git push -u origin feature".into(),
            cwd: "workspace".into(),
            env_delta: "inherited".into(),
            question: None,
            reason: None,
            failure: None,
            options: vec![ApprovalMenuRow {
                label: "Run once".into(),
                detail: None,
                help: Some("Runs now.".into()),
                key: None,
            }],
            selected: 0,
            focused: true,
        };
        for width in [40usize, 60, 72] {
            let lines = render_approval_card(&p, width);
            let widths: Vec<usize> = lines.iter().map(|l| l.width()).collect();
            let first = widths[0];
            assert!(
                widths.iter().all(|w| *w == first),
                "ragged card at width {width}: {widths:?}"
            );
            assert!(first <= width, "card overflowed {width}: {first}");
        }
    }

    /// The lead shares the row with the text it introduces, so it has to come
    /// out of the wrap width. It did not, and the reason's first line ran past
    /// the border and was clipped mid-word — "confined to the wo".
    #[test]
    fn a_reason_line_wraps_around_its_lead_instead_of_clipping() {
        // Long enough that a line wrapped without allowing for the lead runs
        // past the border, short enough to stay inside the line cap so every
        // word must survive somewhere.
        let reason = "writes are confined to the workspace root and the git \
                      directory is read-only inside it";
        let p = ApprovalPendingPresentation {
            tool: "bash".into(),
            command: "rm -rf /tmp/cache".into(),
            cwd: "workspace".into(),
            env_delta: "inherited".into(),
            question: None,
            reason: Some(reason.into()),
            failure: None,
            options: Vec::new(),
            selected: 0,
            focused: true,
        };

        let lines = render_approval_card(&p, 72);
        let rendered: Vec<String> = lines.iter().map(line_text).collect();

        // Every word of the reason has to survive somewhere on the card. A row
        // clipped at the border drops the tail of the reason silently, which
        // is exactly what this catches — the card still looks well-formed.
        let printed = rendered.join(" ");
        let missing: Vec<&str> = reason
            .split_whitespace()
            .filter(|word| !printed.contains(*word))
            .collect();

        assert!(
            missing.is_empty(),
            "the reason was clipped at the border, losing {missing:?}: {rendered:#?}"
        );
    }

    /// The category explanation reads identically for every command in that
    /// category. When the sandbox's own words are available they lead, and the
    /// explanation shrinks to a footnote rather than five rows of policy.
    #[test]
    fn the_real_refusal_leads_and_the_category_is_demoted() {
        let reason = "blocked by the sandbox: writes are confined to the workspace, \
                      and .git/.forge are read-only inside it. Paths outside the \
                      workspace do not exist inside the sandbox, so they report as \
                      missing rather than forbidden.";
        let mut p = ApprovalPendingPresentation {
            tool: "bash".into(),
            command: "rm -rf /tmp/cache".into(),
            cwd: "workspace".into(),
            env_delta: "inherited".into(),
            question: None,
            reason: Some(reason.into()),
            failure: None,
            options: Vec::new(),
            selected: 0,
            focused: true,
        };

        let without = render_approval_card(&p, 72).len();
        p.failure = Some("rm: /tmp/cache: Operation not permitted".into());
        let with = render_approval_card(&p, 72);
        let text: Vec<String> = with.iter().map(line_text).collect();

        assert!(
            text.iter()
                .any(|row| row.contains("Operation not permitted")),
            "the sandbox's own words are missing: {text:#?}"
        );
        assert!(
            text.iter()
                .any(|row| row.contains("The sandbox refused it")),
            "the evidence is not introduced: {text:#?}"
        );
        assert!(
            with.len() < without,
            "the card grew instead of trading policy for evidence: {} -> {}",
            without,
            with.len()
        );
    }

    fn approval_with_help(width: usize) -> Vec<Line<'static>> {
        let p = ApprovalPendingPresentation {
            tool: "bash".into(),
            command: "git push -u origin feature".into(),
            cwd: "workspace".into(),
            env_delta: "inherited".into(),
            question: None,
            reason: Some("your permissions.toml denies bash(*)".into()),
            failure: None,
            options: vec![
                ApprovalMenuRow {
                    label: "Run once".into(),
                    detail: None,
                    help: Some("Runs now. You will be asked again.".into()),
                    key: None,
                },
                ApprovalMenuRow {
                    label: "Don't run".into(),
                    detail: None,
                    help: Some("The agent is told it was denied. Nothing runs.".into()),
                    key: None,
                },
            ],
            selected: 0,
            focused: true,
        };
        render_approval_card(&p, width)
    }

    fn card_text(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The reason was already on the payload and simply never rendered, so the
    /// prompt read as arbitrary.
    #[test]
    fn a_wide_approval_card_says_why_it_is_asking() {
        let text = card_text(&approval_with_help(72));
        assert!(
            text.contains("Asked because") && text.contains("permissions.toml"),
            "{text}"
        );
    }

    /// An unselected option used to show nothing, so you had to arrow onto it
    /// to learn what it did. It now carries a short consequence on its own row,
    /// which costs the card no extra height.
    #[test]
    fn every_approval_option_explains_itself_when_there_is_room() {
        let lines = approval_with_help(72);
        let text = card_text(&lines);
        assert!(text.contains("The agent is told it was denied"), "{text}");
        // The qualifying sentence belongs to the selected option's full text.
        assert!(!text.contains("Nothing runs"), "{text}");
        let widths: Vec<usize> = lines.iter().map(|l| l.width()).collect();
        assert!(widths.iter().all(|w| *w == widths[0]), "{widths:?}");
    }

    /// In the sidebar every optional row wraps to three or four lines and
    /// pushes the card's own title off the pane, so the compact card carries
    /// only what the decision needs.
    #[test]
    fn a_narrow_approval_card_drops_what_it_cannot_afford() {
        let narrow = approval_with_help(30);
        let text = card_text(&narrow);
        assert!(!text.contains("Asked because"), "{text}");
        assert!(!text.contains("The agent is told"), "{text}");
        // The decision itself always survives.
        assert!(
            text.contains("Run once") && text.contains("Don't run"),
            "{text}"
        );
        let widths: Vec<usize> = narrow.iter().map(|l| l.width()).collect();
        assert!(widths.iter().all(|w| *w == widths[0]), "{widths:?}");
    }

    /// A working directory has no spaces, so `wrap` hands it back whole. It
    /// used to run straight out through the card's right border.
    #[test]
    fn an_unbreakable_path_cannot_burst_the_approval_card() {
        let p = ApprovalPendingPresentation {
            tool: "bash".into(),
            command: "ls -la src".into(),
            cwd: "/private/tmp/claude-501/-Users-someone-Projects-forge/ac5a5dcf-403d-4bce-b017-233f3db8e1c0/scratchpad/lab".into(),
            env_delta: "inherited".into(),
            question: None,
            reason: None,
            failure: None,
            options: vec![ApprovalMenuRow {
                label: "Run once".into(),
                detail: None,
                help: None,
                key: None,
            }],
            selected: 0,
            focused: true,
        };
        let lines = render_approval_card(&p, 72);
        let widths: Vec<usize> = lines.iter().map(|l| l.width()).collect();
        assert!(
            widths.iter().all(|w| *w == widths[0]),
            "path burst the card: {widths:?}"
        );
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(text.contains("lab"), "folder name must survive:\n{text}");
    }

    /// A single unbreakable command token must be clipped by the card, not
    /// allowed to push the border out.
    #[test]
    fn an_unbreakable_command_cannot_burst_the_approval_card() {
        let p = ApprovalPendingPresentation {
            tool: "bash".into(),
            command: "x".repeat(400),
            cwd: "workspace".into(),
            env_delta: "inherited".into(),
            question: None,
            reason: None,
            failure: None,
            options: vec![ApprovalMenuRow {
                label: "Run once".into(),
                detail: None,
                help: None,
                key: None,
            }],
            selected: 0,
            focused: true,
        };
        let lines = render_approval_card(&p, 72);
        let widths: Vec<usize> = lines.iter().map(|l| l.width()).collect();
        assert!(
            widths.iter().all(|w| *w == widths[0]),
            "command burst the card: {widths:?}"
        );
    }

    #[test]
    fn pending_approval_renders_full_redacted_payload_inline() {
        let m = ConversationModel::from_messages(
            &[],
            &[],
            TaskLifecycle::Waiting,
            ConversationViewOpts::default(),
        )
        .with_pending_approval(
            ApprovalRequestView {
                tool: "bash".into(),
                command: "git push -u origin feature".into(),
                cwd: "workspace".into(),
                env_delta: "inherited".into(),
                question: None,
                reason: None,
                failure: None,
            },
            vec![
                ApprovalMenuRow {
                    label: "Run once".into(),
                    detail: None,
                    help: Some("Runs now. You will be asked again.".into()),
                    key: None,
                },
                ApprovalMenuRow {
                    label: "Remember similar commands this session".into(),
                    detail: Some("bash(git push *)".into()),
                    help: Some("Would match: git push …".into()),
                    key: None,
                },
                ApprovalMenuRow {
                    label: "Don't run".into(),
                    detail: None,
                    help: Some("The agent is told the command was denied.".into()),
                    key: None,
                },
            ],
            0,
            false,
        );
        let text = m
            .lines()
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(
            text.contains("Forge wants to run a shell command."),
            "{text}"
        );
        assert!(!text.contains("⏸ APPROVAL REQUIRED"), "{text}");
        assert!(text.contains("git push -u origin feature"), "{text}");
        assert!(text.contains("workspace"), "{text}");
        assert!(!text.contains("cwd: workspace"), "{text}");
        assert!(text.contains("\u{276f} Run once"), "{text}");
        assert!(text.contains("Approval needed"), "{text}");
        assert!(
            text.contains("Remember similar commands this session"),
            "{text}"
        );
        assert!(
            text.contains("Runs now. You will be asked again."),
            "{text}"
        );
        // Every option carries its consequence at this width, not just the
        // selected one — an unselected option used to be unreadable.
        assert!(text.contains("Would match: git push"), "{text}");
        assert!(text.contains("↑↓"), "{text}");
        assert!(text.contains("Enter"), "{text}");
        assert!(text.contains("Esc"), "{text}");
        assert!(text.contains("don't run"), "{text}");
    }

    #[test]
    fn approval_prompt_is_a_conversation_message_not_a_card() {
        const PANE_WIDTH: usize = 100;
        let m = ConversationModel::from_messages(
            &[],
            &[],
            TaskLifecycle::Waiting,
            ConversationViewOpts::default(),
        )
        .with_pending_approval(
            ApprovalRequestView {
                tool: "bash".into(),
                command: "ls".into(),
                cwd: "wd".into(),
                env_delta: "inherited".into(),
                question: None,
                reason: None,
                failure: None,
            },
            vec![ApprovalMenuRow {
                label: "Run once".into(),
                detail: None,
                help: Some("Runs now. You will be asked again.".into()),
                key: None,
            }],
            0,
            false,
        );
        let lines = m.lines_for_width(PANE_WIDTH);
        let text = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(
            text.contains("Forge wants to run a shell command."),
            "{text}"
        );
        assert!(
            lines.iter().all(|line| !line_text(line).starts_with('┌')),
            "approval must not render as a boxed card: {text}"
        );
    }

    #[test]
    fn pending_question_renders_as_a_numbered_card() {
        const PANE_WIDTH: usize = 100;
        let m = ConversationModel::from_messages(
            &[],
            &[],
            TaskLifecycle::Waiting,
            ConversationViewOpts::default(),
        )
        .with_pending_question(QuestionPendingPresentation {
            header: "Database".into(),
            question: "Which database?".into(),
            options: vec![
                QuestionMenuRow {
                    label: "Postgres (Recommended)".into(),
                    description: Some("Relational default.".into()),
                    chosen: false,
                },
                QuestionMenuRow {
                    label: "SQLite".into(),
                    description: Some("Embedded, single file.".into()),
                    chosen: false,
                },
                QuestionMenuRow {
                    label: "Other".into(),
                    description: Some("Type a custom answer in the composer.".into()),
                    chosen: false,
                },
            ],
            selected: 0,
            multi_select: false,
            question_index: 0,
            question_count: 1,
            focused: true,
        });
        let lines = m.lines_for_width(PANE_WIDTH);
        let text = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(text.contains("Which database?"), "{text}");
        // Numbered, because 1-9 already answer the question, and marked with
        // the same caret the approval card uses.
        assert!(
            text.contains("\u{276f} 1. Postgres (Recommended)"),
            "{text}"
        );
        assert!(text.contains("Relational default."), "{text}");
        assert!(text.contains("Other"), "{text}");
        // Every option explains itself, not only the one under the cursor:
        // choosing between options means comparing them.
        assert!(
            text.contains("Embedded, single file."),
            "unselected option lost its description: {text}"
        );
        // Framed like the approval card. Both are the same thing to the
        // operator — the agent has stopped and cannot continue without them.
        assert!(
            lines.iter().any(|line| line_text(line).contains('┌')),
            "question should render as a card: {text}"
        );
    }

    #[test]
    fn context_handoff_card_shows_lifecycle_and_progress() {
        let m = ConversationModel {
            items: vec![ChatItem::ContextHandoff {
                before_pct: 82.0,
                after_pct: 14.0,
                goal: "rate limiting middleware".into(),
                completed: vec!["middleware scaffold".into()],
                next_actions: vec!["wire public router".into()],
            }],
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts::default(),
        };
        let blocks = m.semantic_blocks();
        assert!(blocks.iter().any(|block| matches!(
            block,
            ConversationBlock::Callout(CalloutPresentation { text, .. })
                if text.contains("rate limiting middleware")
        )));
    }

    #[test]
    fn session_recovery_card_shows_replay_guarantees() {
        let m = ConversationModel {
            items: vec![ChatItem::SessionRecovery {
                session_id: "a1b2c3d4".into(),
                journal_path: ".forge/sessions/a1b2c3d4.db".into(),
                last_seq: 1847,
                model_steps: 62,
                tool_results: 41,
                incomplete_intents: 1,
                last_assistant: Some("Continuing from the restored journal.".into()),
            }],
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts::default(),
        };
        let blocks = m.semantic_blocks();
        assert!(blocks.iter().any(|block| matches!(
            block,
            ConversationBlock::Callout(CalloutPresentation { text, .. })
                if text.contains("Restoring session")
        )));
    }

    #[test]
    fn repair_task_renders_evaluator_report_and_generator_response() {
        let messages = vec![
            Message {
                outcome: Default::default(),
                role: MessageRole::User,
                content: "[REPAIR TASK EVAL-01]\nSENSOR · DETERMINISTIC\ncargo test · failed\nEVALUATOR REPORT\nCriteria: public API returns 429\nFinding: layer is registered too late\nRepair: attach layer to public router".into(),
                tool_call_id: None,
                name: None,
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
            attachments: Vec::new(),
            },
            Message {
                outcome: Default::default(),
                role: MessageRole::Assistant,
                content: "Moving the layer onto the public router.".into(),
                tool_call_id: None,
                name: None,
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
            attachments: Vec::new(),
            },
        ];
        let model = ConversationModel::from_messages(
            &messages,
            &[],
            TaskLifecycle::Working,
            ConversationViewOpts::default(),
        );
        let text = model
            .lines()
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(text.contains("SENSOR · DETERMINISTIC"), "{text}");
        assert!(
            text.contains("Moving the layer onto the public router."),
            "{text}"
        );
    }

    fn three_block_model() -> ConversationModel {
        let msgs = vec![
            Message {
                outcome: Default::default(),
                role: MessageRole::User,
                content: "first".into(),
                tool_call_id: None,
                name: None,
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
                attachments: Vec::new(),
            },
            Message {
                outcome: Default::default(),
                role: MessageRole::Assistant,
                content: "second".into(),
                tool_call_id: None,
                name: None,
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
                attachments: Vec::new(),
            },
            Message {
                outcome: Default::default(),
                role: MessageRole::User,
                content: "third".into(),
                tool_call_id: None,
                name: None,
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
                attachments: Vec::new(),
            },
        ];
        ConversationModel::from_messages(
            &msgs,
            &[],
            TaskLifecycle::Working,
            ConversationViewOpts::default(),
        )
    }

    fn is_rule_line(line: &Line<'static>) -> bool {
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        !text.is_empty() && text.chars().all(|c| c == '─')
    }

    fn rule_lines<'a>(lines: &'a [Line<'static>]) -> Vec<&'a Line<'static>> {
        lines.iter().filter(|line| is_rule_line(line)).collect()
    }

    fn rendered_text(model: &ConversationModel) -> String {
        model
            .lines()
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>()
    }

    fn line_text(line: &Line<'static>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn tool_turn_items() -> Vec<ChatItem> {
        vec![
            ChatItem::User {
                text: "fix the failing test".into(),
            },
            ChatItem::ToolCard {
                name: "read_file".into(),
                summary: "src/lib.rs · 2 lines".into(),
                detail: "src/lib.rs".into(),
                state: ToolCardState::Done,
                duration: None,
                subcommand: None,
                outcome: forge_types::ExecutionOutcome::Success,
            },
            ChatItem::ToolCard {
                name: "run_shell".into(),
                summary: "cargo test · failed".into(),
                detail: "output".into(),
                state: ToolCardState::Done,
                duration: None,
                subcommand: Some("cargo test".into()),
                outcome: forge_types::ExecutionOutcome::Failed {
                    exit_code: Some(101),
                },
            },
            ChatItem::Assistant {
                text: "Root cause: float rounding.".into(),
            },
        ]
    }

    fn tool_turn_model() -> ConversationModel {
        ConversationModel {
            items: tool_turn_items(),
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts::default(),
        }
    }

    fn plan_items() -> Vec<forge_types::PlanItem> {
        use forge_types::PlanStepStatus;
        vec![
            forge_types::PlanItem {
                step: "Inspect failure".into(),
                status: PlanStepStatus::Completed,
            },
            forge_types::PlanItem {
                step: "Fix float comparison".into(),
                status: PlanStepStatus::InProgress,
            },
        ]
    }

    fn planned_turn_model() -> ConversationModel {
        ConversationModel {
            items: vec![
                ChatItem::User {
                    text: "fix the failing test".into(),
                },
                ChatItem::PlanChecklist {
                    explanation: Some("Next steps".into()),
                    steps: plan_items(),
                    evidence: Vec::new(),
                },
                // A later plan_update in the same turn re-renders the
                // checklist but must not open another phase rule.
                ChatItem::PlanChecklist {
                    explanation: Some("Next steps".into()),
                    steps: plan_items(),
                    evidence: Vec::new(),
                },
                ChatItem::ToolCard {
                    name: "run_shell".into(),
                    summary: "cargo test · passed".into(),
                    detail: "output".into(),
                    state: ToolCardState::Done,
                    duration: None,
                    subcommand: Some("cargo test".into()),
                    outcome: forge_types::ExecutionOutcome::Success,
                },
                ChatItem::Assistant {
                    text: "Fixed.".into(),
                },
            ],
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts::default(),
        }
    }

    #[test]
    fn every_turn_boundary_gets_a_rule_even_without_a_plan() {
        let model = three_block_model();
        assert_eq!(model.semantic_blocks().len(), 3);

        let lines = model.lines_for_width(60);
        // [question, answer, question]: two turns, no plan checklist at all.
        // The rule is a turn-boundary marker now, not tied to plan presence,
        // so the second turn still opens with exactly one rule.
        assert_eq!(
            rule_lines(&lines).len(),
            1,
            "a new turn opens with a rule even without a plan"
        );

        let first = lines.first().expect("non-empty transcript");
        assert!(!is_rule_line(first), "no separator before the first entry");

        let rule_pos = lines.iter().position(is_rule_line).expect("rule present");
        let after = lines[rule_pos + 1..]
            .iter()
            .map(line_text)
            .find(|t| !t.is_empty())
            .expect("content after the rule");
        assert!(
            after.contains("third"),
            "the rule immediately precedes the second turn's user message, got {after:?}"
        );
    }

    #[test]
    fn plan_checklists_alone_do_not_open_a_rule_only_turn_boundaries_do() {
        // `planned_turn_model()` is a single turn containing two consecutive
        // PlanChecklist items (a plan_update). The rule is now purely a
        // turn-boundary marker, decoupled from plan presence — so a
        // single-turn conversation gets zero rules, plan or no plan, and a
        // second plan_update in the same turn still doesn't add one either.
        let model = planned_turn_model();
        let lines = model.lines_for_width(80);
        assert_eq!(
            rule_lines(&lines).len(),
            0,
            "a plan checklist alone must not open a rule without a new turn"
        );
    }

    #[test]
    fn hairline_rule_width_tracks_pane_width() {
        let model = three_block_model();
        for width in [40usize, 90usize] {
            let lines = model.lines_for_width(width);
            let rules = rule_lines(&lines);
            assert_eq!(rules.len(), 1);
            for rule in rules {
                let text = line_text(rule);
                assert_eq!(text.chars().count(), width.max(4));
            }
        }
    }

    #[test]
    fn tool_activity_groups_on_the_turn_rail() {
        let model = tool_turn_model();
        let lines = model.lines_for_width(80);
        assert_eq!(rule_lines(&lines).len(), 0);

        let railed: Vec<&Line<'static>> = lines
            .iter()
            .filter(|l| line_text(l).starts_with('│'))
            .collect();
        assert!(!railed.is_empty(), "tool trail renders on the rail");
        assert!(railed
            .iter()
            .any(|l| line_text(l).contains("Explored repository")));
        assert!(railed.iter().any(|l| line_text(l).contains("cargo test")));

        // User message and final answer break out of the rail.
        let user = lines
            .iter()
            .find(|l| line_text(l).contains("fix the failing test"))
            .expect("user message");
        assert!(!line_text(user).starts_with('│'));
        let answer = lines
            .iter()
            .find(|l| line_text(l).contains("Root cause"))
            .expect("answer");
        assert!(!line_text(answer).starts_with('│'));
        let answer_idx = lines
            .iter()
            .position(|l| line_text(l).contains("Root cause"))
            .unwrap();
        assert!(
            lines[answer_idx - 1]
                .spans
                .iter()
                .all(|s| s.content.is_empty()),
            "the answer is separated from the rail trail by a blank line"
        );
    }

    #[test]
    fn narrow_panes_drop_the_rail() {
        let model = tool_turn_model();
        let lines = model.lines_for_width(40);
        assert!(lines.iter().all(|l| !line_text(l).contains('│')));
        assert_eq!(rule_lines(&lines).len(), 0);
    }

    #[test]
    fn expand_does_not_change_rule_or_rail_structure() {
        let collapsed = ConversationModel {
            items: tool_turn_items(),
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts::default(),
        };
        let expanded = ConversationModel {
            items: tool_turn_items(),
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts {
                tool_expanded: true,
                ..Default::default()
            },
        };
        let collapsed_lines = collapsed.lines_for_width(80);
        let expanded_lines = expanded.lines_for_width(80);
        assert_eq!(
            rule_lines(&collapsed_lines).len(),
            rule_lines(&expanded_lines).len(),
            "expand must not change phase boundaries"
        );
        let railed =
            |ls: &[Line<'static>]| ls.iter().filter(|l| line_text(l).starts_with('│')).count();
        assert!(
            railed(&expanded_lines) >= railed(&collapsed_lines),
            "expanded tool items stay on the rail"
        );
    }

    #[test]
    fn rule_and_rail_structure_is_identical_across_themes() {
        let model = planned_turn_model();
        let registry = crate::theme_registry::ThemeRegistry::load(None);
        let mut baseline: Option<(usize, usize)> = None;
        for id in [
            "gruvbox-dark",
            "kanagawa-wave",
            "catppuccin-mocha",
            "solarized-dark",
            "solarized-light",
        ] {
            crate::theme::install(registry.clone(), id);
            let lines = model.lines_for_width(80);
            let cur = (
                rule_lines(&lines).len(),
                lines
                    .iter()
                    .filter(|l| line_text(l).starts_with('│'))
                    .count(),
            );
            if let Some(prev) = baseline {
                assert_eq!(cur, prev, "structural layout differs under theme {id}");
            } else {
                baseline = Some(cur);
            }
        }
    }

    fn bash_tool_card(command: &str, output_lines: &str, expanded: bool) -> ConversationModel {
        ConversationModel {
            items: vec![ChatItem::ToolCard {
                name: "bash".into(),
                summary: format!("$ {command} · {} output lines", output_lines),
                detail: (0..output_lines.parse::<usize>().unwrap_or(0))
                    .map(|i| format!("line {i}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                state: ToolCardState::Done,
                duration: None,
                subcommand: Some(format!("$ {command}")),
                outcome: forge_types::ExecutionOutcome::Success,
            }],
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts {
                tool_expanded: expanded,
                ..ConversationViewOpts::default()
            },
        }
    }

    #[test]
    fn collapsed_command_summary_splits_at_first_pipe_or_semicolon() {
        let items = vec!["a\nb\nc".to_string()];
        let (command, output_lines) = collapsed_command_summary(
            "$ cargo test --workspace; git diff --check; git status --short --pad-past-eighty-chars-total-length-of-this-line",
            &items,
        )
        .expect("long command should collapse");
        assert_eq!(command, "$ cargo test --workspace");
        assert_eq!(output_lines, 3);
    }

    #[test]
    fn collapsed_command_summary_ellipsizes_an_overlong_single_segment() {
        let items: Vec<String> = vec![];
        let long_single_segment = "cargo test --workspace --all-features --lib --bins --tests --examples --benches --no-fail-fast";
        let (command, _) =
            collapsed_command_summary(&format!("$ {long_single_segment}"), &items).expect("long");
        assert!(command.ends_with('…'), "{command}");
        assert!(command.chars().count() <= COMMAND_LINE_MAX_CHARS + "$ …".chars().count());
    }

    #[test]
    fn collapsed_command_summary_ignores_short_commands() {
        let items: Vec<String> = vec![];
        assert_eq!(
            collapsed_command_summary("$ cargo test -p forge-tui", &items),
            None
        );
        // Non-command summaries (file counts, etc.) are never affected.
        assert_eq!(collapsed_command_summary("3 files inspected", &items), None);
    }

    #[test]
    fn diff_line_numbers_track_additions_removals_and_context() {
        let diff = [
            "@@ -10,3 +20,4 @@",
            " context",
            "-removed",
            "+added",
            "+extra",
            " tail",
        ]
        .map(str::to_string);

        let numbered = number_diff_lines(&diff);

        assert_eq!(
            numbered
                .iter()
                .map(|line| (line.old, line.new, line.marker))
                .collect::<Vec<_>>(),
            vec![
                (None, None, ' '),
                (Some(10), Some(20), ' '),
                (Some(11), None, '-'),
                (None, Some(21), '+'),
                (None, Some(22), '+'),
                (Some(12), Some(23), ' '),
            ]
        );
    }

    /// The cache is fed one chunk at a time for a whole turn; every
    /// intermediate state must equal a one-shot render of the same buffer.
    ///
    /// This is the test that would catch an incremental append going wrong —
    /// the cache grows in place, so a bad append is frozen for the rest of the
    /// turn rather than corrected on the next frame.
    #[test]
    fn the_stream_cache_matches_a_one_shot_render_at_every_prefix() {
        let full = "Here is the plan.\n\n- step one\n- step two\n\nNow the code:\n\n```rust\nfn apply(x: usize) -> usize {\n    x + 1\n}\n```\n\nAnd a table:\n\n| a | b |\n| - | - |\n| 1 | 2 |\n\nDone.\n";
        let mut cache = StreamMarkdownCache::default();
        for end in 0..=full.len() {
            if !full.is_char_boundary(end) {
                continue;
            }
            let buffer = &full[..end];
            let incremental = lines_text(&cache.render(buffer, 60, usize::MAX));
            let one_shot = lines_text(&crate::markdown::render_markdown(buffer, 60));
            assert_eq!(
                incremental, one_shot,
                "cache diverged from a one-shot render at prefix length {end}"
            );
        }
    }

    /// A short conversation used to paint from the top of the pane, leaving the
    /// newest line — the one being written — stranded mid-screen above a
    /// screenful of nothing.
    #[test]
    fn a_short_conversation_hugs_the_composer() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;

        let lines = vec![Line::from("only line")];
        let area = Rect::new(0, 0, 20, 10);
        let row_of = |anchor_bottom: bool| {
            let mut buf = Buffer::empty(area);
            render_conversation_lines(
                TranscriptSlices {
                    lines: &lines,
                    tail_lines: &[],
                    status_lines: &[],
                },
                0,
                true,
                0,
                anchor_bottom,
                None,
                area,
                &mut buf,
            );
            (0..area.height).find(|y| {
                (0..area.width).any(|x| buf[(x, *y)].symbol().trim() == "only")
                    || (0..area.width).any(|x| buf[(x, *y)].symbol() != " ")
            })
        };
        assert_eq!(row_of(false), Some(0), "top-anchored content moved");
        assert_eq!(
            row_of(true),
            Some(area.height - 1),
            "content did not sink to the bottom of the pane"
        );
    }

    /// Blocks were separated by a leading blank *and* a trailing pair, so gaps
    /// doubled and tripled: the transcript read as a list of far-apart items
    /// rather than a conversation.
    #[test]
    fn the_transcript_never_stacks_blank_lines() {
        let model = ConversationModel {
            items: vec![
                ChatItem::User {
                    text: "do the thing".into(),
                },
                ChatItem::Thinking {
                    text: "planning".into(),
                    duration_secs: Some(1.0),
                },
                ChatItem::Assistant {
                    text: "Here you go.\n\n- one\n- two\n".into(),
                },
            ],
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts::default(),
        };
        let lines = model.lines_for_width(80);
        let mut blanks = 0usize;
        for line in &lines {
            if line.width() == 0 || line.spans.iter().all(|s| s.content.trim().is_empty()) {
                blanks += 1;
                assert!(
                    blanks < 2,
                    "two blank rows in a row:\n{}",
                    lines_text(&lines)
                );
            } else {
                blanks = 0;
            }
        }
    }

    /// Text that later bytes can still re-render is painted a step down in
    /// value, so a streaming answer visibly sets. Settled text keeps its own
    /// colour, and the caret — the live edge — keeps full brightness.
    #[test]
    fn the_unsettled_tail_renders_dimmer_than_settled_text() {
        let mut cache = StreamMarkdownCache::default();
        // The first paragraph is settled; the second is still being written.
        let lines = cache.render("Settled paragraph.\n\nIn flight now▌", 60, usize::MAX);
        let dim = crate::theme::text_dim_color();
        let fg_of = |needle: &str| {
            lines
                .iter()
                .find_map(|line| {
                    line.spans
                        .iter()
                        .find(|span| span.content.contains(needle))
                        .map(|span| span.style.fg)
                })
                .unwrap_or_else(|| panic!("{needle} rendered"))
        };
        assert_eq!(fg_of("flight"), Some(dim), "the tail was not faded");
        assert_ne!(fg_of("Settled"), Some(dim), "settled text must not fade");
        assert_ne!(
            fg_of("▌"),
            Some(dim),
            "the caret marks the live edge and keeps its colour"
        );
    }

    /// Fading is paint, not content: the characters on screen must not change.
    #[test]
    fn fading_the_tail_leaves_the_text_alone() {
        let mut cache = StreamMarkdownCache::default();
        let buffer = "Alpha.\n\n- one\n- two▌";
        let faded = lines_text(&cache.render(buffer, 60, usize::MAX));
        let plain = lines_text(&crate::markdown::render_markdown(buffer, 60));
        assert_eq!(faded, plain);
    }

    /// A width change must discard the cache: the lines were wrapped to the old
    /// width and cannot be appended to.
    #[test]
    fn the_stream_cache_rebuilds_on_a_width_change() {
        let text =
            "Alpha beta gamma delta epsilon zeta eta theta.\n\nSecond paragraph here.\n\nTail.";
        let mut cache = StreamMarkdownCache::default();
        let narrow = lines_text(&cache.render(text, 30, usize::MAX));
        let wide = lines_text(&cache.render(text, 90, usize::MAX));
        let back = lines_text(&cache.render(text, 30, usize::MAX));

        assert_eq!(
            wide,
            lines_text(&crate::markdown::render_markdown(text, 90))
        );
        assert_eq!(
            back, narrow,
            "returning to a width must reproduce it exactly"
        );
        assert_ne!(
            narrow, wide,
            "the widths must actually differ, or this proves nothing"
        );
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
        let windowed = cache.render(&body, 60, 10);
        let whole = cache.render(&body, 60, usize::MAX);

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

    fn plain_msg(role: MessageRole, content: &str) -> Message {
        Message {
            outcome: Default::default(),
            role,
            content: content.into(),
            tool_call_id: None,
            name: None,
            thinking: None,
            thinking_duration_secs: None,
            tool_calls: vec![],
            attachments: Vec::new(),
        }
    }

    fn tool_result(id: &str, name: &str) -> Message {
        Message {
            tool_call_id: Some(id.into()),
            name: Some(name.into()),
            ..plain_msg(MessageRole::Tool, "ok")
        }
    }

    fn rendered_height(msgs: &[Message]) -> usize {
        ConversationModel::from_messages(
            msgs,
            &[],
            TaskLifecycle::Working,
            ConversationViewOpts::default(),
        )
        .lines_for_width(80)
        .len()
    }

    /// The view must never get shorter as work arrives.
    ///
    /// A lone routine card used to render full-height and then collapse the
    /// moment a sibling completed, shrinking the block under everything above
    /// it — which reads as the whole transcript jumping.
    #[test]
    fn routine_group_height_never_shrinks() {
        let mut msgs = vec![plain_msg(MessageRole::User, "hi")];
        let mut previous = rendered_height(&msgs);
        for i in 0..4 {
            msgs.push(tool_result(&i.to_string(), "read_file"));
            let now = rendered_height(&msgs);
            assert!(
                now >= previous,
                "height shrank from {previous} to {now} when routine card {i} arrived"
            );
            previous = now;
        }
    }

    /// `ChatItem::Thinking` flushes any pending activity group before pushing
    /// its own block, so its arrival can force a collapse too.
    #[test]
    fn a_thinking_block_arriving_mid_group_does_not_shrink_the_view() {
        let mut msgs = vec![
            plain_msg(MessageRole::User, "hi"),
            tool_result("1", "read_file"),
            tool_result("2", "glob"),
        ];
        let before = rendered_height(&msgs);

        let mut thinking = plain_msg(MessageRole::Assistant, "answer");
        thinking.thinking = Some("planning the edit".into());
        thinking.thinking_duration_secs = Some(1.2);
        msgs.push(thinking);

        let after = rendered_height(&msgs);
        assert!(
            after >= before,
            "height shrank from {before} to {after} when a thinking block arrived"
        );
    }

    /// Every routine tool can now form a group of one, and not all of them have
    /// a counter in `activity_group_summary` — `ls` and read-only `git` are
    /// neither a read nor a search, so the counts come back empty. That was
    /// invisible while a lone card skipped grouping.
    #[test]
    fn no_routine_group_renders_a_blank_summary() {
        for name in ["read_file", "ls", "glob", "grep", "rg"] {
            let msgs = vec![plain_msg(MessageRole::User, "hi"), tool_result("1", name)];
            let model = ConversationModel::from_messages(
                &msgs,
                &[],
                TaskLifecycle::Working,
                ConversationViewOpts::default(),
            );
            let summaries: Vec<&str> = model
                .items
                .iter()
                .filter_map(|item| match item {
                    ChatItem::ActivityGroup { summary, .. } => Some(summary.as_str()),
                    _ => None,
                })
                .collect();
            assert!(
                !summaries.is_empty(),
                "{name} should form a routine group: {:?}",
                model.items
            );
            for summary in summaries {
                assert!(
                    !summary.trim().is_empty(),
                    "{name} rendered a blank group summary"
                );
            }
        }
    }
}
