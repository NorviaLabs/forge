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
    let inner_w = longest_content.min(PROSE_MAX_WIDTH).min(available_interior);
    let border = theme::accent_style();
    lines.push(card_top_border(inner_w + 4, None, border));
    for item in &plan.steps {
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
    }
    lines.push(card_bottom_border(inner_w + 4, border));
    lines
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
}

impl ConversationRender for ConversationModel {
    fn lines(&self) -> Vec<Line<'static>> {
        self.lines_for_width(if self.opts.compact { 88 } else { 100 })
    }

    /// Build display lines for the actual conversation viewport. Prose gets a
    /// readable cap; code and structured blocks keep the full pane width.
    fn lines_for_width(&self, available_width: usize) -> Vec<Line<'static>> {
        let width = available_width.max(4);
        let prose_width = width
            .saturating_sub(MESSAGE_PADDING * 2)
            .clamp(4, PROSE_MAX_WIDTH);
        let mut lines = Vec::new();
        let gap = !self.opts.compact;
        let rail = width >= RAIL_MIN_WIDTH;
        let blocks = self.semantic_blocks();
        // A full-width rule opens every turn boundary (every UserMessage
        // after the first block in the transcript) — independent of whether
        // that turn has a plan checklist. Compact tool rows stay tight
        // against each other; major blocks get a blank separator.
        let mut seen_any_block = false;
        for block in blocks {
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
                    lines.extend([Line::from(""), Line::from("")]);
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
                        lines.extend([Line::from(""), Line::from("")]);
                    }
                }
                ConversationBlock::AssistantAnswer(p) => {
                    let parts = render_markdown(&p.text, prose_width);
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
                        lines.extend([Line::from(""), Line::from("")]);
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
                        theme::metadata_style(),
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
                    let border = if p.focused {
                        theme::approval_accent()
                    } else {
                        theme::border_muted()
                    };
                    let title = "⏸ APPROVAL REQUIRED";
                    const HINT: &str = "↑↓ select · Enter confirm · Esc cancel";
                    let cwd_env = format!("cwd: {}  env: {}", p.cwd, p.env_delta);
                    let option_rows: Vec<String> = p
                        .options
                        .iter()
                        .map(|opt| match &opt.detail {
                            Some(detail) => format!("›  {}  {detail}", opt.label),
                            None => format!("›  {}", opt.label),
                        })
                        .collect();
                    // Hug the card's own content instead of always spanning
                    // the full pane width; still capped at prose width for
                    // readability and clamped to what the pane can show.
                    let longest_content = [
                        title.chars().count() + 5,
                        cwd_env.chars().count(),
                        HINT.chars().count(),
                        p.command.chars().count(),
                    ]
                    .into_iter()
                    .chain(option_rows.iter().map(|r| r.chars().count()))
                    .max()
                    .unwrap_or(0);
                    let available_interior = width.saturating_sub(4);
                    let inner_w = longest_content
                        .min(PROSE_MAX_WIDTH)
                        .min(available_interior)
                        .max((title.chars().count() + 1).min(available_interior));
                    let card_width = inner_w + 4;
                    let fill = Some(theme::panel_alt_bg());
                    let boxed_line =
                        |s: &str, style: Style| card_content_line(s, inner_w, style, border, fill);
                    lines.push(card_top_border(card_width, Some(title), border));
                    lines.push(boxed_line("", theme::panel()));
                    for wrapped in wrap(&p.command, inner_w) {
                        lines.push(boxed_line(&wrapped, theme::muted()));
                    }
                    for wrapped in wrap(&cwd_env, inner_w) {
                        lines.push(boxed_line(&wrapped, theme::muted()));
                    }
                    lines.push(boxed_line("", theme::panel()));
                    for (idx, opt) in p.options.iter().enumerate() {
                        let marker = if idx == p.selected { "›" } else { " " };
                        let style = if idx == p.selected {
                            theme::text().add_modifier(Modifier::BOLD)
                        } else {
                            theme::muted()
                        };
                        let row = match &opt.detail {
                            Some(detail) => format!("{marker} {}  {detail}", opt.label),
                            None => format!("{marker} {}", opt.label),
                        };
                        for wrapped in wrap(&row, inner_w) {
                            lines.push(boxed_line(&wrapped, style));
                        }
                    }
                    for wrapped in wrap(HINT, inner_w) {
                        lines.push(boxed_line(&wrapped, theme::metadata_style()));
                    }
                    lines.push(boxed_line("", theme::panel()));
                    lines.push(card_bottom_border(card_width, border));
                    if gap {
                        lines.extend([Line::from(""), Line::from("")]);
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
                        lines.extend([Line::from(""), Line::from("")]);
                    }
                }
                ConversationBlock::CodeBlock(p) => {
                    for line in render_markdown(&p.text, width) {
                        lines.push(line.style(theme::code_block()));
                    }
                    if gap {
                        lines.extend([Line::from(""), Line::from("")]);
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
                        lines.extend([Line::from(""), Line::from("")]);
                    }
                }
                ConversationBlock::PlanChecklist(p) => {
                    lines.extend(render_plan_checklist(&p, width));
                    if gap {
                        lines.extend([Line::from(""), Line::from("")]);
                    }
                }
                ConversationBlock::Metadata(p) => {
                    for l in wrap(&p.text, width) {
                        lines.push(Line::from(Span::styled(l, theme::muted())));
                    }
                    if gap {
                        lines.extend([Line::from(""), Line::from("")]);
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
                    for l in wrap(&full_text, content_width) {
                        lines.push(Line::from(Span::styled(
                            format!("{indent}{l}"),
                            theme::dim().add_modifier(Modifier::ITALIC),
                        )));
                    }
                    if gap {
                        lines.extend([Line::from(""), Line::from("")]);
                    }
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
    pub scroll: u16,
    pub follow: bool,
    pub bottom_padding: u16,
}

pub(super) fn render_conversation_lines(
    lines: &[Line<'static>],
    tail_lines: &[Line<'static>],
    scroll_from_bottom: u16,
    follow: bool,
    bottom_padding: u16,
    area: Rect,
    buf: &mut Buffer,
) {
    theme::fill(area, buf, theme::assistant_message());
    let content_len = lines.len().saturating_add(tail_lines.len());
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
    let visible = (scroll..end)
        .map(|index| {
            if index < lines.len() {
                &lines[index]
            } else if index < content_len {
                &tail_lines[index - lines.len()]
            } else {
                &blank
            }
        })
        .collect::<Vec<_>>();
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
            self.lines,
            self.tail_lines,
            self.scroll,
            self.follow,
            self.bottom_padding,
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
            &lines,
            &[],
            self.model.scroll,
            self.model.follow,
            0,
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

const DIFF_BLOCK_MARKER: &str = "\u{200b}";

const DIFF_BLOCK_END_MARKER: &str = "\u{200c}";

const INDENT_UNIT: &str = "  ";

const MESSAGE_PADDING: usize = 2;

const PROSE_MAX_WIDTH: usize = 72;

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

fn activity_detail_label(expanded: bool) -> &'static str {
    if expanded {
        "  [Ctrl + o] collapse"
    } else {
        "  [Ctrl + o]"
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
        assert!(m
            .items
            .iter()
            .any(|i| matches!(i, ChatItem::ToolCard { .. })));
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
        let semantic = m.semantic_blocks();
        assert!(
            semantic
                .iter()
                .any(|block| matches!(block, ConversationBlock::ActivityGroup(_))),
            "tool result should classify into semantic activity blocks: {semantic:?}"
        );
        assert!(
            rendered.contains("**ponder** · 2.4s"),
            "completed thought should remain visible, without a spelled-out caption:\n{rendered}"
        );
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
        let span = thinking_line
            .spans
            .iter()
            .find(|s| s.content.contains("reasoning text"))
            .expect("thinking span present");
        assert_eq!(
            span.style.fg,
            Some(dark.dim),
            "thinking should use the dim token"
        );
        assert!(
            span.style.add_modifier.contains(Modifier::ITALIC),
            "thinking should be italic"
        );
        assert!(
            !span.style.add_modifier.contains(Modifier::BOLD),
            "thinking should not be bold — no label, unlike tool activity"
        );
        assert!(
            thinking_text.contains("2.4s"),
            "duration should still be shown, got {thinking_text:?}"
        );
    }

    #[test]
    fn wide_viewport_does_not_wrap_at_the_old_column_limit() {
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
        assert_eq!(answer_lines, 2);
    }

    #[test]
    fn active_thinking_is_hidden_from_rendered_lines() {
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
        }];
        let model = ConversationModel::from_messages(
            &msgs,
            &[],
            TaskLifecycle::Working,
            ConversationViewOpts::default(),
        );

        let rendered_text = rendered_text(&model);
        assert!(
            !rendered_text.contains("one two three"),
            "active reasoning must not appear in chat: {rendered_text}"
        );
        // The only visible content should be the empty assistant placeholder (if any).
        assert!(model.items.is_empty() || rendered_text.is_empty());
    }

    #[test]
    fn assistant_output_remains_visible_while_thinking_is_hidden() {
        let msgs = vec![Message {
            outcome: Default::default(),
            role: MessageRole::Assistant,
            content: "ans".into(),
            tool_call_id: None,
            name: None,
            thinking: Some("this is a very long active thinking message that should wrap into multiple lines in the conversation pane".into()),
            thinking_duration_secs: None,
            tool_calls: vec![],
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
        let thought_lines = rendered_lines
            .iter()
            .filter(|line| line.starts_with("⋯ "))
            .count();
        assert_eq!(
            thought_lines,
            0,
            "active reasoning must not produce visible rows, got:\n{}",
            rendered_lines.join("\n")
        );
        assert!(
            rendered_lines.iter().any(|line| line.contains("ans")),
            "assistant output must remain visible, got:\n{}",
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
            ("find", "fffind", serde_json::json!({"query": "*.rs"})),
            ("grep", "ffgrep", serde_json::json!({"pattern": "ToolCard"})),
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
        }];
        let outputs = [
            ("read", "read_file", "pub fn noisy() {\n- old\n+ new\n}"),
            ("bash", "bash", "running tests\nfeature-a\n+ experimental"),
            ("find", "fffind", "src/lib.rs\nsrc/main.rs"),
            (
                "grep",
                "ffgrep",
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
    fn inline_code_in_body_text_uses_secondary_body_color_not_interactive_accent() {
        let lines = render_markdown("plain text with `inline code` in it", 80);
        let code_span = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .find(|span| span.content.as_ref() == "inline")
            .expect("inline code token present");
        assert_eq!(code_span.style.fg, Some(theme::text_secondary_color()));
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
        assert!(text.contains("[Ctrl + o]"), "{text}");
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
        assert!(text.contains("[Ctrl + o] collapse"), "{text}");
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
        assert_eq!(activity_detail_label(true), "  [Ctrl + o] collapse");
        assert_eq!(activity_detail_label(false), "  [Ctrl + o]");
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
                    steps
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

    #[test]
    fn plan_checklist_card_is_bordered_with_no_background_fill() {
        let plan = PlanChecklistPresentation {
            explanation: Some("Next steps".into()),
            steps: vec![forge_types::PlanItem {
                step: "Inspect code".into(),
                status: forge_types::PlanStepStatus::Completed,
            }],
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
            text.contains("```rust"),
            "opening fence should render:\n{text}"
        );
        assert!(
            text.contains("alpha"),
            "partial code should render:\n{text}"
        );
        assert!(text.contains("beta"), "partial code should render:\n{text}");
        assert!(
            !text.contains("  ```\n") && !text.ends_with("  ```"),
            "no closing fence should be invented for an unterminated block:\n{text}"
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
        .with_home("workspace".into(), 2);
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
            },
            vec![
                ApprovalMenuRow {
                    label: "Allow once".into(),
                    detail: None,
                },
                ApprovalMenuRow {
                    label: "Allow pattern going forward".into(),
                    detail: Some("bash(git push *)".into()),
                },
                ApprovalMenuRow {
                    label: "Deny".into(),
                    detail: None,
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
        assert!(text.contains("⏸ APPROVAL REQUIRED"), "{text}");
        assert!(text.contains("git push -u origin feature"), "{text}");
        assert!(text.contains("cwd: workspace"), "{text}");
        assert!(text.contains("env: inherited"), "{text}");
        assert!(text.contains("› Allow once"), "{text}");
        assert!(text.contains("bash(git push *)"), "{text}");
        assert!(
            text.contains("↑↓ select · Enter confirm · Esc cancel"),
            "{text}"
        );
    }

    #[test]
    fn approval_card_hugs_short_content_instead_of_spanning_the_full_pane() {
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
            },
            vec![ApprovalMenuRow {
                label: "Allow once".into(),
                detail: None,
            }],
            0,
            false,
        );
        let lines = m.lines_for_width(PANE_WIDTH);
        let top_border = lines
            .iter()
            .find(|l| line_text(l).starts_with('┌'))
            .expect("top border present");
        let border_width = line_text(top_border).chars().count();
        assert!(
            border_width < PANE_WIDTH,
            "a short command's card should not span the full {PANE_WIDTH}-col pane, got {border_width}: {lines:?}"
        );
        // But every content row and the bottom border must still match the
        // top border's width exactly, or the box wouldn't line up.
        let bottom_border = lines
            .iter()
            .rev()
            .find(|l| line_text(l).starts_with('└'))
            .expect("bottom border present");
        assert_eq!(line_text(bottom_border).chars().count(), border_width);
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
                },
                // A later plan_update in the same turn re-renders the
                // checklist but must not open another phase rule.
                ChatItem::PlanChecklist {
                    explanation: Some("Next steps".into()),
                    steps: plan_items(),
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
}
