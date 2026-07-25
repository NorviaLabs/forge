//! Tree-sitter based syntax highlighting.

use std::ops::Range;
use crate::lang::{get_parser, SyntaxLanguage};

#[derive(Debug, Clone)]
pub struct HighlightSpan {
    pub range: Range<usize>,
    pub style: HighlightStyle,
}

impl HighlightSpan {
    pub fn text<'a>(&self, source: &'a str) -> &'a str {
        &source[self.range.clone()]
    }
}

#[derive(Debug, Clone, Copy)]
pub struct HighlightStyle {
    pub class: HighlightClass,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HighlightClass {
    Comment, Keyword, String, Number, Function, Type,
    Variable, Operator, Punctuation, Property, Tag, Attribute, Default,
}

#[derive(Debug, Clone, Copy)]
pub struct HighlightTheme {
    pub comment: (u8, u8, u8),
    pub keyword: (u8, u8, u8),
    pub string: (u8, u8, u8),
    pub number: (u8, u8, u8),
    pub function: (u8, u8, u8),
    pub type_: (u8, u8, u8),
    pub variable: (u8, u8, u8),
    pub operator: (u8, u8, u8),
    pub punctuation: (u8, u8, u8),
    pub property: (u8, u8, u8),
    pub tag: (u8, u8, u8),
    pub attribute: (u8, u8, u8),
    pub default: (u8, u8, u8),
}

impl HighlightStyle {
    pub fn rgb(self) -> (u8, u8, u8) {
        match self.class {
            HighlightClass::Comment => (139, 155, 176),
            HighlightClass::Keyword => (61, 214, 198),
            HighlightClass::String => (227, 179, 65),
            HighlightClass::Number => (110, 168, 254),
            HighlightClass::Function => (210, 168, 255),
            HighlightClass::Type => (110, 168, 254),
            HighlightClass::Variable => (230, 237, 243),
            HighlightClass::Operator => (139, 155, 176),
            HighlightClass::Punctuation => (139, 155, 176),
            HighlightClass::Property => (61, 214, 198),
            HighlightClass::Tag => (61, 214, 198),
            HighlightClass::Attribute => (227, 179, 65),
            HighlightClass::Default => (230, 237, 243),
        }
    }

    pub fn is_bold(self) -> bool {
        matches!(self.class, HighlightClass::Keyword | HighlightClass::Number | HighlightClass::Type)
    }

