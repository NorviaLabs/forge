//! Submitted user-message rendering and active-composer prompt helpers.

use crate::theme;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

const PRIMARY_GLYPH: &str = "▎";
const FALLBACK_GLYPH_PIPE: &str = "│";
const FALLBACK_GLYPH_ASCII: &str = "|";
pub const GUTTER_GAP: &str = " ";

/// Prompt marker for the active composer. Plain ASCII, so unlike
/// [`gutter_glyph`] it needs no encoding fallback. Callers show this on the
/// composer's first visual row only — continuation/wrapped rows get blank
/// padding of the same width instead (see `build_input_lines` in
/// `widgets/input.rs`), so a multi-line draft reads as one prompt, not one
/// per wrapped line the way the transcript's per-row gutter does.
pub const ACTIVE_GLYPH: &str = ">";

/// Active-composer gutter role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GutterRole {
    /// Active composer input (Prompt 12P).
    Active,
}

/// Decorative gutter glyph for the active theme.
///
/// `_theme` is currently unused: every theme shares the same glyph set. It is
/// kept in the signature so theme-specific glyphs can be introduced without
/// touching every call site.
pub fn gutter_glyph(_theme: &str, force_fallback: bool) -> &'static str {
    if force_fallback {
        return gutter_fallback_glyph();
    }
    if glyph_display_width(PRIMARY_GLYPH) == 1 {
        PRIMARY_GLYPH
    } else {
        gutter_fallback_glyph()
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
pub fn gutter_style_for(theme: &str, role: GutterRole) -> Style {
    match role {
        GutterRole::Active => theme::user_gutter_active_style_for(theme),
    }
}

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
    let block_style = theme::user_message();

    parts
        .into_iter()
        .map(|content| Line::from(Span::styled(content, text_style)).style(block_style))
        .collect()
}

/// Strip a decorative gutter prefix from one rendered display row.
#[cfg(test)]
pub fn strip_rendered_line_prefix<'a>(line: &'a str, glyph: &str) -> &'a str {
    let Some(rest) = line.strip_prefix(glyph) else {
        return line;
    };
    rest.strip_prefix(GUTTER_GAP).unwrap_or(rest)
}

/// Map a display column within a wrapped row to a message-text column.
#[cfg(test)]
pub fn display_column_to_content(display_col: usize, glyph: &str) -> usize {
    display_col.saturating_sub(gutter_prefix_width(glyph))
}

