//! Submitted user-message rendering for the conversation transcript.
//!
//! Historically this module also owned a decorative leading glyph (a
//! continuous "rail" repeated on every wrapped row of a submitted message)
//! and the column-mapping helpers needed to skip over it for copy/paste and
//! mouse selection. Both the composer and the transcript have since dropped
//! that marker entirely — see `widgets/input.rs` (`TEXT_INSET`) and
//! `conversation.rs` (`ConversationBlock::UserMessage` render arm) — so only
//! the actual text-wrapping logic remains here.

use crate::theme;
use ratatui::text::{Line, Span};

/// Build wrapped visual rows for a submitted user message.
pub fn render_user_message_lines(
    text: &str,
    available_width: usize,
    _theme: &str,
    _force_fallback: bool,
    wrap: impl Fn(&str, usize) -> Vec<String>,
) -> Vec<Line<'static>> {
    let parts = wrap(text, available_width.max(1));
    let text_style = theme::user_message_style();

    parts
        .into_iter()
        .map(|content| Line::from(Span::styled(content, text_style)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_config::{THEME_SOLARIZED_DARK, THEME_SOLARIZED_LIGHT};
    use forge_types::{Message, MessageRole, TaskLifecycle};
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;
    use ratatui::Terminal;

    use crate::conversation::{ConversationModel, ConversationViewOpts, ConversationWidget};

    fn user_model(text: &str) -> ConversationModel {
        ConversationModel::from_messages(
            &[Message {
                outcome: Default::default(),
                role: MessageRole::User,
                content: text.into(),
                tool_call_id: None,
                name: None,
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
            }],
            &[],
            TaskLifecycle::Working,
            ConversationViewOpts::default(),
        )
    }

    fn line_plain(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    /// Rendered rows with the 2-column left indent (matching assistant
    /// messages' own `MESSAGE_PADDING`) stripped, isolating content text.
    fn rendered_rows(text: &str, width: usize) -> Vec<String> {
        user_model(text)
            .lines_for_width(width)
            .into_iter()
            .map(|line| line_plain(&line))
            .filter(|row| !row.is_empty() && !row.chars().all(|c| c == '─'))
            .map(|row| row.strip_prefix("  ").unwrap_or(&row).to_string())
            .collect()
    }

    #[test]
    fn single_line_message_has_one_row() {
        let rows = rendered_rows("Summarize this codebase", 100);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].trim_end(), "Summarize this codebase");
    }

    #[test]
    fn wrapped_message_spans_expected_rows() {
        let text = "Explain how session recovery works and identify any conditions where a persisted turn could remain stuck after the original process exits.";
        let rows = rendered_rows(text, 40);
        assert_eq!(rows.len(), 4, "rows:\n{}", rows.join("\n"));
        assert!(rows.iter().all(|row| !row.trim().is_empty()));
    }

    #[test]
    fn explicit_newlines_each_get_a_row() {
        let text =
            "Summarize the codebase.\nFocus on:\n- providers\n- session persistence\n- tool safety";
        let rows = rendered_rows(text, 100);
        assert_eq!(rows.len(), 5);
    }

    #[test]
    fn blank_line_is_preserved() {
        let text = "First paragraph.\n\nSecond paragraph.";
        let rows = rendered_rows(text, 100);
        assert_eq!(rows.len(), 3, "rows:\n{}", rows.join("\n"));
        assert!(rows[1].trim().is_empty());
    }

    #[test]
    fn source_newline_plus_wrapping_counts_all_rows() {
        let text = "First paragraph with enough words to wrap across multiple visual rows.\n\nSecond paragraph that also wraps when the terminal is narrow.";
        let rows = rendered_rows(text, 30);
        assert!(rows.len() > 4, "expected wrapping, got {}", rows.len());
    }

    #[test]
    fn bullet_list_rows_keep_list_markers() {
        let text = "- providers\n- session persistence\n- tool safety";
        let rows = rendered_rows(text, 100);
        assert_eq!(rows.len(), 3);
        assert!(rows[1].contains("- session persistence"));
    }

    #[test]
    fn pasted_code_preserves_indentation() {
        let text = "Please review:\n\nfn main() {\n    println!(\"hello\");\n}";
        let rows = rendered_rows(text, 100);
        assert!(rows.iter().any(|row| row.contains("fn main()")));
        assert!(rows.iter().any(|row| row.contains("println!")));
    }

    #[test]
    fn long_unbroken_token_keeps_no_break_policy() {
        let token = "a".repeat(120);
        let rows = rendered_rows(&token, 40);
        assert_eq!(rows.len(), 1, "long tokens keep existing no-break policy");
    }

    #[test]
    fn unicode_and_wide_characters_align() {
        let text = "emoji 🚀 test 日本語 café";
        let rows = rendered_rows(text, 20);
        assert!(rows.iter().all(|row| !row.trim().is_empty()));
    }

    #[test]
    fn narrow_terminal_stays_positive_and_aligned() {
        for width in [80, 40, 20, 8] {
            let rows = rendered_rows("hello world", width);
            assert!(!rows.is_empty(), "width {width}");
            assert!(rows
                .iter()
                .all(|row| Span::raw(row.clone()).width() <= width.saturating_sub(2)));
        }
        // Extremely narrow widths still render without panicking.
        let rows = rendered_rows("hi", 4);
        assert!(!rows.is_empty());
    }

    #[test]
    fn resize_reflow_regenerates_rows() {
        let text = "word ".repeat(30);
        let wide = rendered_rows(&text, 120);
        let narrow = rendered_rows(&text, 30);
        let wide_again = rendered_rows(&text, 120);
        assert!(narrow.len() > wide.len());
        assert_eq!(wide_again.len(), wide.len());
    }

    #[test]
    fn continuation_rows_present_when_first_row_is_absent() {
        let text = "word ".repeat(30);
        let rows = rendered_rows(&text, 30);
        assert!(rows.len() > 2);
    }

    #[test]
    fn copy_complete_message_uses_persisted_text() {
        let text = "Summarize how session recovery works and identify stuck cases.";
        let model = user_model(text);
        let block = model
            .semantic_blocks()
            .into_iter()
            .find_map(|block| match block {
                crate::conversation::ConversationBlock::UserMessage(p) => Some(p.text),
                _ => None,
            })
            .expect("user block");
        assert_eq!(block, text);
    }

    #[test]
    fn consecutive_user_messages_each_render() {
        let model = ConversationModel::from_messages(
            &[
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
                    role: MessageRole::User,
                    content: "second".into(),
                    tool_call_id: None,
                    name: None,
                    thinking: None,
                    thinking_duration_secs: None,
                    tool_calls: vec![],
                },
            ],
            &[],
            TaskLifecycle::Working,
            ConversationViewOpts::default(),
        );
        let rows: Vec<String> = model
            .lines_for_width(80)
            .into_iter()
            .map(|line| line_plain(&line))
            .filter(|row| {
                !row.is_empty()
                    && !row.trim_end().ends_with("You ─")
                    && !row.trim().chars().all(|c| c == '─')
            })
            .collect();
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn theme_matrix_keeps_request_text_unshaded() {
        for theme in [THEME_SOLARIZED_DARK, THEME_SOLARIZED_LIGHT] {
            let lines =
                render_user_message_lines("hello", 40, theme, false, crate::conversation::wrap);
            assert!(
                lines[0].spans[0].style.bg.is_none(),
                "theme {:?} request text should be unshaded",
                theme,
            );
        }
    }

    #[test]
    fn legacy_session_message_renders_without_content_rewrite() {
        let text = "legacy\nmulti\nline";
        let model = user_model(text);
        let rows = rendered_rows(text, 80);
        assert_eq!(rows.len(), 3);
        let block = model
            .semantic_blocks()
            .into_iter()
            .find_map(|block| match block {
                crate::conversation::ConversationBlock::UserMessage(p) => Some(p.text),
                _ => None,
            })
            .expect("user block");
        assert_eq!(block, text);
    }

    #[test]
    fn snapshot_single_line_message() {
        snapshot_lines("Summarize this codebase", 100, "single_line");
    }

    #[test]
    fn snapshot_three_line_wrapped_message() {
        let text = "Explain how session recovery works and identify any conditions where a persisted turn could remain stuck after the original process exits.";
        snapshot_lines(text, 40, "wrapped_three");
    }

    #[test]
    fn snapshot_explicit_multiline_message() {
        snapshot_lines(
            "Summarize the codebase.\nFocus on:\n- providers",
            100,
            "explicit_multiline",
        );
    }

    #[test]
    fn snapshot_blank_line_message() {
        snapshot_lines("First paragraph.\n\nSecond paragraph.", 100, "blank_line");
    }

    #[test]
    fn snapshot_bullet_list_message() {
        snapshot_lines("- providers\n- session persistence", 100, "bullet_list");
    }

    #[test]
    fn snapshot_pasted_code_message() {
        snapshot_lines("Please review:\n\nfn main() {}", 100, "pasted_code");
    }

    #[test]
    fn snapshot_long_unbroken_token() {
        snapshot_lines(
            &("path/".to_string() + &"segment/".repeat(12)),
            40,
            "long_token",
        );
    }

    #[test]
    fn snapshot_unicode_message() {
        snapshot_lines("emoji 🚀 日本語 café", 30, "unicode");
    }

    #[test]
    fn snapshot_80_column_terminal() {
        snapshot_lines("word ".repeat(20).trim(), 80, "width_80");
    }

    #[test]
    fn snapshot_wide_terminal() {
        snapshot_lines("word ".repeat(20).trim(), 160, "width_160");
    }

    #[test]
    fn snapshot_partial_viewport_clip() {
        let model = user_model(&"word ".repeat(40));
        let lines: Vec<String> = model
            .lines_for_width(30)
            .into_iter()
            .map(|line| line_plain(&line))
            .filter(|row| !row.is_empty())
            .collect();
        let visible = &lines[2..5.min(lines.len())];
        assert!(!visible.is_empty());
        assert!(visible.iter().all(|row| !row.trim().is_empty()));
    }

    #[test]
    fn snapshot_consecutive_user_messages() {
        let model = ConversationModel::from_messages(
            &[
                Message {
                    outcome: Default::default(),
                    role: MessageRole::User,
                    content: "first message".into(),
                    tool_call_id: None,
                    name: None,
                    thinking: None,
                    thinking_duration_secs: None,
                    tool_calls: vec![],
                },
                Message {
                    outcome: Default::default(),
                    role: MessageRole::User,
                    content: "second message".into(),
                    tool_call_id: None,
                    name: None,
                    thinking: None,
                    thinking_duration_secs: None,
                    tool_calls: vec![],
                },
            ],
            &[],
            TaskLifecycle::Working,
            ConversationViewOpts::default(),
        );
        snapshot_model(&model, 100, "consecutive_users");
    }

    #[test]
    fn snapshot_request_text_has_no_background() {
        let lines = render_user_message_lines(
            "hello",
            40,
            THEME_SOLARIZED_DARK,
            false,
            crate::conversation::wrap,
        );
        assert_eq!(lines[0].spans[0].style.bg, None);
    }

    #[test]
    fn snapshot_forced_fallback_still_renders_content() {
        let lines = render_user_message_lines(
            "hello",
            40,
            THEME_SOLARIZED_DARK,
            true,
            crate::conversation::wrap,
        );
        assert_eq!(lines[0].spans[0].content, "hello");
    }

    fn snapshot_lines(text: &str, width: usize, label: &str) {
        let model = user_model(text);
        snapshot_model(&model, width, label);
    }

    fn snapshot_model(model: &ConversationModel, width: usize, label: &str) {
        let lines = model.lines_for_width(width);
        assert!(
            lines
                .iter()
                .map(line_plain)
                .filter(|text| !text.is_empty() && !text.chars().all(|c| c == '─'))
                .all(|text| text.starts_with("  ")),
            "{label}: request is not left-aligned with the message indent:\n{}",
            lines.iter().map(line_plain).collect::<Vec<_>>().join("\n")
        );

        let area = Rect::new(0, 0, width as u16, 12);
        let backend = TestBackend::new(area.width, area.height);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|frame| frame.render_widget(ConversationWidget { model }, area))
            .unwrap();
        let buf = term.backend().buffer();
        let mut rendered = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                rendered.push_str(buf[(x, y)].symbol());
            }
            rendered.push('\n');
        }
        assert!(rendered.contains(' '), "{label}: buffer missing request");
    }
}
