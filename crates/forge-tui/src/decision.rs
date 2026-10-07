//! Literal, scrollable details for human decisions. Never collapse command
//! whitespace or elide a path; control characters are shown as escapes.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

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
