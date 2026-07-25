//! Structural refactoring via tree-sitter queries.

use std::ops::Range;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tree_sitter::{Query, QueryCursor};
use streaming_iterator::StreamingIterator;

use crate::lang::{get_parser, SyntaxLanguage};

#[derive(Debug, Error)]
pub enum QueryError {
    #[error("parse error: {0}")]
    Parse(String),
    #[error("query error: {0}")]
    Query(String),
}

#[derive(Debug, Error)]
pub enum RefactorError {
    #[error("query error: {0}")]
    Query(#[from] QueryError),
    #[error("apply error: {0}")]
    Apply(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capture {
    pub name: String,
    pub text: String,
    pub range: Range<usize>,
    pub start: Position,
    pub end: Position,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub row: usize,
    pub col: usize,
}

pub fn query_code(lang: &str, code: &str, pattern: &str) -> Result<Vec<Capture>, QueryError> {
    let lang: SyntaxLanguage = lang
        .parse()
        .map_err(|e: <SyntaxLanguage as std::str::FromStr>::Err| QueryError::Parse(e))?;
    if lang == SyntaxLanguage::Unknown {
        return Err(QueryError::Parse("unknown language".to_string()));
    }

    let mut parser = get_parser(lang);
    let tree = parser
        .parse(code, None)
        .ok_or_else(|| QueryError::Parse("failed to parse".to_string()))?;

    let ts_query = Query::new(&lang.tree_sitter(), pattern)
        .map_err(|e| QueryError::Query(e.to_string()))?;

    let mut cursor = QueryCursor::new();
    let mut captures = Vec::new();
    let names: &[&str] = &ts_query.capture_names();

    let mut query_captures = cursor.captures(&ts_query, tree.root_node(), code.as_bytes());

    while let Some((m, idx)) = query_captures.get() {
        let capture_name: &str = names[*idx];
        if let Some(capture_node) = m.captures.first() {
            let text = capture_node.node.utf8_text(code.as_bytes()).unwrap_or_default().to_string();
            let start = capture_node.node.start_position();
            let end = capture_node.node.end_position();

            captures.push(Capture {
                name: capture_name.to_string(),
                text,
                range: capture_node.node.start_byte()..capture_node.node.end_byte(),
                start: Position { row: start.row, col: start.column },
                end: Position { row: end.row, col: end.column },
            });
        }
        query_captures.advance();
    }

    Ok(captures)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefactorOp {
    pub op_type: RefactorType,
    pub query: String,
    pub replacement: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum RefactorType {
    Extract,
    Rename { old_name: String, new_name: String },
    Delete,
    Replace { template: String },
    Inline,
    Wrap { before: String, after: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefactorResult {
    pub code: String,
    pub changes: usize,
    pub captures: Vec<Capture>,
}

pub fn refactor(lang: &str, code: &str, op: &RefactorOp) -> Result<RefactorResult, RefactorError> {
    let captures = query_code(lang, code, &op.query)?;

    let mut result = code.to_string();
    let mut changes = 0;

    for cap in captures.iter().rev() {
        match &op.op_type {
            RefactorType::Extract => {}
            RefactorType::Rename { new_name: _, .. } => {
                if let Some(replacement) = &op.replacement {
                    result.replace_range(cap.range.clone(), replacement);
                    changes += 1;
                }
            }
            RefactorType::Delete => {
                result.replace_range(cap.range.clone(), "");
                changes += 1;
            }
            RefactorType::Replace { template } => {
                result.replace_range(cap.range.clone(), template);
                changes += 1;
            }
            RefactorType::Wrap { before, after } => {
                let range = cap.range.clone();
                let new_text = format!("{before}{}{after}", &result[range.clone()]);
                result.replace_range(range, &new_text);
                changes += 1;
            }
            RefactorType::Inline => {
                result.replace_range(cap.range.clone(), "");
                changes += 1;
            }
        }
    }

    Ok(RefactorResult { code: result, changes, captures })
}

pub fn extract_functions(lang: &str, code: &str) -> Result<Vec<Capture>, QueryError> {
    let pattern = match lang.parse::<SyntaxLanguage>() {
        Ok(SyntaxLanguage::Rust) => "(function_item name: (identifier) @name) @fn",
        Ok(SyntaxLanguage::TypeScript | SyntaxLanguage::JavaScript) => {
            "(function_declaration name: (identifier) @name) @fn"
        }
        Ok(SyntaxLanguage::Python) => "(function_definition name: (identifier) @name) @fn",
        Ok(SyntaxLanguage::Go) => "(function_declaration name: (identifier) @name) @fn",
        _ => "(function_declaration) @fn",
    };
    query_code(lang, code, pattern)
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_api_works() {
        let code = "fn main() {}";
        let result = query_code("rust", code, "(source_file)");
        assert!(result.is_ok());
    }

    #[test]
    fn refactor_api_works() {
        let code = "fn main() {}";
        let op = RefactorOp {
            op_type: RefactorType::Extract,
            query: "(source_file)".into(),
            replacement: None,
        };
        let result = refactor("rust", code, &op);
        assert!(result.is_ok());
    }
}