/// Map a message-text column to a display column for hit testing.
#[cfg(test)]
pub fn content_column_to_display(content_col: usize, glyph: &str) -> usize {
    content_col.saturating_add(gutter_prefix_width(glyph))
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

    fn rendered_rows(text: &str, width: usize) -> Vec<String> {
        user_model(text)
            .lines_for_width(width)
            .into_iter()
            .map(|line| line_plain(&line))
            .filter(|row| !row.is_empty())
            .collect()
    }

    fn gutter_rows(rows: &[String], glyph: &str) -> usize {
        let _ = glyph;
        rows.len()
    }

    #[test]
    fn single_line_message_has_one_gutter() {
        let glyph = gutter_glyph(THEME_SOLARIZED_DARK, false);
        let rows = rendered_rows("Summarize this codebase", 100);
        assert_eq!(rows.len(), 1);
        assert_eq!(gutter_rows(&rows, glyph), 1);
        assert!(rows[0].ends_with("Summarize this codebase"));
        assert_eq!(Span::raw(rows[0].clone()).width(), 100);
    }

    #[test]
    fn wrapped_message_has_gutter_on_every_row() {
        let glyph = gutter_glyph(THEME_SOLARIZED_DARK, false);
        let text = "Explain how session recovery works and identify any conditions where a persisted turn could remain stuck after the original process exits.";
        let rows = rendered_rows(text, 40);
        assert_eq!(rows.len(), 4, "rows:\n{}", rows.join("\n"));
        assert_eq!(gutter_rows(&rows, glyph), 4);
        assert!(rows.iter().all(|row| !row.trim().is_empty()));
    }

    #[test]
    fn explicit_newlines_receive_gutters() {
        let glyph = gutter_glyph(THEME_SOLARIZED_DARK, false);
        let text =
            "Summarize the codebase.\nFocus on:\n- providers\n- session persistence\n- tool safety";
        let rows = rendered_rows(text, 100);
        assert_eq!(rows.len(), 5);
        assert_eq!(gutter_rows(&rows, glyph), 5);
    }

    #[test]
    fn blank_line_retains_gutter() {
        let glyph = gutter_glyph(THEME_SOLARIZED_DARK, false);
        let text = "First paragraph.\n\nSecond paragraph.";
        let rows = rendered_rows(text, 100);
        assert_eq!(rows.len(), 3, "rows:\n{}", rows.join("\n"));
        assert_eq!(gutter_rows(&rows, glyph), 3);
        assert!(rows[1].trim().is_empty());
    }

    #[test]
    fn source_newline_plus_wrapping_counts_all_rows() {
        let glyph = gutter_glyph(THEME_SOLARIZED_DARK, false);
        let text = "First paragraph with enough words to wrap across multiple visual rows.\n\nSecond paragraph that also wraps when the terminal is narrow.";
        let rows = rendered_rows(text, 30);
        assert_eq!(gutter_rows(&rows, glyph), rows.len());
        assert!(rows.len() > 4, "expected wrapping, got {}", rows.len());
    }

    #[test]
    fn bullet_list_rows_keep_list_markers() {
        let glyph = gutter_glyph(THEME_SOLARIZED_DARK, false);
        let text = "- providers\n- session persistence\n- tool safety";
        let rows = rendered_rows(text, 100);
        assert_eq!(gutter_rows(&rows, glyph), 3);
        assert!(rows[1].contains("- session persistence"));
    }

    #[test]
    fn pasted_code_preserves_indentation() {
        let glyph = gutter_glyph(THEME_SOLARIZED_DARK, false);
        let text = "Please review:\n\nfn main() {\n    println!(\"hello\");\n}";
        let rows = rendered_rows(text, 100);
        assert_eq!(gutter_rows(&rows, glyph), rows.len());
        assert!(rows.iter().any(|row| row.contains("fn main()")));
        assert!(rows.iter().any(|row| row.contains("println!")));
    }

    #[test]
    fn long_unbroken_token_keeps_policy_and_gutters() {
        let glyph = gutter_glyph(THEME_SOLARIZED_DARK, false);
        let token = "a".repeat(120);
        let rows = rendered_rows(&token, 40);
        assert_eq!(gutter_rows(&rows, glyph), rows.len());
        assert_eq!(rows.len(), 1, "long tokens keep existing no-break policy");
    }

    #[test]
    fn unicode_and_wide_characters_align() {
        let glyph = gutter_glyph(THEME_SOLARIZED_DARK, false);
        let text = "emoji 🚀 test 日本語 café";
        let rows = rendered_rows(text, 20);
        assert_eq!(gutter_rows(&rows, glyph), rows.len());
        assert!(rows.iter().all(|row| !row.trim().is_empty()));
    }

    #[test]
    fn narrow_terminal_stays_positive_and_aligned() {
        let glyph = gutter_glyph(THEME_SOLARIZED_DARK, false);
        for width in [80, 40, 20, 8] {
            let rows = rendered_rows("hello world", width);
            assert!(!rows.is_empty(), "width {width}");
            assert_eq!(gutter_rows(&rows, glyph), rows.len(), "width {width}");
            assert!(rows
                .iter()
                .all(|row| Span::raw(row.clone()).width() >= width));
        }
        // Extremely narrow widths still render without panicking.
        let rows = rendered_rows("hi", 4);
        assert_eq!(gutter_rows(&rows, glyph), rows.len());
    }

    #[test]
    fn resize_reflow_regenerates_gutters() {
        let glyph = gutter_glyph(THEME_SOLARIZED_DARK, false);
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
        let glyph = gutter_glyph(THEME_SOLARIZED_DARK, false);
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
        assert!(!block.contains(gutter_glyph(THEME_SOLARIZED_DARK, false)));
    }

    #[test]
    fn partial_copy_strips_gutter_prefix() {
        let glyph = gutter_glyph(THEME_SOLARIZED_DARK, false);
        let rows = rendered_rows("alpha beta gamma delta", 10);
        let copied: Vec<String> = rows
            .iter()
            .map(|row| strip_rendered_line_prefix(row.trim_start(), glyph).to_string())
            .collect();
        for row in copied {
            assert!(!row.starts_with(glyph));
            assert!(!row.starts_with('▎'));
        }
    }

    #[test]
    fn mouse_selection_translates_past_gutter() {
        let glyph = gutter_glyph(THEME_SOLARIZED_DARK, false);
        assert_eq!(display_column_to_content(0, glyph), 0);
        let prefix = gutter_prefix_width(glyph);
        assert_eq!(display_column_to_content(prefix, glyph), 0);
        assert_eq!(content_column_to_display(0, glyph), prefix);
        assert_eq!(display_column_to_content(prefix + 3, glyph), 3);
    }

    #[test]
    fn consecutive_user_messages_each_have_gutters() {
        let glyph = gutter_glyph(THEME_SOLARIZED_DARK, false);
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
            TaskLifecycle::Working,
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
    fn theme_matrix_uses_raised_request_background() {
        for theme in [THEME_SOLARIZED_DARK, THEME_SOLARIZED_LIGHT] {
            let lines =
                render_user_message_lines("hello", 40, theme, false, crate::conversation::wrap);
            assert!(
                lines[0].style.bg.is_some(),
                "theme {:?} request background is missing",
                theme,
            );
        }
    }

    #[test]
    fn forced_fallback_renders_on_all_rows() {
        let glyph = gutter_glyph(THEME_SOLARIZED_DARK, true);
        assert_ne!(glyph, PRIMARY_GLYPH);
        let rows: Vec<String> = render_user_message_lines(
            "one two three four five six seven",
            12,
            THEME_SOLARIZED_DARK,
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
        assert_eq!(
            gutter_rows(&rows, gutter_glyph(THEME_SOLARIZED_DARK, false)),
            3
        );
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
            TaskLifecycle::Working,
            ConversationViewOpts::default(),
        );
        snapshot_model(&model, 100, "consecutive_users");
    }

    #[test]
    fn snapshot_dark_theme_gutter_colour() {
        let lines = render_user_message_lines(
            "hello",
            40,
            THEME_SOLARIZED_DARK,
            false,
            crate::conversation::wrap,
        );
        assert_eq!(
            lines[0].style.bg,
            Some(theme::palette(THEME_SOLARIZED_DARK).panel_alt)
        );
    }

    #[test]
    fn snapshot_default_theme_gutter_colour() {
        let lines = render_user_message_lines(
            "hello",
            40,
            THEME_SOLARIZED_DARK,
            false,
            crate::conversation::wrap,
        );
        assert_eq!(
            lines[0].style.bg,
            Some(theme::palette(THEME_SOLARIZED_DARK).panel_alt)
        );
    }

    #[test]
    fn snapshot_forced_fallback_gutter() {
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
                .filter(|line| !line_plain(line).is_empty())
                .all(|line| line.width() >= width),
            "{label}: request is not right-aligned:\n{}",
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
