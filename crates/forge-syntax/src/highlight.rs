//! Tree-sitter based syntax highlighting.

use crate::lang::{get_parser, SyntaxLanguage};
use std::ops::Range;
use std::sync::Arc;
use tree_sitter::StreamingIterator;

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

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HighlightStyle {
    pub class: HighlightClass,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HighlightClass {
    Comment,
    Keyword,
    String,
    Number,
    Function,
    Type,
    Variable,
    Operator,
    Punctuation,
    Property,
    Tag,
    Attribute,
    Default,
}

// `Eq` + `Hash` let the theme participate in the highlight cache key, so a theme
// switch cannot serve colours from the previous theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    pub fn rgb(&self, theme: &HighlightTheme) -> (u8, u8, u8) {
        match self.class {
            HighlightClass::Comment => theme.comment,
            HighlightClass::Keyword => theme.keyword,
            HighlightClass::String => theme.string,
            HighlightClass::Number => theme.number,
            HighlightClass::Function => theme.function,
            HighlightClass::Type => theme.type_,
            HighlightClass::Variable => theme.variable,
            HighlightClass::Operator => theme.operator,
            HighlightClass::Punctuation => theme.punctuation,
            HighlightClass::Property => theme.property,
            HighlightClass::Tag => theme.tag,
            HighlightClass::Attribute => theme.attribute,
            HighlightClass::Default => theme.default,
        }
    }

    pub fn is_bold(self) -> bool {
        matches!(
            self.class,
            HighlightClass::Keyword | HighlightClass::Number | HighlightClass::Type
        )
    }

    pub fn is_italic(self) -> bool {
        matches!(self.class, HighlightClass::Comment)
    }
}

impl Default for HighlightTheme {
    fn default() -> Self {
        Self {
            comment: (157, 170, 189),
            keyword: (104, 168, 255),
            string: (227, 179, 65),
            number: (104, 168, 255),
            function: (180, 156, 255),
            type_: (104, 168, 255),
            variable: (230, 237, 243),
            operator: (157, 170, 189),
            punctuation: (157, 170, 189),
            property: (86, 212, 221),
            tag: (86, 212, 221),
            attribute: (227, 179, 65),
            default: (230, 237, 243),
        }
    }
}

impl HighlightTheme {
    fn style_for_class(&self, class: HighlightClass) -> HighlightStyle {
        HighlightStyle { class }
    }

    /// Readable syntax colours on the Forge Light canvas.
    pub fn light() -> Self {
        Self {
            comment: (122, 135, 152),
            keyword: (23, 105, 204),
            string: (153, 101, 0),
            number: (23, 105, 204),
            function: (112, 72, 200),
            type_: (23, 105, 204),
            variable: (23, 32, 44),
            operator: (122, 135, 152),
            punctuation: (122, 135, 152),
            property: (8, 126, 139),
            tag: (8, 126, 139),
            attribute: (153, 101, 0),
            default: (23, 32, 44),
        }
    }
}

pub fn highlight(lang: &str, code: &str, theme: &HighlightTheme) -> Vec<HighlightSpan> {
    let lang: SyntaxLanguage = match lang.parse() {
        Ok(l) => l,
        Err(_) => {
            return vec![HighlightSpan {
                range: 0..code.len(),
                style: HighlightStyle {
                    class: HighlightClass::Default,
                },
            }]
        }
    };

    if lang == SyntaxLanguage::Unknown {
        return vec![HighlightSpan {
            range: 0..code.len(),
            style: HighlightStyle {
                class: HighlightClass::Default,
            },
        }];
    }

    let mut parser = get_parser(lang);
    let tree = match parser.parse(code, None) {
        Some(t) => t,
        None => {
            return vec![HighlightSpan {
                range: 0..code.len(),
                style: HighlightStyle {
                    class: HighlightClass::Default,
                },
            }]
        }
    };

    let mut spans = Vec::new();
    let query = crate::queries::query(lang);
    let mut cursor = tree_sitter::QueryCursor::new();
    let mut captures = cursor.captures(query, tree.root_node(), code.as_bytes());
    while let Some((matched, index)) = captures.next() {
        let capture = matched.captures()[*index];
        if let Some(class) = capture_class(query.capture_names()[capture.index as usize]) {
            spans.push(HighlightSpan {
                range: capture.node.byte_range(),
                style: theme.style_for_class(class),
            });
        }
    }

    if spans.is_empty() {
        spans.push(HighlightSpan {
            range: 0..code.len(),
            style: HighlightStyle {
                class: HighlightClass::Default,
            },
        });
    }

    spans.sort_by_key(|s| s.range.start);
    merge_spans(spans)
}

