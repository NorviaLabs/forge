//! Repo-wide symbol graph — `find_definition`/`find_references` backing
//! store. Syntactic only: every fact is a real tree-sitter node, and where a
//! name can't be structurally narrowed to one definition, every real
//! candidate is returned rather than a guess (see `schema.rs`, `store.rs`).
//!
//! Covers the 5 languages `forge-syntax` already tree-sitter-parses for
//! highlighting: Rust, Python, Go, TypeScript, JavaScript (PR2). Still one
//! synchronous full-repo build at `open()`, no live watcher yet (that's
//! PR3 — see the plan). `GraphHandle::open` is still the entry point later
//! work builds on: PR3 adds a background initial build plus a `notify`
//! watcher without changing this API shape.

mod extract;
pub mod schema;
mod store;

use std::path::{Path, PathBuf};

pub use extract::{EdgeFact, ExtractedFacts, Extractor, SymbolFact};
pub use store::{GraphError, GraphStore, ReferenceMatch, SymbolMatch};

/// Directories never walked when building the graph — generated code,
/// dependencies, Forge's own runtime state, and any other tool's scratch
/// area. Mirrors the same category of skip-list `forge-tui`'s file watcher
/// already hardcodes for `.git`/`.forge`
/// (`crates/forge-tui/src/app/watch.rs`), extended with build-output/
/// dependency directories a symbol graph has no reason to enter.
///
/// This is a hardcoded approximation, not real `.gitignore` compliance —
/// found the hard way: an earlier dogfood run against forge's own repo
/// indexed four copies of every symbol because `.claude/worktrees/` (this
/// harness's own gitignored scratch worktrees, full checkouts of the repo)
/// wasn't excluded, and every duplicate read as a genuine repo-wide
/// ambiguity. A future revision should walk via the same gitignore-aware
/// crate `fff_search` already uses instead of maintaining this list by
/// hand as new cases turn up.
const SKIP_DIRS: &[&str] = &[
    ".git",
    ".forge",
    ".claude",
    "target",
    "node_modules",
    ".venv",
    "vendor",
    "dist",
    "build",
    "__pycache__",
];

pub struct GraphHandle {
    store: GraphStore,
}

impl GraphHandle {
    /// Opens (or creates) the persisted store for `workspace` and runs a
    /// full synchronous build. `db_path` is passed in rather than resolved
    /// here: `forge-graph` doesn't depend on `forge-storage`'s
    /// `RuntimeDataKind` directly, to keep this crate storage-location-
    /// agnostic — the caller (`forge-session`) resolves the path via
    /// `RuntimeDataKind::Graph`.
    ///
    /// PR1 rebuilds every call rather than checking "is this already
    /// built" — there's no live watcher yet to keep a skipped build from
    /// going stale, and re-running `build_full` on an already-populated
    /// store just duplicates rows (no correctness issue, since every query
    /// still returns real facts, just wasted work). PR3's `files`
    /// bookkeeping (content-hash comparison) replaces this with a real
    /// "build once, then watch" lifecycle — see `store.rs::clear_file`,
    /// already in place for it to build on.
    pub async fn open(workspace: &Path, db_path: &Path) -> Result<Self, GraphError> {
        let store = GraphStore::open(db_path).await?;
        let handle = Self { store };
        handle.build_full(workspace).await?;
        Ok(handle)
    }

    /// In-memory store, immediately built from `workspace` — for tests.
    pub async fn open_in_memory(workspace: &Path) -> Result<Self, GraphError> {
        let store = GraphStore::open_in_memory().await?;
        let handle = Self { store };
        handle.build_full(workspace).await?;
        Ok(handle)
    }

    pub fn store(&self) -> &GraphStore {
        &self.store
    }

    pub async fn find_definition(&self, name: &str) -> Result<Vec<SymbolMatch>, GraphError> {
        self.store.find_definition(name).await
    }

    pub async fn find_references(&self, name: &str) -> Result<Vec<ReferenceMatch>, GraphError> {
        self.store.find_references(name).await
    }

    /// Two-pass full-repo build (see `store.rs`'s module doc): every
    /// recognized file's symbols first, grouped by language, then every
    /// file's edges — across all 5 languages together — resolved against
    /// the complete symbol table.
    async fn build_full(&self, workspace: &Path) -> Result<(), GraphError> {
        let mut symbols_by_lang: std::collections::HashMap<&'static str, Vec<SymbolFact>> =
            std::collections::HashMap::new();
        let mut all_edges = Vec::new();
        for path in walk_files(workspace) {
            let Some((lang, facts)) = extract_file(&path, workspace) else {
                continue;
            };
            symbols_by_lang
                .entry(lang)
                .or_default()
                .extend(facts.symbols);
            all_edges.extend(facts.edges);
        }
        for (lang, symbols) in &symbols_by_lang {
            self.store.insert_symbols(lang, symbols).await?;
        }
        self.store.resolve_and_insert_edges(&all_edges).await?;
        Ok(())
    }
}

