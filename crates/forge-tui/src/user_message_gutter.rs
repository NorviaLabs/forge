//! Submitted user-message rendering for the conversation transcript.
//!
//! Historically this module also owned a decorative leading glyph (a
//! continuous "rail" repeated on every wrapped row of a submitted message)
//! and the column-mapping helpers needed to skip over it for copy/paste and
//! mouse selection. Both the composer and the transcript have since dropped
//! that marker entirely — see `widgets/input.rs` (`TEXT_INSET`) and
//! `conversation.rs` (`ConversationBlock::UserMessage` render arm) — so only
//! the actual text-wrapping logic remains here.

use crate::links::{autolink_matches, link_label_style, HyperlinkLine, TerminalHyperlink};
use crate::markdown::display_width;
use crate::theme;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

/// Build wrapped visual rows for a submitted user message.
///
/// A prompt is reproduced as typed — newlines, list markers and indentation all
/// survive — so this is deliberately not a markdown path. A URL pasted into it
/// is still a URL the reader may want to open, though, so every row is scanned
/// with the same gate the transcript uses and a bare `http`/`https` URL carries
/// its destination beside its columns.
pub fn render_user_message_lines(
    text: &str,
    available_width: usize,
    _theme: &str,
    _force_fallback: bool,
    wrap: impl Fn(&str, usize) -> Vec<String>,
) -> Vec<HyperlinkLine> {
    let parts = wrap(text, available_width.max(1));
    let text_style = theme::user_message_style();

    parts
        .into_iter()
        .map(|content| prompt_row(&content, text_style))
        .collect()
}

/// One wrapped row of a prompt, with the columns of any URL it holds.
///
/// Columns are counted from the row's own left edge; the caller offsets them by
/// whatever indent it adds, exactly as the answer renderer does with its own
/// message padding.
fn prompt_row(content: &str, text_style: Style) -> HyperlinkLine {
    let matches = autolink_matches(content);
    if matches.is_empty() {
        return HyperlinkLine::new(Line::from(Span::styled(content.to_string(), text_style)));
    }
    let mut spans = Vec::new();
    let mut links = Vec::new();
    let mut cursor = 0usize;
    let mut column = 0usize;
    for (range, destination) in matches {
        if range.start > cursor {
            let plain = &content[cursor..range.start];
            column += display_width(plain);
            spans.push(Span::styled(plain.to_string(), text_style));
        }
        let label = &content[range.clone()];
        let width = display_width(label);
        links.push(TerminalHyperlink::new(column..column + width, destination));
        spans.push(Span::styled(
            label.to_string(),
            text_style.patch(link_label_style()),
        ));
        column += width;
        cursor = range.end;
    }
    if cursor < content.len() {
        spans.push(Span::styled(content[cursor..].to_string(), text_style));
    }
    HyperlinkLine::with_links(Line::from(spans), links)
}
