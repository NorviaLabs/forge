//! Go fact extraction. See `rust.rs` for the shared shape every language
//! extractor follows; only the grammar-specific node kinds and
//! classification rules differ. Go's grammar distinguishes free functions
//! from methods by node kind (`function_declaration` vs.
//! `method_declaration`), so — unlike Rust/Python — no ancestor walk is
//! needed to classify a definition.

use std::path::Path;
use std::sync::OnceLock;

use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Parser, Query, QueryCursor};

use super::{EdgeFact, ExtractedFacts, Extractor, SymbolFact};
use crate::schema::kind;

const IMPORTS_QUERY: &str = include_str!("../queries/go/imports.scm");
const TYPES_QUERY: &str = include_str!("../queries/go/types.scm");
const IMPLEMENTS_QUERY: &str = include_str!("../queries/go/implements.scm");
const FUNCTIONS_QUERY: &str = include_str!("../queries/go/functions.scm");
const CALLS_QUERY: &str = include_str!("../queries/go/calls.scm");

struct Queries {
    imports: Query,
    types: Query,
    implements: Query,
    functions: Query,
    calls: Query,
}

fn queries() -> &'static Queries {
    static QUERIES: OnceLock<Queries> = OnceLock::new();
    QUERIES.get_or_init(|| {
        let lang = tree_sitter_go::LANGUAGE.into();
        let build = |src: &str| Query::new(&lang, src).expect("built-in query must compile");
        Queries {
            imports: build(IMPORTS_QUERY),
            types: build(TYPES_QUERY),
            implements: build(IMPLEMENTS_QUERY),
            functions: build(FUNCTIONS_QUERY),
            calls: build(CALLS_QUERY),
        }
    })
}

pub struct GoExtractor;

impl Extractor for GoExtractor {
    fn language(&self) -> forge_syntax::SyntaxLanguage {
        forge_syntax::SyntaxLanguage::Go
    }

    fn extract(&self, source: &str, path: &Path) -> ExtractedFacts {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_go::LANGUAGE.into())
            .expect("go grammar is valid");
        let Some(tree) = parser.parse(source, None) else {
            return ExtractedFacts::default();
        };
        let root = tree.root_node();
        let bytes = source.as_bytes();
        let file = path.to_string_lossy().replace('\\', "/");
        let q = queries();

        let mut facts = ExtractedFacts::default();
        extract_imports(q, root, bytes, &file, &mut facts);
        extract_types(q, root, bytes, &file, &mut facts);
        extract_implements(q, root, bytes, &file, &mut facts);
        extract_functions(q, root, bytes, &file, &mut facts);
        extract_calls(q, root, bytes, &file, &mut facts);
        facts
    }
}

fn node_text<'a>(node: Node, bytes: &'a [u8]) -> &'a str {
    node.utf8_text(bytes).unwrap_or("")
}

fn line_of(node: Node) -> u32 {
    node.start_position().row as u32 + 1
}

fn col_of(node: Node) -> u32 {
    node.start_position().column as u32
}

/// `"github.com/foo/bar"` -> `bar`. Best-effort: doesn't account for an
/// `import alias "path"` renaming the package at use sites.
fn last_path_segment(raw: &str) -> &str {
    let unquoted = raw.trim_matches('"');
    unquoted.rsplit('/').next().unwrap_or(unquoted)
}

fn enclosing_function_name(node: Node, bytes: &[u8]) -> Option<String> {
    let mut current = node.parent();
    while let Some(n) = current {
        if n.kind() == "function_declaration" || n.kind() == "method_declaration" {
            let name = n.child_by_field_name("name")?;
            return Some(node_text(name, bytes).to_string());
        }
        current = n.parent();
    }
    None
}

fn extract_imports(q: &Queries, root: Node, bytes: &[u8], file: &str, facts: &mut ExtractedFacts) {
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&q.imports, root, bytes);
    while let Some(m) = matches.next() {
        for cap in m.captures {
            let raw = node_text(cap.node, bytes);
            let name = last_path_segment(raw);
            if name.is_empty() {
                continue;
            }
            facts.edges.push(EdgeFact {
                kind: kind::IMPORTS,
                src_symbol: None,
                target_name: name.to_string(),
                from_file: file.to_string(),
                from_line: line_of(cap.node),
            });
        }
    }
}

fn extract_types(q: &Queries, root: Node, bytes: &[u8], file: &str, facts: &mut ExtractedFacts) {
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&q.types, root, bytes);
    while let Some(m) = matches.next() {
        for cap in m.captures {
            let capture_name = q.types.capture_names()[cap.index as usize];
            if capture_name != "type.name" {
                continue;
            }
            facts.symbols.push(SymbolFact {
                name: node_text(cap.node, bytes).to_string(),
                qualified: None,
                kind: kind::TYPE,
                file: file.to_string(),
                line: line_of(cap.node),
                col: col_of(cap.node),
            });
        }
    }
}