fn capture_class(name: &str) -> Option<HighlightClass> {
    use HighlightClass::*;
    Some(match name {
        "variable.member" => Property,
        "string.special.key" => Property,
        _ => match name.split('.').next()? {
            "comment" => Comment,
            "keyword" | "boolean" | "constant" => Keyword,
            "string" | "character" => String,
            "number" | "float" => Number,
            "function" | "method" | "constructor" => Function,
            "type" | "namespace" | "module" => Type,
            "variable" => Variable,
            "operator" => Operator,
            "punctuation" => Punctuation,
            "property" | "field" => Property,
            "tag" => Tag,
            "attribute" => Attribute,
            _ => return None,
        },
    })
}

fn merge_spans(spans: Vec<HighlightSpan>) -> Vec<HighlightSpan> {
    if spans.is_empty() {
        return spans;
    }
    // Find the maximum byte offset covered
    let max_end = spans.iter().map(|s| s.range.end).max().unwrap_or(0);
    if max_end == 0 {
        return Vec::new();
    }
    // Build a style-per-byte array; shorter (inner) spans override longer (outer) ones.
    let mut coverage: Vec<Option<HighlightStyle>> = vec![None; max_end];
    // Sort by length descending so outer spans lay down first, inner overwrite.
    let mut sorted: Vec<_> = spans.into_iter().collect();
    sorted.sort_by(|a, b| {
        let la = a.range.end - a.range.start;
        let lb = b.range.end - b.range.start;
        lb.cmp(&la)
    });
    for span in sorted {
        for i in span.range.clone() {
            if i < max_end {
                coverage[i] = Some(span.style);
            }
        }
    }
    // Reconstruct contiguous same-style spans.
    let mut merged = Vec::new();
    let mut i = 0;
    while i < max_end {
        if let Some(style) = coverage[i] {
            let start = i;
            while i < max_end && coverage[i] == Some(style) {
                i += 1;
            }
            merged.push(HighlightSpan {
                range: start..i,
                style,
            });
        } else {
            i += 1;
        }
    }
    merged
}

pub fn parse_and_capture(
    lang: &str,
    code: &str,
    _query: &str,
) -> Result<tree_sitter::Tree, String> {
    let lang: SyntaxLanguage = lang.parse::<SyntaxLanguage>().map_err(|e| e.to_string())?;
    if lang == SyntaxLanguage::Unknown {
        return Err("unknown language".to_string());
    }
    let mut parser = get_parser(lang);
    parser
        .parse(code, None)
        .ok_or_else(|| "parse failed".to_string())
}

/// Highlight `code` into per-line styled segments.
///
/// Results are memoised on `(lang, code, theme)` — see [`crate::cache`]. The TUI
/// re-renders identical code blocks on every resize, theme switch, scroll and
/// busy-phase change, and tree-sitter is far too expensive to repeat for output
/// that cannot have changed.
///
/// Shared rather than owned: callers render the segments into their own styled
/// spans and do not need to mutate them, so handing back an [`Arc`] avoids
/// copying a `String` per token on every lookup. Borrow with `.iter()`.
pub fn highlight_to_lines(lang: &str, code: &str, theme: &HighlightTheme) -> HighlightedLines {
    crate::cache::cached_or_compute(lang, code, theme, || {
        highlight_to_lines_uncached(lang, code, theme)
    })
}

fn highlight_to_lines_uncached(
    lang: &str,
    code: &str,
    theme: &HighlightTheme,
) -> Vec<Vec<HighlightedSegment>> {
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
                            segments.push((
                                code[gap_start..gap_end].to_string(),
                                theme.default,
                                false,
                                false,
                            ));
                        }
                    }
                    let (s_start, s_end) = (span_start, span_end);
                    if s_end > s_start {
                        let rgb = span.style.rgb(theme);
                        segments.push((
                            code[s_start..s_end].to_string(),
                            rgb,
                            span.style.is_bold(),
                            span.style.is_italic(),
                        ));
                    }
                    pos = span_end;
                }
            }
            if pos < line_end {
                segments.push((code[pos..line_end].to_string(), theme.default, false, false));
            }
            if segments.is_empty() {
                segments.push((
                    code[line_start..line_end].to_string(),
                    theme.default,
                    false,
                    false,
                ));
            }
            segments
        })
        .collect()
}

/// One styled run inside a highlighted line: text, RGB colour, bold, italic.
pub type HighlightedSegment = (String, (u8, u8, u8), bool, bool);

