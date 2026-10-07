//! Literal, scrollable details for human decisions. Never collapse command
//! whitespace or elide a path; control characters are shown as escapes.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

/// Named stop and approval dialogs share measured details and pinned choices.
pub(crate) fn choice_surface(
    area: ratatui::layout::Rect,
    buf: &mut ratatui::buffer::Buffer,
    title: &str,
    text: &str,
    scroll: &mut usize,
    choices: [&str; 2],
    selected: usize,
) -> Option<[ratatui::layout::Rect; 2]> {
    use crate::{design::MODAL_PAD_X, overlays::centered_content_rect, theme};
    use ratatui::layout::Rect;
    use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph, Widget};

    let width = centered_content_rect(area, 84, 7, 24)
        .width
        .saturating_sub(2 + 2 * MODAL_PAD_X);
    let lines = literal_lines(text, width, theme::text());
    let r = centered_content_rect(area, 84, lines.len().min(19) as u16 + 5, 24);
    Clear.render(r, buf);
    theme::fill(r, buf, theme::panel());
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::border())
        .style(theme::panel())
        .padding(Padding::horizontal(MODAL_PAD_X))
        .title(theme::modal_title(title));
    let inner = block.inner(r);
    block.render(r, buf);
    if inner.height < 4 {
        return None;
    }
    let body = Rect::new(inner.x, inner.y, inner.width, inner.height - 3);
    *scroll = (*scroll).min(lines.len().saturating_sub(body.height as usize));
    Paragraph::new(
        lines
            .into_iter()
            .skip(*scroll)
            .take(body.height as usize)
            .collect::<Vec<_>>(),
    )
    .render(body, buf);
    let rects = [
        Rect::new(inner.x, inner.bottom() - 3, inner.width, 1),
        Rect::new(inner.x, inner.bottom() - 2, inner.width, 1),
    ];
    for (index, (label, rect)) in choices.into_iter().zip(rects).enumerate() {
        let style = if selected == index {
            theme::focused_selection_style()
        } else if index == 1 {
            theme::warn()
        } else {
            theme::metadata_style()
        };
        if selected == index {
            theme::fill(rect, buf, style);
        }
        buf.set_line(
            rect.x,
            rect.y,
            &Line::styled(
                format!("{} {label}", if selected == index { ">" } else { " " }),
                style,
            ),
            rect.width,
        );
    }
    buf.set_line(
        inner.x,
        inner.bottom() - 1,
        &Line::styled(
            "←→ choose · Enter decide · PgUp/PgDn details · Esc return",
            theme::metadata_style(),
        ),
        inner.width,
    );
    Some(rects)
}

pub(crate) fn visible_text(text: &str) -> String {
    let mut visible = String::new();
    for c in text.chars() {
        if (c.is_control() && c != '\n')
            || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        {
            visible.extend(c.escape_default());
        } else {
            visible.push(c);
        }
    }
    visible
}

pub(crate) fn bounded_text(text: &str, max_bytes: usize) -> String {
    let text = visible_text(text);
    let mut output = String::new();
    for grapheme in Span::raw(&text).styled_graphemes(Style::default()) {
        if output.len() + grapheme.symbol.len() > max_bytes {
            output.push_str("\n[findings truncated]");
            break;
        }
        output.push_str(grapheme.symbol);
    }
    output
}

pub(crate) fn literal_lines(text: &str, width: u16, style: Style) -> Vec<Line<'static>> {
    let text = visible_text(text);
    let mut lines = Vec::new();
    for source in text.split('\n') {
        let span = Span::raw(source);
        let mut row = String::new();
        let mut columns = 0;
        for grapheme in span.styled_graphemes(style) {
            let next = Span::raw(grapheme.symbol).width();
            if columns + next > usize::from(width.max(2)) && !row.is_empty() {
                lines.push(Line::styled(std::mem::take(&mut row), style));
                columns = 0;
            }
            row.push_str(grapheme.symbol);
            columns += next;
        }
        lines.push(Line::styled(row, style));
    }
    lines
}

/// A single-line preview retains whole graphemes and puts the elision in the
/// available cell budget. Full details continue to use literal wrapping.
pub(crate) fn preview(text: &str, width: usize) -> String {
    let text = visible_text(text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let span = Span::raw(text.as_str());
    if span.width() <= width {
        return text;
    }
    if width == 0 {
        return String::new();
    }
    let mut output = String::new();
    let mut used = 0;
    for grapheme in span.styled_graphemes(Style::default()) {
        let cells = Span::raw(grapheme.symbol).width();
        if used + cells > width - 1 {
            break;
        }
        output.push_str(grapheme.symbol);
        used += cells;
    }
    output.push('…');
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_wrapping_retains_spaces_long_tokens_and_unicode_graphemes() {
        let command = "printf  '東京 👩‍💻 e\u{301}'    --aaaaaaaaaaaaaaaaaaaaaaaaaa";
        let lines = literal_lines(command, 12, Style::default());
        let restored: String = lines
            .iter()
            .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
            .collect();
        assert_eq!(restored, command);
        assert!(lines.iter().all(|line| line.width() <= 12));
        assert!(lines.iter().any(|line| line.to_string().contains("👩‍💻")));
    }

    #[test]
    fn previews_keep_whole_graphemes_and_visible_elision() {
        let text = "東京 👩‍💻 e\u{301} long tail";
        for width in 0..20 {
            let clipped = preview(text, width);
            assert!(Span::raw(&clipped).width() <= width);
            assert!(!clipped.contains('👩') || clipped.contains("👩‍💻"));
            assert!(width == 0 || clipped == text || clipped.ends_with('…'));
        }
        assert_eq!(preview("run\u{1b} command", 40), "run\\u{1b} command");
    }

    #[test]
    fn decision_text_shows_control_and_direction_changes_as_escapes() {
        let visible = visible_text("run\tthis\r\u{1b}]52;bad\u{7}\u{202e}\nnext");
        assert!(visible.contains("\\t"));
        assert!(visible.contains("\\r"));
        assert!(visible.contains("\\u{1b}"));
        assert!(visible.contains("\\u{202e}"));
        assert!(!visible.chars().any(|c| c.is_control() && c != '\n'));
        assert!(visible.ends_with("\nnext"));
    }
}
