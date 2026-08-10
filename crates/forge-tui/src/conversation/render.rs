//! Turning a `ConversationModel` into ratatui lines.
//!
//! Split out of the transcript's domain code: everything here produces
//! `Line<'static>`, and nothing here decides *what* the transcript shows —
//! that is the parent module's job. Keeping the boundary sharp is what lets
//! the projection move to a crate of its own without dragging ratatui along.
//!
//! A child module rather than a sibling on purpose: children can see their
//! ancestors' private items, so the domain helpers used below stay private
//! instead of being widened just to cross a file boundary.

use super::*;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Padding, Widget};

/// Blocks rendered on the turn's tool rail: grouped under the turn, compact,
/// and subordinate to user turns, phases, gates, and the final answer.
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

impl ConversationModel {
    pub fn lines(&self) -> Vec<Line<'static>> {
        self.lines_for_width(if self.opts.compact { 88 } else { 100 })
    }

    /// Build display lines for the actual conversation viewport. Prose gets a
    /// readable cap; code and structured blocks keep the full pane width.
    pub(crate) fn lines_for_width(&self, available_width: usize) -> Vec<Line<'static>> {
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
        // that turn has a plan checklist. Everything on the tool rail stays
        // grouped compactly instead of walled off.
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