fn extract_implements(
    q: &Queries,
    root: Node,
    bytes: &[u8],
    file: &str,
    facts: &mut ExtractedFacts,
) {
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&q.implements, root, bytes);
    while let Some(m) = matches.next() {
        let mut type_name = None;
        let mut trait_name = None;
        let mut at_node = None;
        for cap in m.captures {
            let capture_name = q.implements.capture_names()[cap.index as usize];
            match capture_name {
                "impl.type" => type_name = Some(node_text(cap.node, bytes).to_string()),
                "impl.trait" => {
                    trait_name = Some(node_text(cap.node, bytes).to_string());
                    at_node = Some(cap.node);
                }
                _ => {}
            }
        }
        if let (Some(type_name), Some(trait_name), Some(node)) = (type_name, trait_name, at_node) {
            facts.edges.push(EdgeFact {
                kind: kind::IMPLEMENTS,
                src_symbol: Some(type_name),
                target_name: trait_name,
                from_file: file.to_string(),
                from_line: line_of(node),
            });
        }
    }
}

fn extract_functions(
    q: &Queries,
    root: Node,
    bytes: &[u8],
    file: &str,
    facts: &mut ExtractedFacts,
) {
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&q.functions, root, bytes);
    while let Some(m) = matches.next() {
        for cap in m.captures {
            let capture_name = q.functions.capture_names()[cap.index as usize];
            if capture_name != "function.name" {
                continue;
            }
            // The sibling `@function.free`/`@function.method` capture on the
            // same match tells us which pattern fired; find it among this
            // match's other captures rather than re-querying.
            let symbol_kind = if m
                .captures
                .iter()
                .any(|c| q.functions.capture_names()[c.index as usize] == "function.method")
            {
                kind::METHOD
            } else {
                kind::FUNCTION
            };
            facts.symbols.push(SymbolFact {
                name: node_text(cap.node, bytes).to_string(),
                qualified: None,
                kind: symbol_kind,
                file: file.to_string(),
                line: line_of(cap.node),
                col: col_of(cap.node),
            });
        }
    }
}

fn extract_calls(q: &Queries, root: Node, bytes: &[u8], file: &str, facts: &mut ExtractedFacts) {
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&q.calls, root, bytes);
    while let Some(m) = matches.next() {
        let mut callee = None;
        let mut call_node = None;
        for cap in m.captures {
            let capture_name = q.calls.capture_names()[cap.index as usize];
            match capture_name {
                "call.callee" => callee = Some(node_text(cap.node, bytes).to_string()),
                "call.node" => call_node = Some(cap.node),
                _ => {}
            }
        }
        if let (Some(callee), Some(call_node)) = (callee, call_node) {
            facts.edges.push(EdgeFact {
                kind: kind::CALLS,
                src_symbol: enclosing_function_name(call_node, bytes),
                target_name: callee,
                from_file: file.to_string(),
                from_line: line_of(call_node),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn extract(source: &str) -> ExtractedFacts {
        GoExtractor.extract(source, &PathBuf::from("main.go"))
    }

    #[test]
    fn extracts_a_free_function_and_a_method_distinctly() {
        let facts = extract(
            "package main\n\nfunc FreeFn() {}\n\ntype Foo struct{}\n\nfunc (f Foo) MethodFn() {}\n",
        );
        let names: Vec<_> = facts
            .symbols
            .iter()
            .map(|s| (s.name.as_str(), s.kind))
            .collect();
        assert!(names.contains(&("FreeFn", kind::FUNCTION)));
        assert!(names.contains(&("MethodFn", kind::METHOD)));
        assert!(names.contains(&("Foo", kind::TYPE)));
    }

    #[test]
    fn extracts_embedding_as_an_implements_edge() {
        let facts = extract("package main\n\ntype Base struct{}\n\ntype Foo struct {\n\tBase\n}\n");
        let implements: Vec<_> = facts
            .edges
            .iter()
            .filter(|e| e.kind == kind::IMPLEMENTS)
            .collect();
        assert_eq!(implements.len(), 1);
        assert_eq!(implements[0].src_symbol.as_deref(), Some("Foo"));
        assert_eq!(implements[0].target_name, "Base");
    }

    #[test]
    fn named_field_of_a_type_is_not_mistaken_for_embedding() {
        let facts =
            extract("package main\n\ntype Base struct{}\n\ntype Foo struct {\n\tB Base\n}\n");
        assert!(facts.edges.iter().all(|e| e.kind != kind::IMPLEMENTS));
    }

    #[test]
    fn extracts_calls_with_enclosing_function_as_src_symbol() {
        let facts = extract("package main\n\nfunc caller() {\n\tcallee()\n}\n\nfunc callee() {}\n");
        let calls: Vec<_> = facts
            .edges
            .iter()
            .filter(|e| e.kind == kind::CALLS)
            .collect();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].target_name, "callee");
        assert_eq!(calls[0].src_symbol.as_deref(), Some("caller"));
    }

    #[test]
    fn extracts_import_path_last_segment() {
        let facts = extract("package main\n\nimport \"fmt\"\nimport \"github.com/foo/bar\"\n");
        let mut imports: Vec<_> = facts
            .edges
            .iter()
            .filter(|e| e.kind == kind::IMPORTS)
            .map(|e| e.target_name.as_str())
            .collect();
        imports.sort();
        assert_eq!(imports, vec!["bar", "fmt"]);
    }
}
