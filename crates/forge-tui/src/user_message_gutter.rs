//! Continuous blue gutter for submitted user messages in the transcript.

use crate::theme;
use forge_config::Theme;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

const PRIMARY_GLYPH: &str = "▎";
const FALLBACK_GLYPH_PIPE: &str = "│";
const FALLBACK_GLYPH_ASCII: &str = "|";
pub const GUTTER_GAP: &str = " ";

/// Decorative gutter glyph for the active theme.
pub fn gutter_glyph(theme: Theme, force_fallback: bool) -> &'static str {
    if force_fallback {
        return gutter_fallback_glyph();
    }
    match theme {
        Theme::Ansi => gutter_fallback_glyph(),
        _ => {
            if glyph_display_width(PRIMARY_GLYPH) == 1 {
                PRIMARY_GLYPH
            } else {
                gutter_fallback_glyph()
            }
        }
    }
}

fn gutter_fallback_glyph() -> &'static str {
    if glyph_display_width(FALLBACK_GLYPH_PIPE) == 1 {
        FALLBACK_GLYPH_PIPE
    } else {
        FALLBACK_GLYPH_ASCII
    }
}

/// Terminal display width of the gutter glyph alone.
pub fn glyph_display_width(glyph: &str) -> usize {
    Span::from(glyph).width()
}

/// Display width of gutter glyph plus separating space.
pub fn gutter_prefix_width(glyph: &str) -> usize {
    glyph_display_width(glyph) + GUTTER_GAP.len()
}

/// Style for the decorative gutter marker.
pub fn gutter_style_for(theme: Theme) -> Style {
    theme::user_message_gutter_style_for(theme)
}

/// Build wrapped visual rows for a submitted user message.
pub fn render_user_message_lines(
    text: &str,
    available_width: usize,
    theme: Theme,
    force_fallback: bool,
    wrap: impl Fn(&str, usize) -> Vec<String>,
) -> Vec<Line<'static>> {
    let glyph = gutter_glyph(theme, force_fallback);
    let prefix_width = gutter_prefix_width(glyph);
    let content_width = available_width.saturating_sub(prefix_width).max(1);
    let parts = wrap(text, content_width);
    let gutter_style = gutter_style_for(theme);
    let text_style = theme::user_message_style();
    let block_style = theme::user_message();

    parts
        .into_iter()
        .map(|content| {
            Line::from(vec![
                Span::styled(glyph, gutter_style),
                Span::styled(GUTTER_GAP, text_style),
                Span::styled(content, text_style),
            ])
            .style(block_style)
        })
        .collect()
}

/// Strip a decorative gutter prefix from one rendered display row.
pub fn strip_rendered_line_prefix<'a>(line: &'a str, glyph: &str) -> &'a str {
    let Some(rest) = line.strip_prefix(glyph) else {
        return line;
    };
    rest.strip_prefix(GUTTER_GAP).unwrap_or(rest)
}

/// Map a display column within a wrapped row to a message-text column.
pub fn display_column_to_content(display_col: usize, glyph: &str) -> usize {
    display_col.saturating_sub(gutter_prefix_width(glyph))
}

