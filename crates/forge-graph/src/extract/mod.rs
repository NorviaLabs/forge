//! Per-language fact extraction from a parsed tree-sitter tree.
//!
//! Every `Extractor` implementation only ever emits facts it can back with a
//! real syntax-tree node — no type resolution, no guessing which overload a
//! call site means. Name resolution (turning an `EdgeFact::callee_name` into
//! an actual `dst_symbol_id`, honestly enumerating every candidate when more
//! than one symbol matches) happens later, in `store.rs`'s second pass, once
//! every language's symbols are known — see the module doc there.

pub mod rust;

use std::path::Path;

/// One definition site: a function, method, or type declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolFact {
    pub name: String,
    pub qualified: Option<String>,
    pub kind: &'static str,
    pub file: String,
    pub line: u32,
    pub col: u32,
}

/// One syntactic relationship whose *source* is fully known but whose
/// *target* is a bare name still to be resolved against the repo-wide
/// symbol table (see `store.rs::resolve_and_insert_edges_batch`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeFact {
    pub kind: &'static str,
    /// The enclosing symbol this edge originates from (the caller, the
    /// importing module, the implementing type). `None` for a module-level
    /// import with no enclosing symbol.
    pub src_symbol: Option<String>,
    /// Bare name of the referenced symbol — resolved later.
    pub target_name: String,
    pub from_file: String,
    pub from_line: u32,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ExtractedFacts {
    pub symbols: Vec<SymbolFact>,
    pub edges: Vec<EdgeFact>,
}

pub trait Extractor {
    fn language(&self) -> forge_syntax::SyntaxLanguage;
    /// `path` is workspace-relative — every `SymbolFact`/`EdgeFact` this
    /// returns carries it verbatim as `file`/`from_file`, so the caller must
    /// pass a path already relative to the workspace root, not absolute.
    fn extract(&self, source: &str, path: &Path) -> ExtractedFacts;
}
