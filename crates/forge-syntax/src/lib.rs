//! Tree-sitter powered syntax highlighting.

mod highlight;
mod lang;

pub use highlight::{
    highlight, highlight_to_lines, parse_and_capture, HighlightClass, HighlightSpan,
    HighlightStyle, HighlightTheme,
};
pub use lang::{detect_from_path, detect_language, get_parser, SyntaxLanguage};

pub use highlight::HighlightTheme as Theme;