/// Reads and extracts one file, if its extension maps to a supported
/// language and it decodes as UTF-8 text. `None` for anything else —
/// unrecognized extensions, binaries, and unreadable files are silently
/// skipped, not errors: most of a real repo isn't source code in one of
/// these 5 languages.
fn extract_file(path: &Path, workspace: &Path) -> Option<(&'static str, ExtractedFacts)> {
    let ext = path.extension()?.to_str()?;
    let source = std::fs::read_to_string(path).ok()?;
    let rel = path.strip_prefix(workspace).unwrap_or(path);
    match ext {
        "rs" => Some(("rust", extract::rust::RustExtractor.extract(&source, rel))),
        "py" | "pyi" => Some((
            "python",
            extract::python::PythonExtractor.extract(&source, rel),
        )),
        "go" => Some(("go", extract::go::GoExtractor.extract(&source, rel))),
        "ts" | "tsx" => Some((
            "typescript",
            extract::typescript::TypeScriptExtractor.extract(&source, rel),
        )),
        "js" | "jsx" | "mjs" | "cjs" => Some((
            "javascript",
            extract::javascript::JavaScriptExtractor.extract(&source, rel),
        )),
        _ => None,
    }
}

fn walk_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk_dir(root, &mut out);
    out
}

fn walk_dir(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if SKIP_DIRS.contains(&name.as_ref()) {
                continue;
            }
            walk_dir(&path, out);
        } else {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[tokio::test]
    async fn builds_from_a_real_small_workspace_and_answers_queries() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn helper() {}\n\npub fn entry() {\n    helper();\n}\n",
        )
        .unwrap();

        let handle = GraphHandle::open_in_memory(dir.path()).await.unwrap();

        let def = handle.find_definition("helper").await.unwrap();
        assert_eq!(def.len(), 1);
        assert_eq!(def[0].file, "src/lib.rs");

        let refs = handle.find_references("helper").await.unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].from_line, 4);
    }

    /// A polyglot repo — the real shape a build has to handle once it's not
    /// Rust-only: each language's file is parsed by its own grammar, all
    /// five land in one shared graph, and a name unique to one language
    /// resolves without leaking into or colliding with the others.
    #[tokio::test]
    async fn builds_across_all_five_languages_in_one_repo() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("main.rs"), "pub fn rust_only() {}\n").unwrap();
        fs::write(dir.path().join("main.py"), "def python_only():\n    pass\n").unwrap();
        fs::write(
            dir.path().join("main.go"),
            "package main\n\nfunc GoOnly() {}\n",
        )
        .unwrap();
        fs::write(dir.path().join("main.ts"), "function typescriptOnly() {}\n").unwrap();
        fs::write(dir.path().join("main.js"), "function javascriptOnly() {}\n").unwrap();

        let handle = GraphHandle::open_in_memory(dir.path()).await.unwrap();

        for name in [
            "rust_only",
            "python_only",
            "GoOnly",
            "typescriptOnly",
            "javascriptOnly",
        ] {
            let def = handle.find_definition(name).await.unwrap();
            assert_eq!(
                def.len(),
                1,
                "{name} should resolve to exactly one definition"
            );
        }
    }

    #[tokio::test]
    async fn skips_target_and_git_directories() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("target/debug")).unwrap();
        fs::write(
            dir.path().join("target/debug/generated.rs"),
            "pub fn should_not_be_indexed() {}\n",
        )
        .unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/lib.rs"), "pub fn real() {}\n").unwrap();

        let handle = GraphHandle::open_in_memory(dir.path()).await.unwrap();
        assert!(handle
            .find_definition("should_not_be_indexed")
            .await
            .unwrap()
            .is_empty());
        assert_eq!(handle.find_definition("real").await.unwrap().len(), 1);
    }

    /// Regression for a real dogfood-run bug: a gitignored scratch worktree
    /// checked out inside the workspace (as `.claude/worktrees/<id>/` is in
    /// this very repo) is a full copy of the source tree. Walking into it
    /// turned every real symbol into a false multi-candidate ambiguity.
    #[tokio::test]
    async fn skips_gitignored_scratch_worktrees() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/lib.rs"), "pub fn real_one() {}\n").unwrap();
        fs::create_dir_all(dir.path().join(".claude/worktrees/agent-x/src")).unwrap();
        fs::write(
            dir.path().join(".claude/worktrees/agent-x/src/lib.rs"),
            "pub fn real_one() {}\n",
        )
        .unwrap();

        let handle = GraphHandle::open_in_memory(dir.path()).await.unwrap();
        assert_eq!(
            handle.find_definition("real_one").await.unwrap().len(),
            1,
            "a scratch worktree copy must not be indexed as a second real definition"
        );
    }
}
