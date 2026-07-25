//! Tree-sitter powered syntax highlighting and structural refactoring.

mod highlight;
mod lang;
mod refactor;

pub use highlight::{
    highlight, highlight_to_lines, parse_and_capture, HighlightClass, HighlightSpan,
    HighlightStyle, HighlightTheme,
};
pub use lang::{detect_from_path, detect_language, get_parser, SyntaxLanguage};
pub use refactor::{
    query_code, refactor, Capture, Position, QueryError, RefactorError, RefactorOp, RefactorType,
};

pub use highlight::HighlightTheme as Theme;