/// Highlighted lines, shared so a cache lookup does not copy every segment.
pub type HighlightedLines = Arc<Vec<Vec<HighlightedSegment>>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlight_toml() {
        let code = "[package]\nname = \"forge\"\nenabled = true\ncount = 42\n\"quoted\" = 'literal'\n# comment\n";
        let spans = highlight("toml", code, &HighlightTheme::default());
        for (token, class) in [
            ("package", HighlightClass::Property),
            ("name", HighlightClass::Property),
            ("\"forge\"", HighlightClass::String),
            ("true", HighlightClass::Keyword),
            ("42", HighlightClass::Number),
            ("\"quoted\"", HighlightClass::Property),
            ("'literal'", HighlightClass::String),
            ("# comment", HighlightClass::Comment),
        ] {
            let start = code.find(token).unwrap();
            assert!(
                spans.iter().any(|span| span.range.start <= start
                    && span.range.end >= start + token.len()
                    && span.style.class == class),
                "missing {class:?} for {token}: {spans:?}"
            );
        }
    }

    #[test]
    fn highlight_rust() {
        let code = "fn main() { let x = 42; }";
        let spans = highlight("rust", code, &HighlightTheme::default());
        assert!(!spans.is_empty());
    }

    #[test]
    fn test_highlight_to_lines() {
        // Populates the process-global highlight cache; see `cache::lock_cache`.
        let _guard = crate::cache::lock_cache();
        let code = "fn main() { 42 }";
        let lines = highlight_to_lines("rust", code, &HighlightTheme::default());
        assert_eq!(lines.len(), 1);
        assert!(!lines[0].is_empty());
    }

    #[test]
    fn light_theme_uses_distinct_colours() {
        let _guard = crate::cache::lock_cache();
        let theme = HighlightTheme::light();
        assert_ne!(theme.default, theme.comment);
        let lines = highlight_to_lines("rust", "fn main() {}", &theme);
        assert!(!lines.is_empty());
    }

    /// Every field holds a distinct value so a mis-wired match arm in
    /// `HighlightStyle::rgb` cannot pass by coincidence.
    fn distinct_theme() -> HighlightTheme {
        HighlightTheme {
            comment: (1, 1, 1),
            keyword: (2, 2, 2),
            string: (3, 3, 3),
            number: (4, 4, 4),
            function: (5, 5, 5),
            type_: (6, 6, 6),
            variable: (7, 7, 7),
            operator: (8, 8, 8),
            punctuation: (9, 9, 9),
            property: (10, 10, 10),
            tag: (11, 11, 11),
            attribute: (12, 12, 12),
            default: (13, 13, 13),
        }
    }

    #[test]
    fn rgb_maps_every_class_to_its_own_theme_slot() {
        let theme = distinct_theme();
        let cases = [
            (HighlightClass::Comment, theme.comment),
            (HighlightClass::Keyword, theme.keyword),
            (HighlightClass::String, theme.string),
            (HighlightClass::Number, theme.number),
            (HighlightClass::Function, theme.function),
            (HighlightClass::Type, theme.type_),
            (HighlightClass::Variable, theme.variable),
            (HighlightClass::Operator, theme.operator),
            (HighlightClass::Punctuation, theme.punctuation),
            (HighlightClass::Property, theme.property),
            (HighlightClass::Tag, theme.tag),
            (HighlightClass::Attribute, theme.attribute),
            (HighlightClass::Default, theme.default),
        ];
        for (class, expected) in cases {
            assert_eq!(
                HighlightStyle { class }.rgb(&theme),
                expected,
                "{class:?} resolved to the wrong theme colour"
            );
        }
    }

    #[test]
    fn span_text_slices_the_source() {
        let span = HighlightSpan {
            range: 3..7,
            style: HighlightStyle {
                class: HighlightClass::Function,
            },
        };
        assert_eq!(span.text("fn main()"), "main");
    }

    #[test]
    fn unparseable_language_falls_back_to_one_default_span() {
        let code = "some arbitrary text";
        let spans = highlight(
            "definitely-not-a-language",
            code,
            &HighlightTheme::default(),
        );
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].range, 0..code.len());
        assert_eq!(spans[0].style.class, HighlightClass::Default);
    }

    #[test]
    fn unknown_language_falls_back_to_one_default_span() {
        let code = "plain text body";
        for name in ["unknown", "*"] {
            let spans = highlight(name, code, &HighlightTheme::default());
            assert_eq!(
                spans.len(),
                1,
                "language {name:?} should not be highlighted"
            );
            assert_eq!(spans[0].range, 0..code.len());
            assert_eq!(spans[0].style.class, HighlightClass::Default);
        }
    }

    /// Empty input takes the `spans.is_empty()` fallback, which pushes a
    /// zero-width span; `merge_spans` then sees `max_end == 0` and drops it.
    #[test]
    fn empty_source_yields_no_spans() {
        let spans = highlight("rust", "", &HighlightTheme::default());
        assert!(spans.is_empty());
    }

    #[test]
    fn rust_line_comment_is_classified_as_comment() {
        let code = "// explanatory note\nfn main() {}";
        let spans = highlight("rust", code, &HighlightTheme::default());
        let comment = spans
            .iter()
            .find(|s| s.style.class == HighlightClass::Comment)
            .expect("comment should be classified");
        assert!(comment.text(code).contains("explanatory note"));
    }

    #[test]
    fn rust_attribute_is_classified_as_attribute() {
        let code = "#[derive(Debug)]\nstruct Widget;";
        let spans = highlight("rust", code, &HighlightTheme::default());
        assert!(
            spans
                .iter()
                .any(|s| s.style.class == HighlightClass::Attribute),
            "expected an Attribute span, got {:?}",
            spans.iter().map(|s| s.style.class).collect::<Vec<_>>()
        );
    }

    #[test]
    fn called_identifier_is_classified_as_function() {
        let code = "fn main() { helper(); }";
        let spans = highlight("rust", code, &HighlightTheme::default());
        let called = spans
            .iter()
            .filter(|s| s.style.class == HighlightClass::Function)
            .map(|s| s.text(code))
            .collect::<Vec<_>>();
        assert!(
            called.contains(&"helper"),
            "call target should be a Function span, got {called:?}"
        );
    }

    #[test]
    fn merge_spans_handles_degenerate_input() {
        assert!(merge_spans(Vec::new()).is_empty());
        let zero_width = vec![HighlightSpan {
            range: 0..0,
            style: HighlightStyle {
                class: HighlightClass::Default,
            },
        }];
        assert!(merge_spans(zero_width).is_empty());
    }

    #[test]
    fn merge_spans_lets_inner_spans_override_outer_ones() {
        let outer = HighlightSpan {
            range: 0..10,
            style: HighlightStyle {
                class: HighlightClass::Default,
            },
        };
        let inner = HighlightSpan {
            range: 4..6,
            style: HighlightStyle {
                class: HighlightClass::Keyword,
            },
        };
        let merged = merge_spans(vec![outer, inner]);
        assert_eq!(
            merged
                .iter()
                .map(|s| (s.range.clone(), s.style.class))
                .collect::<Vec<_>>(),
            vec![
                (0..4, HighlightClass::Default),
                (4..6, HighlightClass::Keyword),
                (6..10, HighlightClass::Default),
            ]
        );
    }

    #[test]
    fn parse_and_capture_returns_a_tree_for_rust() {
        let tree = parse_and_capture("rust", "fn main() {}", "").expect("rust source should parse");
        assert_eq!(tree.root_node().kind(), "source_file");
        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn parse_and_capture_rejects_unknown_languages() {
        assert_eq!(
            parse_and_capture("unknown", "anything", "").unwrap_err(),
            "unknown language"
        );
        assert!(parse_and_capture("definitely-not-a-language", "anything", "").is_err());
    }

    #[test]
    fn rust_raw_string_literal_is_classified_as_string() {
        let code = "fn main() { let s = r\"raw\"; }";
        let spans = highlight("rust", code, &HighlightTheme::default());
        let raw = spans
            .iter()
            .find(|s| s.style.class == HighlightClass::String)
            .expect("raw string literal should be classified as a String span");
        assert!(raw.text(code).contains("raw"));
    }

    /// A blank line between statements carries no spans at all, which drives
    /// `highlight_to_lines_uncached` through its `segments.is_empty()`
    /// fallback for that one line while sibling lines still get real spans.
    /// The incomplete line with `let y = x` leaves unstyled text after the
    /// last span before `line_end`, exercising the trailing-remainder
    /// push for a non-blank line.
    #[test]
    fn highlight_to_lines_handles_blank_lines_and_trailing_unstyled_text() {
        // Populates the process-global highlight cache; see `cache::lock_cache`.
        let _guard = crate::cache::lock_cache();
        let code = "fn main() {\n    let s = r\"raw\";\n\n    let y = x\n}\n";
        let lines = highlight_to_lines("rust", code, &HighlightTheme::default());
        // `code.lines()` yields 5 entries: the trailing "\n" does not add a 6th.
        assert_eq!(lines.len(), 5);

        // Blank line: exactly one segment, and it is the empty string (the
        // `segments.is_empty()` fallback pushing the whole—empty—line span).
        let blank = &lines[2];
        assert_eq!(blank.len(), 1);
        assert_eq!(blank[0].0, "");

        // The incomplete statement's trailing text must not be dropped.
        let let_y_line = &lines[3];
        let joined: String = let_y_line.iter().map(|seg| seg.0.as_str()).collect();
        assert_eq!(joined, "    let y = x");
        assert_eq!(
            let_y_line.last().expect("line must have segments").0,
            " y = x",
            "unstyled text after the last span must be pushed as a trailing segment"
        );
    }
}