    pub fn is_italic(self) -> bool {
        matches!(self.class, HighlightClass::Comment)
    }
}

impl Default for HighlightTheme {
    fn default() -> Self {
        Self {
            comment: (139, 155, 176),
            keyword: (61, 214, 198),
            string: (227, 179, 65),
            number: (110, 168, 254),
            function: (210, 168, 255),
            type_: (110, 168, 254),
            variable: (230, 237, 243),
            operator: (139, 155, 176),
            punctuation: (139, 155, 176),
            property: (61, 214, 198),
            tag: (61, 214, 198),
            attribute: (227, 179, 65),
            default: (230, 237, 243),
        }
    }
}

impl HighlightTheme {
    fn style_for_class(&self, class: HighlightClass) -> HighlightStyle {
        HighlightStyle { class }
    }
}

pub fn highlight(lang: &str, code: &str, theme: &HighlightTheme) -> Vec<HighlightSpan> {
    let lang: SyntaxLanguage = match lang.parse() {
        Ok(l) => l,
        Err(_) => return vec![HighlightSpan { range: 0..code.len(), style: HighlightStyle { class: HighlightClass::Default } }],
    };

    if lang == SyntaxLanguage::Unknown {
        return vec![HighlightSpan { range: 0..code.len(), style: HighlightStyle { class: HighlightClass::Default } }];
    }

    let mut parser = get_parser(lang);
    let tree = match parser.parse(code, None) {
        Some(t) => t,
        None => return vec![HighlightSpan { range: 0..code.len(), style: HighlightStyle { class: HighlightClass::Default } }],
    };

    let mut spans = Vec::new();
    let mut cursor = tree.walk();
    collect_highlights(&mut cursor, theme, &mut spans);

    if spans.is_empty() {
        spans.push(HighlightSpan { range: 0..code.len(), style: HighlightStyle { class: HighlightClass::Default } });
    }

    spans.sort_by_key(|s| s.range.start);
    merge_spans(spans)
}

fn collect_highlights(cursor: &mut tree_sitter::TreeCursor, theme: &HighlightTheme, spans: &mut Vec<HighlightSpan>) {
    let node = cursor.node();
    let kind = node.kind();

    let style = match kind {
        "comment" | "line_comment" | "block_comment" | "documentation_comment" => Some(theme.style_for_class(HighlightClass::Comment)),
        "attribute" | "decorator" => Some(theme.style_for_class(HighlightClass::Keyword)),
        "string" | "string_literal" | "char_literal" | "interpreted_string_literal" => Some(theme.style_for_class(HighlightClass::String)),
        "integer" | "float" | "hex_integer" | "octal_integer" | "binary_integer" => Some(theme.style_for_class(HighlightClass::Number)),
        "function_declaration" | "function_item" | "method_declaration" | "method_definition" | "function_signature" => Some(theme.style_for_class(HighlightClass::Function)),
        "type_identifier" | "primitive_type" | "builtin_type" => Some(theme.style_for_class(HighlightClass::Type)),
        "binary_expression" | "unary_expression" => Some(theme.style_for_class(HighlightClass::Operator)),
        "{" | "}" | "(" | ")" | "[" | "]" | "," | ";" | ":" => Some(theme.style_for_class(HighlightClass::Punctuation)),
        _ => None,
    };

    if let Some(style) = style {
        let start = node.start_byte();
        let end = node.end_byte();
        if end > start {
            spans.push(HighlightSpan { range: start..end, style });
        }
    }

    if cursor.goto_first_child() {
        collect_highlights(cursor, theme, spans);
        while cursor.goto_next_sibling() {
            collect_highlights(cursor, theme, spans);
        }
        cursor.goto_parent();
    }
}

fn merge_spans(spans: Vec<HighlightSpan>) -> Vec<HighlightSpan> {
    if spans.is_empty() { return spans; }
    let mut merged = Vec::new();
    let mut current = spans[0].clone();
    for span in spans.into_iter().skip(1) {
        if span.range.start <= current.range.end {
            if span.range.end > current.range.end {
                current.range.end = span.range.end;
            }
        } else {
            merged.push(current);
            current = span;
        }
    }
    merged.push(current);
    merged
}

pub fn parse_and_capture(lang: &str, code: &str, _query: &str) -> Result<tree_sitter::Tree, String> {
    let lang: SyntaxLanguage = lang.parse::<SyntaxLanguage>().map_err(|e| e.to_string())?;
    if lang == SyntaxLanguage::Unknown { return Err("unknown language".to_string()); }
    let mut parser = get_parser(lang);
    parser.parse(code, None).ok_or_else(|| "parse failed".to_string())
}

pub fn highlight_to_lines(lang: &str, code: &str, theme: &HighlightTheme) -> Vec<Vec<(String, (u8, u8, u8), bool, bool)>> {
    let spans = highlight(lang, code, theme);
    let line_offsets: Vec<usize> = std::iter::once(0)
        .chain(code.match_indices('\n').map(|(i, _)| i + 1))
        .collect();

    code.lines()
        .enumerate()
        .map(|(i, line)| {
            let line_start = if i == 0 { 0 } else { line_offsets[i] };
            let line_end = line_start + line.len();
            let mut segments = Vec::new();
            let mut pos = line_start;

            for span in &spans {
                let span_start = span.range.start.max(line_start);
                let span_end = span.range.end.min(line_end);
                if span_end > span_start && span_end > pos {
                    if span_start > pos {
                        let (gap_start, gap_end) = (pos, span_start.min(line_end));
                        if gap_end > gap_start {
                            segments.push((code[gap_start..gap_end].to_string(), theme.default, false, false));
                        }
                    }
                    let (s_start, s_end) = (span_start, span_end);
                    if s_end > s_start {
                        let rgb = span.style.rgb();
                        segments.push((code[s_start..s_end].to_string(), rgb, span.style.is_bold(), span.style.is_italic()));
                    }
                    pos = span_end;
                }
            }
            if pos < line_end {
                segments.push((code[pos..line_end].to_string(), theme.default, false, false));
            }
            if segments.is_empty() {
                segments.push((code[line_start..line_end].to_string(), theme.default, false, false));
            }
            segments
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlight_rust() {
        let code = "fn main() { let x = 42; }";
        let spans = highlight("rust", code, &HighlightTheme::default());
        assert!(!spans.is_empty());
    }

    #[test]
    fn test_highlight_to_lines() {
        let code = "fn main() { 42 }";
        let lines = highlight_to_lines("rust", code, &HighlightTheme::default());
        assert_eq!(lines.len(), 1);
        assert!(!lines[0].is_empty());
    }
}