/// Map a message-text column to a display column for hit testing.
pub fn content_column_to_display(content_col: usize, glyph: &str) -> usize {
    content_col.saturating_add(gutter_prefix_width(glyph))
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_types::{Message, MessageRole, SessionStatus};
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;
    use ratatui::style::Color;
    use ratatui::Terminal;

    use crate::conversation::{ConversationModel, ConversationViewOpts, ConversationWidget};

    fn user_model(text: &str) -> ConversationModel {
        ConversationModel::from_messages(
            &[Message {
                role: MessageRole::User,
                content: text.into(),
                tool_call_id: None,
                name: None,
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
            }],
            &[],
            SessionStatus::Running,
            ConversationViewOpts::default(),
        )
    }

    fn line_plain(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn rendered_rows(text: &str, width: usize) -> Vec<String> {
        user_model(text)
            .lines_for_width(width)
            .into_iter()
            .map(|line| line_plain(&line))
            .filter(|row| !row.is_empty())
            .collect()
    }

    fn gutter_rows(rows: &[String], glyph: &str) -> usize {
        rows.iter().filter(|row| row.starts_with(glyph)).count()
    }

    fn content_column(rows: &[String], glyph: &str) -> Option<usize> {
        rows.first().map(|row| {
            let stripped = strip_rendered_line_prefix(row, glyph);
            row.len() - stripped.len()
        })
    }

    #[test]
    fn single_line_message_has_one_gutter() {
        let glyph = gutter_glyph(Theme::Dark, false);
        let rows = rendered_rows("Summarize this codebase", 100);
        assert_eq!(rows.len(), 1);
        assert_eq!(gutter_rows(&rows, glyph), 1);
        assert!(rows[0].starts_with(&format!("{glyph} Summarize this codebase")));
    }

    #[test]
    fn wrapped_message_has_gutter_on_every_row() {
        let glyph = gutter_glyph(Theme::Dark, false);
        let text = "Explain how session recovery works and identify any conditions where a persisted turn could remain stuck after the original process exits.";
        let rows = rendered_rows(text, 40);
        assert_eq!(rows.len(), 4, "rows:\n{}", rows.join("\n"));
        assert_eq!(gutter_rows(&rows, glyph), 4);
        let col = content_column(&rows, glyph).expect("column");
        assert!(rows.iter().all(|row| row.len() >= col));
        for row in &rows[1..] {
            assert!(
                row.starts_with(glyph),
                "continuation row missing gutter: {row}"
            );
        }
    }

    #[test]
    fn explicit_newlines_receive_gutters() {
        let glyph = gutter_glyph(Theme::Dark, false);
        let text =
            "Summarize the codebase.\nFocus on:\n- providers\n- session persistence\n- tool safety";
        let rows = rendered_rows(text, 100);
        assert_eq!(rows.len(), 5);
        assert_eq!(gutter_rows(&rows, glyph), 5);
    }

    #[test]
    fn blank_line_retains_gutter() {
        let glyph = gutter_glyph(Theme::Dark, false);
        let text = "First paragraph.\n\nSecond paragraph.";
        let rows = rendered_rows(text, 100);
        assert_eq!(rows.len(), 3, "rows:\n{}", rows.join("\n"));
        assert_eq!(gutter_rows(&rows, glyph), 3);
        assert_eq!(strip_rendered_line_prefix(&rows[1], glyph), "");
    }

    #[test]
    fn source_newline_plus_wrapping_counts_all_rows() {
        let glyph = gutter_glyph(Theme::Dark, false);
        let text = "First paragraph with enough words to wrap across multiple visual rows.\n\nSecond paragraph that also wraps when the terminal is narrow.";
        let rows = rendered_rows(text, 30);
        assert_eq!(gutter_rows(&rows, glyph), rows.len());
        assert!(rows.len() > 4, "expected wrapping, got {}", rows.len());
    }

    #[test]
    fn bullet_list_rows_keep_list_markers() {
        let glyph = gutter_glyph(Theme::Dark, false);
        let text = "- providers\n- session persistence\n- tool safety";
        let rows = rendered_rows(text, 100);
        assert_eq!(gutter_rows(&rows, glyph), 3);
        assert!(rows[1].contains("- session persistence"));
    }

    #[test]
    fn pasted_code_preserves_indentation() {
        let glyph = gutter_glyph(Theme::Dark, false);
        let text = "Please review:\n\nfn main() {\n    println!(\"hello\");\n}";
        let rows = rendered_rows(text, 100);
        assert_eq!(gutter_rows(&rows, glyph), rows.len());
        assert!(rows.iter().any(|row| row.contains("fn main()")));
        assert!(rows.iter().any(|row| row.contains("println!")));
    }

    #[test]
    fn long_unbroken_token_keeps_policy_and_gutters() {
        let glyph = gutter_glyph(Theme::Dark, false);
        let token = "a".repeat(120);
        let rows = rendered_rows(&token, 40);
        assert_eq!(gutter_rows(&rows, glyph), rows.len());
        assert_eq!(rows.len(), 1, "long tokens keep existing no-break policy");
    }

    #[test]
    fn unicode_and_wide_characters_align() {
        let glyph = gutter_glyph(Theme::Dark, false);
        let text = "emoji 🚀 test 日本語 café";
        let rows = rendered_rows(text, 20);
        assert_eq!(gutter_rows(&rows, glyph), rows.len());
        let col = content_column(&rows, glyph).expect("column");
        assert!(rows.iter().all(|row| row.len() >= col));
    }

    #[test]
    fn narrow_terminal_stays_positive_and_aligned() {
        let glyph = gutter_glyph(Theme::Dark, false);
        for width in [80, 40, 20, 8] {
            let rows = rendered_rows("hello world", width);
            assert!(!rows.is_empty(), "width {width}");
            assert_eq!(gutter_rows(&rows, glyph), rows.len(), "width {width}");
            let col = content_column(&rows, glyph).expect("column");
            assert!(col > 0 && col < width, "width {width} col {col}");
        }
        // Extremely narrow widths still render without panicking.
        let rows = rendered_rows("hi", 4);
        assert_eq!(gutter_rows(&rows, glyph), rows.len());
    }

    #[test]
    fn resize_reflow_regenerates_gutters() {
        let glyph = gutter_glyph(Theme::Dark, false);
        let text = "word ".repeat(30);
        let wide = rendered_rows(&text, 120);
        let narrow = rendered_rows(&text, 30);
        let wide_again = rendered_rows(&text, 120);
        assert_eq!(gutter_rows(&wide, glyph), wide.len());
        assert_eq!(gutter_rows(&narrow, glyph), narrow.len());
        assert!(narrow.len() > wide.len());
        assert_eq!(gutter_rows(&wide_again, glyph), wide_again.len());
    }

    #[test]
    fn continuation_rows_keep_gutter_when_first_row_is_absent() {
        let glyph = gutter_glyph(Theme::Dark, false);
        let text = "word ".repeat(30);
        let rows = rendered_rows(&text, 30);
        assert!(rows.len() > 2);
        let tail = &rows[1..];
        assert_eq!(gutter_rows(tail, glyph), tail.len());
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
        assert!(!block.contains(gutter_glyph(Theme::Dark, false)));
    }

    #[test]
    fn partial_copy_strips_gutter_prefix() {
        let glyph = gutter_glyph(Theme::Dark, false);
        let rows = rendered_rows("alpha beta gamma delta", 10);
        let copied: Vec<String> = rows
            .iter()
            .map(|row| strip_rendered_line_prefix(row, glyph).to_string())
            .collect();
        for row in copied {
            assert!(!row.starts_with(glyph));
            assert!(!row.starts_with('▎'));
        }
    }

    #[test]
    fn mouse_selection_translates_past_gutter() {
        let glyph = gutter_glyph(Theme::Dark, false);
        assert_eq!(display_column_to_content(0, glyph), 0);
        let prefix = gutter_prefix_width(glyph);
        assert_eq!(display_column_to_content(prefix, glyph), 0);
        assert_eq!(content_column_to_display(0, glyph), prefix);
        assert_eq!(display_column_to_content(prefix + 3, glyph), 3);
    }

    #[test]
    fn consecutive_user_messages_each_have_gutters() {
        let glyph = gutter_glyph(Theme::Dark, false);
        let model = ConversationModel::from_messages(
            &[
                Message {
                    role: MessageRole::User,
                    content: "first".into(),
                    tool_call_id: None,
                    name: None,
                    thinking: None,
                    thinking_duration_secs: None,
                    tool_calls: vec![],
                },
                Message {
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
            SessionStatus::Running,
            ConversationViewOpts::default(),
        );
        let rows: Vec<String> = model
            .lines_for_width(80)
            .into_iter()
            .map(|line| line_plain(&line))
            .filter(|row| !row.is_empty())
            .collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(gutter_rows(&rows, glyph), 2);
    }

    #[test]
    fn theme_matrix_keeps_gutter_blue_and_text_neutral() {
        for theme in [Theme::Dark, Theme::Light, Theme::System, Theme::Ansi] {
            let lines =
                render_user_message_lines("hello", 40, theme, false, crate::conversation::wrap);
            let gutter_fg = lines[0].spans[0].style.fg;
            let text_fg = lines[0].spans[2].style.fg;
            assert_ne!(
                gutter_fg, text_fg,
                "theme {:?} should separate gutter and text colours",
                theme
            );
            assert!(
                matches!(
                    gutter_fg,
                    Some(Color::Rgb(_, _, _))
                        | Some(Color::Blue)
                        | Some(Color::LightBlue)
                        | Some(Color::Cyan)
                ),
                "theme {:?} gutter fg {:?}",
                theme,
                gutter_fg
            );
        }
    }

    #[test]
    fn forced_fallback_renders_on_all_rows() {
        let glyph = gutter_glyph(Theme::Dark, true);
        assert_ne!(glyph, PRIMARY_GLYPH);
        let rows: Vec<String> = render_user_message_lines(
            "one two three four five six seven",
            12,
            Theme::Dark,
            true,
            crate::conversation::wrap,
        )
        .into_iter()
        .map(|line| line_plain(&line))
        .collect();
        assert_eq!(gutter_rows(&rows, glyph), rows.len());
        let prefix = gutter_prefix_width(glyph);
        assert!(prefix >= 2);
    }

    #[test]
    fn legacy_session_message_receives_gutter_without_content_rewrite() {
        let text = "legacy\nmulti\nline";
        let model = user_model(text);
        let rows = rendered_rows(text, 80);
        assert_eq!(gutter_rows(&rows, gutter_glyph(Theme::Dark, false)), 3);
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
        let glyph = gutter_glyph(Theme::Dark, false);
        let visible = &lines[2..5.min(lines.len())];
        assert!(!visible.is_empty());
        assert_eq!(
            visible.iter().filter(|row| row.starts_with(glyph)).count(),
            visible.len()
        );
    }

    #[test]
    fn snapshot_consecutive_user_messages() {
        let model = ConversationModel::from_messages(
            &[
                Message {
                    role: MessageRole::User,
                    content: "first message".into(),
                    tool_call_id: None,
                    name: None,
                    thinking: None,
                    thinking_duration_secs: None,
                    tool_calls: vec![],
                },
                Message {
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
            SessionStatus::Running,
            ConversationViewOpts::default(),
        );
        snapshot_model(&model, 100, "consecutive_users");
    }

    #[test]
    fn snapshot_dark_theme_gutter_colour() {
        let lines =
            render_user_message_lines("hello", 40, Theme::Dark, false, crate::conversation::wrap);
        assert_eq!(
            lines[0].spans[0].style.fg,
            Some(theme::USER_MESSAGE_GUTTER_DARK)
        );
    }

    #[test]
    fn snapshot_light_theme_gutter_colour() {
        let lines =
            render_user_message_lines("hello", 40, Theme::Light, false, crate::conversation::wrap);
        assert_eq!(
            lines[0].spans[0].style.fg,
            Some(theme::USER_MESSAGE_GUTTER_LIGHT)
        );
    }

    #[test]
    fn snapshot_system_theme_gutter_colour() {
        let lines =
            render_user_message_lines("hello", 40, Theme::System, false, crate::conversation::wrap);
        assert_eq!(lines[0].spans[0].style.fg, Some(Color::Blue));
    }

    #[test]
    fn snapshot_ansi_fallback_gutter() {
        let glyph = gutter_glyph(Theme::Ansi, false);
        let lines =
            render_user_message_lines("hello", 40, Theme::Ansi, false, crate::conversation::wrap);
        assert_eq!(lines[0].spans[0].content, glyph);
        assert_ne!(glyph, PRIMARY_GLYPH);
    }

    fn snapshot_lines(text: &str, width: usize, label: &str) {
        let model = user_model(text);
        snapshot_model(&model, width, label);
    }

    fn snapshot_model(model: &ConversationModel, width: usize, label: &str) {
        let glyph = gutter_glyph(Theme::Dark, false);
        let lines = model.lines_for_width(width);
        assert!(
            lines
                .iter()
                .filter(|line| !line_plain(line).is_empty())
                .all(|line| line_plain(line).starts_with(glyph)),
            "{label}: missing gutter:\n{}",
            lines
                .iter()
                .map(|line| line_plain(line))
                .collect::<Vec<_>>()
                .join("\n")
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
        assert!(
            rendered.contains(glyph),
            "{label}: buffer missing gutter:\n{rendered}"
        );
    }
}
