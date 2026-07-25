//! Syntax highlighted code block widget for ratatui.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Widget};

use forge_syntax::highlight_to_lines;

/// A code block widget with optional syntax highlighting.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct CodeBlock<'a> {
    code: &'a str,
    language: Option<&'a str>,
    block_style: Style,
    show_border: bool,
}

impl<'a> CodeBlock<'a> {
    #[allow(dead_code)]
    pub fn new(code: &'a str) -> Self {
        Self {
            code,
            language: None,
            block_style: Style::default(),
            show_border: true,
        }
    }

    #[allow(dead_code)]
    pub fn language(mut self, lang: &'a str) -> Self {
        self.language = Some(lang);
        self
    }

    #[allow(dead_code)]
    pub fn style(mut self, style: Style) -> Self {
        self.block_style = style;
        self
    }

    #[allow(dead_code)]
    pub fn borderless(mut self) -> Self {
        self.show_border = false;
        self
    }
}

impl Widget for CodeBlock<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let lines = if let Some(lang) = self.language {
            let theme = forge_syntax::HighlightTheme::default();
            let highlighted = highlight_to_lines(lang, self.code, &theme);
            highlighted
                .into_iter()
                .map(|segments| {
                    Line::from(
                        segments
                            .into_iter()
                            .map(|(text, rgb, bold, italic)| {
                                let mut style = Style::default()
                                    .fg(ratatui::style::Color::Rgb(rgb.0, rgb.1, rgb.2));
                                if bold {
                                    style = style.add_modifier(ratatui::style::Modifier::BOLD);
                                }
                                if italic {
                                    style = style.add_modifier(ratatui::style::Modifier::ITALIC);
                                }
                                ratatui::text::Span::styled(text, style)
                            })
                            .collect::<Vec<_>>(),
                    )
                })
                .collect::<Vec<_>>()
        } else {
            self.code.lines().map(Line::raw).collect()
        };

        if self.show_border {
            let block = Block::default()
                .borders(Borders::TOP | Borders::BOTTOM)
                .border_style(self.block_style);
            block.render(area, buf);
        }

        let inner_area = if self.show_border {
            Rect::new(
                area.x + 1,
                area.y,
                area.width.saturating_sub(2),
                area.height,
            )
        } else {
            area
        };

        for (i, line) in lines.iter().enumerate() {
            if i < inner_area.height as usize {
                line.render(
                    Rect::new(inner_area.x, inner_area.y + i as u16, inner_area.width, 1),
                    buf,
                );
            }
        }
    }
}

#[allow(dead_code)]
/// Extract language from a markdown code fence (e.g., ```rust).
pub fn extract_lang_from_fence(fence: &str) -> Option<&str> {
    let trimmed = fence.trim_start_matches("```").trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

#[allow(dead_code)]
/// Parse a markdown code block into owned strings.
pub fn parse_markdown_code_block(markdown: &str) -> Vec<(String, String)> {
    let mut blocks = Vec::new();
    let mut in_block = false;
    let mut current_fence = String::new();
    let mut current_content = String::new();

    for line in markdown.lines() {
        if line.starts_with("```") {
            if in_block {
                blocks.push((current_fence.clone(), current_content.trim().to_string()));
                current_content.clear();
                in_block = false;
            } else {
                current_fence = line.to_string();
                in_block = true;
            }
        } else if in_block {
            if !current_content.is_empty() {
                current_content.push('\n');
            }
            current_content.push_str(line);
        }
    }

    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_fence() {
        assert_eq!(extract_lang_from_fence("```rust"), Some("rust"));
        assert_eq!(extract_lang_from_fence("```"), None);
    }

    #[test]
    fn parse_markdown_blocks() {
        let md =
            "Some text\n```rust\nfn main() {}\n```\nMiddle\n```python\ndef hi():\n    pass\n```";
        let blocks = parse_markdown_code_block(md);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].0, "```rust");
        assert_eq!(blocks[1].0, "```python");
    }
}
